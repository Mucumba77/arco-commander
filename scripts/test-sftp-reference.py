"""Run Arco's opt-in SFTP workflow against a disposable Paramiko server."""

import argparse
import base64
import hashlib
import os
from pathlib import Path
import socket
import socketserver
import subprocess
import tempfile
import threading
import time

import paramiko


APP_TEST = "app::ftp_sync::tests::sftp_search_and_bidirectional_sync"


class Login(paramiko.ServerInterface):
    def __init__(self, server):
        self.server = server

    def get_allowed_auths(self, username):
        return "password,publickey"

    def check_auth_password(self, username, password):
        self.server.auth_attempts.append(username)
        if (username, password) == ("arco", "fixture-only"):
            return paramiko.AUTH_SUCCESSFUL
        return paramiko.AUTH_FAILED

    def check_auth_publickey(self, username, key):
        self.server.auth_attempts.append(username)
        if username == "arco" and key == self.server.client_key:
            return paramiko.AUTH_SUCCESSFUL
        return paramiko.AUTH_FAILED

    def check_channel_request(self, kind, channel_id):
        if kind == "session":
            return paramiko.OPEN_SUCCEEDED
        return paramiko.OPEN_FAILED_ADMINISTRATIVELY_PROHIBITED


def sftp_operation(operation):
    def invoke(self, *args):
        try:
            return operation(self, *args)
        except OSError as error:
            return paramiko.SFTPServer.convert_errno(error.errno)
    return invoke


class Files(paramiko.SFTPServerInterface):
    def __init__(self, server, *, root):
        super().__init__(server)
        self.root = root

    def local(self, remote):
        base = self.root.joinpath(*remote.split("/")).resolve()
        if not base.is_relative_to(self.root):
            raise PermissionError(13, "Outside test fixtures")
        return base

    @sftp_operation
    def list_folder(self, remote):
        return [
            paramiko.SFTPAttributes.from_stat(entry.stat(), entry.name)
            for entry in self.local(remote).iterdir()
        ]

    @sftp_operation
    def stat(self, remote):
        return paramiko.SFTPAttributes.from_stat(self.local(remote).stat())

    lstat = stat

    @sftp_operation
    def open(self, remote, flags, attrs):
        path = self.local(remote)
        fd = os.open(path, flags | os.O_BINARY, attrs.st_mode or 0o600)
        writable = bool(flags & (os.O_WRONLY | os.O_RDWR))
        file = os.fdopen(fd, "r+b" if writable else "rb", buffering=0)
        handle = paramiko.SFTPHandle(flags)
        handle.readfile = file
        handle.writefile = file
        return handle

    @sftp_operation
    def mkdir(self, remote, attrs):
        self.local(remote).mkdir()
        return paramiko.SFTP_OK

    @sftp_operation
    def rmdir(self, remote):
        self.local(remote).rmdir()
        return paramiko.SFTP_OK

    @sftp_operation
    def remove(self, remote):
        self.local(remote).unlink()
        return paramiko.SFTP_OK

    @sftp_operation
    def rename(self, before, after):
        self.local(before).rename(self.local(after))
        return paramiko.SFTP_OK

    @sftp_operation
    def chattr(self, remote, attrs):
        paramiko.SFTPServer.set_file_attr(str(self.local(remote)), attrs)
        return paramiko.SFTP_OK


class Server(socketserver.ThreadingTCPServer):
    daemon_threads = True


class Peer(socketserver.BaseRequestHandler):
    def handle(self):
        with paramiko.Transport(
            self.request,
            disabled_algorithms={
                "keys": ["ssh-rsa"],
                "pubkeys": ["ssh-rsa"],
                "kex": ["diffie-hellman-group-exchange-sha256", "diffie-hellman-group-exchange-sha1"],
            },
        ) as connection:
            connection.add_server_key(self.server.host_key)
            connection.set_subsystem_handler("sftp", paramiko.SFTPServer, Files, root=self.server.root)
            connection.start_server(server=Login(self.server))
            channel = connection.accept(20)
            if channel:
                while connection.is_active() and not channel.closed:
                    time.sleep(0.1)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--harness", type=Path, help="Already built arco-windows lib test harness")
    parser.add_argument("--app-harness", type=Path, help="Already built arco-commander unit test harness")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="arco-native-sftp-") as fixture:
        work = Path(fixture).resolve()
        root = work / "files"
        root.mkdir()
        with Server(("127.0.0.1", 0), Peer) as server:
            server.root = root
            server.host_key = paramiko.RSAKey.generate(2048)
            server.client_key = paramiko.RSAKey.generate(2048)
            server.auth_attempts = []
            key = work / "id_rsa"
            server.client_key.write_private_key_file(str(key))
            fingerprint = base64.b64encode(hashlib.sha256(server.host_key.asbytes()).digest())
            environment = dict(
                os.environ,
                ARCO_SFTP_TEST_URL=f"sftp://127.0.0.1:{server.server_address[1]}/",
                ARCO_SFTP_TEST_FINGERPRINT="SHA256:" + fingerprint.decode("ascii").rstrip("="),
                ARCO_SFTP_TEST_KEY=str(key),
            )
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            command = [str(args.harness.resolve())] if args.harness else [
                "cargo", "test", "--locked", "-p", "arco-windows", "--lib",
                "reference_sftp_workflow", "--",
            ]
            if args.harness:
                command.append("reference_sftp_workflow")
            try:
                subprocess.run([*command, "--ignored", "--nocapture"], env=environment, check=True)
                app_command = [str(args.app_harness.resolve()), APP_TEST] if args.app_harness else [
                    "cargo", "test", "--locked", "-p", "arco-app", "--bin", "arco-commander",
                    APP_TEST, "--",
                ]
                subprocess.run(
                    [*app_command, "--exact", "--ignored", "--nocapture"],
                    env=environment, check=True, creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
                )
                uploaded = root / "round trip" / "outgoing" / "ä # export.txt"
                assert uploaded.read_bytes() == b"from local"
                print("Application reference passed: recursive search and bidirectional SFTP sync")
                assert server.auth_attempts and set(server.auth_attempts) == {"arco"}, server.auth_attempts
            finally:
                server.shutdown()
                thread.join()


if __name__ == "__main__":
    main()
