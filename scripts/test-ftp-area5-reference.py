"""Run opt-in FTP tests against disposable, loopback-only reference peers."""

import argparse
import contextlib
import hashlib
import os
from pathlib import Path
import socket
import socketserver
import ssl
import subprocess
import tempfile
import threading
import zlib


class Server(socketserver.ThreadingTCPServer):
    daemon_threads = True

    def __init__(self, handler, *, ipv6=False, tls=None, implicit=False, otp=False, gateway=False, checksum=None):
        self.address_family = socket.AF_INET6 if ipv6 else socket.AF_INET
        super().__init__(("::1" if ipv6 else "127.0.0.1", 0), handler)
        self.tls = tls
        self.implicit = implicit
        self.otp = otp
        self.gateway = gateway
        self.checksum = checksum
        self.files = {"fixture.txt": b"reference\n"}
        self.otp_count = 99
        self.lock = threading.Lock()

    def url(self, host="127.0.0.1"):
        scheme = "ftps" if self.implicit else "ftp"
        return f"{scheme}://{host}:{self.server_address[1]}/"


class Ftp(socketserver.BaseRequestHandler):
    def handle(self):
        self.control = self.request
        self.control.settimeout(15)
        self.data = None
        self.protected = False
        self.binary = True
        self.compressed = False
        self.offset = 0
        self.authenticated = False
        try:
            if self.server.implicit:
                self.control = self.server.tls.wrap_socket(self.control, server_side=True)
            self.lines = self.control.makefile("rb")
            self.reply("220 Arco loopback reference")
            while line := self.lines.readline(4096):
                verb, _, argument = line.decode("cp1252").rstrip("\r\n").partition(" ")
                if verb == "QUIT":
                    self.reply("221 Bye")
                    return
                self.command(verb, argument)
        except (ConnectionError, TimeoutError, ssl.SSLError):
            # Expected for rejected TLS trust, cancelled transfers and negative tests.
            return
        finally:
            if self.data:
                self.data.close()
            self.control.close()

    def reply(self, value):
        self.control.sendall((value + "\r\n").encode("cp1252"))

    def command(self, verb, argument):
        if verb == "AUTH" and self.server.tls and not self.server.implicit:
            self.reply("234 TLS ready")
            self.lines.close()
            self.control = self.server.tls.wrap_socket(self.control, server_side=True)
            self.lines = self.control.makefile("rb")
            return
        if verb == "USER":
            expected = "arco@files.example.invalid:2121" if self.server.gateway else "arco"
            if argument != expected:
                self.reply("530 Username refused")
                return
            self.user = True
            self.sequence = self.server.otp_count
            self.reply(f"331 otp-md5 {self.sequence} TeSt ext" if self.server.otp else "331 Password")
            return
        if verb == "PASS":
            expected = "test-only"
            if self.server.otp:
                value = b"testThis is a test."
                for _ in range(self.sequence + 1):
                    digest = hashlib.md5(value).digest()
                    value = bytes(left ^ right for left, right in zip(digest[:8], digest[8:]))
                expected = value.hex()
            with self.server.lock:
                if not getattr(self, "user", False) or argument != expected or (
                    self.server.otp and self.server.otp_count != self.sequence
                ):
                    self.reply("530 Password refused")
                    return
                if self.server.otp:
                    self.server.otp_count -= 1
            self.authenticated = True
            self.reply("230 Logged in")
            return
        if not self.authenticated:
            self.reply("530 Login first")
            return
        if verb in {"OPTS", "PBSZ", "NOOP"}:
            self.reply("200 OK")
        elif verb == "PROT":
            self.protected = argument == "P"
            self.reply("200 Protection selected")
        elif verb == "FEAT":
            extension = f" {self.server.checksum}\r\n" if self.server.checksum else ""
            self.reply(f"211-Features\r\n MLSD\r\n SIZE\r\n MODE Z\r\n{extension}211 End")
        elif verb == self.server.checksum and argument in self.server.files:
            payload = self.server.files[argument]
            if verb == "XSHA1":
                digest = hashlib.sha1(payload).hexdigest()
            elif verb == "XCRC":
                digest = f"{zlib.crc32(payload):08x}"
            else:
                digest = hashlib.md5(payload).hexdigest()
            self.reply(f"213 {digest} {argument}")
        elif verb == "SYST":
            self.reply("215 UNIX Type: L8")
        elif verb == "PWD":
            self.reply('257 "/" is the current directory')
        elif verb == "CWD":
            self.reply("250 Directory changed" if argument == "/" else "550 Directory missing")
        elif verb == "TYPE":
            self.binary = argument == "I"
            self.reply("200 Type selected")
        elif verb == "MODE":
            self.compressed = argument == "Z"
            self.reply("200 Mode selected")
        elif verb == "SIZE":
            payload = self.server.files.get(argument)
            self.reply(f"213 {len(payload)}" if payload is not None else "550 Missing")
        elif verb == "REST":
            self.offset = int(argument)
            self.reply("350 Restart position set")
        elif verb in {"PASV", "EPSV"}:
            if self.data:
                self.data.close()
            self.data = socket.socket(self.server.address_family, socket.SOCK_STREAM)
            self.data.bind((self.server.server_address[0], 0))
            self.data.listen(1)
            self.data.settimeout(10)
            port = self.data.getsockname()[1]
            if verb == "EPSV":
                self.reply(f"229 Entering Extended Passive Mode (|||{port}|)")
            else:
                self.reply(f"227 Entering Passive Mode (127,0,0,1,{port // 256},{port % 256})")
        elif verb in {"MLSD", "LIST", "NLST", "RETR", "STOR", "APPE"}:
            self.transfer(verb, argument)
        else:
            self.reply("502 Unsupported command")

    def transfer(self, verb, argument):
        entries = dict(self.server.files)
        if not self.data or verb == "RETR" and argument not in entries:
            self.reply("550 No data channel or missing file")
            return
        if argument == "compressed-text.txt" and (not self.compressed or self.binary):
            self.reply("504 This fixture requires MODE Z and TYPE A")
            return
        self.reply("150 Data channel ready")
        connection, _ = self.data.accept()
        self.data.close()
        self.data = None
        if self.protected:
            connection = self.server.tls.wrap_socket(connection, server_side=True)
        with connection:
            if verb in {"STOR", "APPE"}:
                with connection.makefile("rb") as reader:
                    payload = reader.read(1024 * 1024)
                if self.compressed:
                    payload = zlib.decompress(payload)
                if not self.binary:
                    payload = payload.replace(b"\r\n", b"\n")
                with self.server.lock:
                    previous = self.server.files.get(argument, b"") if verb == "APPE" else b""
                    self.server.files[argument] = previous + payload
            else:
                if verb == "MLSD":
                    payload = "".join(
                        f"type=file;size={len(data)};modify=20261117010000; {name}\r\n"
                        for name, data in entries.items()
                    ).encode("cp1252")
                elif verb == "LIST":
                    if argument == "-a":
                        entries[".hidden"] = b"h"
                    payload = "".join(f"f|{len(data)}|{name}\r\n" for name, data in entries.items()).encode("cp1252")
                elif verb == "NLST":
                    payload = "\r\n".join(entries).encode("cp1252")
                else:
                    payload = entries[argument][self.offset:]
                    self.offset = 0
                    if not self.binary:
                        payload = payload.replace(b"\n", b"\r\n")
                if self.compressed:
                    payload = zlib.compress(payload)
                connection.sendall(payload)
            if self.protected:
                connection.unwrap().close()
        self.reply("226 Transfer complete")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--openssl", required=True, type=Path)
    parser.add_argument("--harness", type=Path, help="Already built arco-windows lib test harness")
    args = parser.parse_args()
    flags = getattr(subprocess, "CREATE_NO_WINDOW", 0)
    with tempfile.TemporaryDirectory(prefix="arco-ftp-area5-") as folder, contextlib.ExitStack() as stack:
        cert, key = Path(folder) / "cert.pem", Path(folder) / "key.pem"
        subprocess.run([
            str(args.openssl.resolve()), "req", "-x509", "-newkey", "rsa:2048", "-nodes",
            "-days", "2", "-subj", "/CN=localhost",
            "-addext", "subjectAltName=DNS:localhost",
            "-keyout", str(key), "-out", str(cert),
        ], check=True, capture_output=True, creationflags=flags)
        tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        tls.load_cert_chain(cert, key)
        tls.minimum_version = ssl.TLSVersion.TLSv1_2

        def start(**kwargs):
            server = stack.enter_context(Server(Ftp, **kwargs))
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            stack.callback(server.shutdown)
            return server

        env = dict(os.environ, ARCO_TEST_FTPS_CA=str(cert))
        env["ARCO_TEST_FTP_AREA5"] = start().url()
        env["ARCO_TEST_FTPS_EXPLICIT"] = start(tls=tls).url("localhost")
        env["ARCO_TEST_FTPS_IMPLICIT"] = start(tls=tls, implicit=True).url("localhost")
        env["ARCO_TEST_FTP_OTP"] = start(otp=True).url()
        env["ARCO_TEST_FTP_GATEWAY"] = start(gateway=True).url()
        for checksum in ("XSHA1", "XMD5", "MD5", "XCRC"):
            env[f"ARCO_TEST_FTP_{checksum}"] = start(checksum=checksum).url()
        try:
            env["ARCO_TEST_FTP_IPV6"] = start(ipv6=True).url("[::1]")
        except OSError:
            print("IPv6 loopback is unavailable; IPv6 verification skipped.")
        command = [str(args.harness.resolve()), "ftp::reference::"] if args.harness else [
            "cargo", "test", "-p", "arco-windows", "--lib", "--locked", "ftp::reference::", "--",
        ]
        subprocess.run([*command, "--ignored", "--test-threads=1", "--nocapture"], env=env, check=True)


if __name__ == "__main__":
    main()
