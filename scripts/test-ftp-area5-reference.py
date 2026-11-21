"""Run opt-in FTP tests against disposable, loopback-only reference peers."""

import argparse
import base64
import contextlib
import hashlib
import http.server
import json
import os
from pathlib import Path
import select
import socket
import socketserver
import ssl
import subprocess
import tempfile
import threading
import zlib
from urllib.parse import unquote, urlsplit
from socks4_fixture import read_connect, reply_code
from windows_tls_fixture import reference_client_certificate


class Server(socketserver.ThreadingTCPServer):
    daemon_threads = True

    def __init__(self, handler, *, ipv6=False, tls=None, implicit=False, otp=False, skey=False,
                 gateway=False, gateway_steps=(), checksum=None, require_protection=False):
        self.address_family = socket.AF_INET6 if ipv6 else socket.AF_INET
        super().__init__(("::1" if ipv6 else "127.0.0.1", 0), handler)
        self.tls = tls
        self.implicit = implicit
        self.otp = otp
        self.skey = skey
        self.gateway = gateway
        self.gateway_steps = gateway_steps
        self.checksum = checksum
        self.require_protection = require_protection
        self.files = {"note with spaces.txt": b"initial text"} if require_protection else {"fixture.txt": b"reference\n"}
        self.otp_count = 1 if skey else 99
        self.lock = threading.Lock()
        self.passive_ports = set()
        self.resume_faults = {}
        self.resume_attempts = []
        self.fxp_target = None
        self.fxp_commands = []
        self.fxp_source_data = []
        self.fxp_passive_target = False

    def url(self, host="127.0.0.1"):
        scheme = "ftps" if self.implicit else "ftp"
        return f"{scheme}://{host}:{self.server_address[1]}/"


class Ftp(socketserver.BaseRequestHandler):
    def handle(self):
        self.control = self.request
        self.control.settimeout(15)
        self.data = None
        self.active_data = None
        self.protected = False
        self.binary = True
        self.compressed = False
        self.offset = 0
        self.authenticated = False
        self.gateway_step = 0
        self.rename_from = None
        self.deny_fxp = False
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
            self.close_data_listener()
            self.control.close()

    def close_data_listener(self):
        if self.data:
            with self.server.lock:
                self.server.passive_ports.discard(self.data.getsockname()[1])
            self.data.close()
            self.data = None

    def reply(self, value):
        self.control.sendall((value + "\r\n").encode("cp1252"))

    def command(self, verb, argument):
        if self.server.gateway_steps and not self.authenticated:
            expected_verb, expected_arg, reply = self.server.gateway_steps[self.gateway_step]
            if (verb, argument) != (expected_verb, expected_arg):
                self.reply("530 Unexpected gateway step")
                return
            self.gateway_step += 1
            self.authenticated = self.gateway_step == len(self.server.gateway_steps)
            self.reply(reply)
            return
        if verb == "AUTH" and self.server.tls and not self.server.implicit:
            self.reply("234 TLS ready")
            self.lines.close()
            self.control = self.server.tls.wrap_socket(self.control, server_side=True)
            self.lines = self.control.makefile("rb")
            return
        if verb == "USER":
            if self.server.require_protection and not isinstance(self.control, ssl.SSLSocket):
                self.reply("534 TLS control connection required")
                return
            expected = "arco@files.example.invalid:2121" if self.server.gateway else "arco"
            if argument != expected:
                self.reply("530 Username refused")
                return
            self.user = True
            self.sequence = self.server.otp_count
            if self.server.skey:
                if self.sequence < 0:
                    self.reply("530 S/KEY sequence exhausted")
                else:
                    self.reply(f"331 Password [s/key {self.sequence} test]")
            else:
                self.reply(f"331 otp-md5 {self.sequence} TeSt ext" if self.server.otp else "331 Password")
            return
        if verb == "PASS":
            expected = "test-only"
            if self.server.skey:
                expected = {1: "CARD SAD MINI RYE COL KIN", 0: "ROME MUG FRED SCAN LIVE LACE"}.get(self.sequence)
            elif self.server.otp:
                value = b"testThis is a test."
                for _ in range(self.sequence + 1):
                    digest = hashlib.md5(value).digest()
                    value = bytes(left ^ right for left, right in zip(digest[:8], digest[8:]))
                expected = value.hex()
            with self.server.lock:
                if not getattr(self, "user", False) or argument != expected or (
                    (self.server.otp or self.server.skey) and self.server.otp_count != self.sequence
                ):
                    self.reply("530 Password refused")
                    return
                if self.server.otp or self.server.skey:
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
            self.deny_fxp = argument == "fxp-denied.bin"
            payload = self.server.files.get(argument)
            self.reply(f"213 {len(payload)}" if payload is not None else "550 Missing")
        elif verb == "REST":
            self.offset = int(argument)
            self.reply("350 Restart position set")
        elif verb == "RNFR":
            with self.server.lock:
                self.rename_from = argument if argument in self.server.files else None
            self.reply("350 Source accepted" if self.rename_from is not None else "550 Missing source")
        elif verb == "RNTO":
            with self.server.lock:
                old, self.rename_from = self.rename_from, None
                exists = old in self.server.files
                if exists:
                    self.server.files[argument] = self.server.files.pop(old)
            self.reply("250 Renamed" if exists else "550 Missing source")
        elif verb == "DELE":
            with self.server.lock:
                removed = self.server.files.pop(argument, None)
            self.reply("250 Deleted" if removed is not None else "550 Missing file")
        elif self.server.fxp_target and verb in {"PORT", "EPRT"}:
            self.setup_fxp(verb, argument)
        elif verb in {"PASV", "EPSV"}:
            if verb == "PASV" and self.server.address_family == socket.AF_INET6:
                self.reply("522 Use EPSV for IPv6")
                return
            self.close_data_listener()
            self.data = socket.socket(self.server.address_family, socket.SOCK_STREAM)
            self.data.bind((self.server.server_address[0], 0))
            self.data.listen(1)
            self.data.settimeout(10)
            port = self.data.getsockname()[1]
            with self.server.lock:
                self.server.passive_ports.add(port)
            if verb == "EPSV":
                delimiter = "!" if self.server.fxp_passive_target else "|"
                self.reply(f"229 Entering Extended Passive Mode ({delimiter * 3}{port}{delimiter})")
            else:
                address = "192,0,2,1" if self.server.fxp_passive_target else "127,0,0,1"
                self.reply(f"227 Entering Passive Mode ({address},{port // 256},{port % 256})")
        elif verb in {"MLSD", "LIST", "NLST", "RETR", "STOR", "APPE"}:
            self.transfer(verb, argument)
        else:
            self.reply("502 Unsupported command")

    def setup_fxp(self, verb, argument):
        target = self.server.fxp_target
        try:
            if verb == "PORT" and target.address_family == socket.AF_INET:
                a, b, c, d, hi, lo = map(int, argument.split(","))
                address = f"{a}.{b}.{c}.{d}"
                port = hi * 256 + lo
            elif verb == "EPRT" and target.address_family == socket.AF_INET6:
                empty, family, address, port, tail = argument.split("|")
                if empty or family != "2" or tail:
                    raise ValueError("wrong EPRT format")
                port = int(port)
            else:
                raise ValueError("wrong command for address family")
            with target.lock:
                if address != target.server_address[0] or port not in target.passive_ports:
                    raise ValueError("unexpected data endpoint")
        except ValueError:
            self.reply("501 Unsafe or unsupported data endpoint")
            return
        with self.server.lock:
            self.server.fxp_commands.append((verb, address, self.deny_fxp))
        if self.deny_fxp:
            self.reply("522 Direct server transfer refused")
        else:
            self.active_data = (address, port)
            self.close_data_listener()
            self.reply("200 Direct server transfer accepted")

    def transfer(self, verb, argument):
        if self.server.require_protection and not self.protected:
            self.reply("534 Protected data connection required")
            return
        entries = dict(self.server.files)
        if self.server.fxp_target and verb == "RETR" and not self.active_data:
            self.reply("425 Direct data channel required")
            return
        if not (self.data or self.active_data) or verb == "RETR" and argument not in entries:
            self.reply("550 No data channel or missing file")
            return
        if argument == "compressed-text.txt" and (not self.compressed or self.binary):
            self.reply("504 This fixture requires MODE Z and TYPE A")
            return
        fault = None
        if argument.startswith("resume-") and verb in {"RETR", "APPE"}:
            offset = len(entries.get(argument, b"")) if verb == "APPE" else self.offset
            with self.server.lock:
                attempt = (verb, argument, offset)
                self.server.resume_attempts.append(attempt)
                fault = self.server.resume_faults.pop(attempt, None)
        self.reply("150 Data channel ready")
        if self.active_data:
            connection = socket.create_connection(self.active_data, timeout=10)
            self.active_data = None
            with self.server.lock:
                self.server.fxp_source_data.append((verb, argument))
        else:
            connection, _ = self.data.accept()
            self.close_data_listener()
        if self.protected:
            connection = self.server.tls.wrap_socket(connection, server_side=True)
        with connection:
            if verb in {"STOR", "APPE"}:
                limit = fault[1] if fault and fault[0] != "ack" else 1024 * 1024
                with connection.makefile("rb") as reader:
                    payload = reader.read(limit)
                if self.compressed:
                    payload = zlib.decompress(payload)
                if not self.binary:
                    payload = payload.replace(b"\r\n", b"\n")
                with self.server.lock:
                    previous = self.server.files.get(argument, b"") if verb == "APPE" else b""
                    if fault and fault[0] == "truncate":
                        self.server.files[argument] = previous[:16_384]
                    else:
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
                    if self.server.fxp_target:
                        if argument == "fxp-short.bin":
                            payload = payload[:len(payload) // 2]
                        elif argument == "fxp-collision.bin":
                            with self.server.fxp_target.lock:
                                self.server.fxp_target.files["raced.bin"] = b"arrived during transfer"
                    if not self.binary:
                        payload = payload.replace(b"\n", b"\r\n")
                if self.compressed:
                    payload = zlib.compress(payload)
                if fault:
                    payload = payload[:fault[1]]
                    if fault[0] == "change":
                        with self.server.lock:
                            data = bytearray(self.server.files[argument])
                            data[60_000] ^= 0xff
                            self.server.files[argument] = bytes(data)
                connection.sendall(payload)
            if self.protected and (not fault or fault[0] == "ack"):
                connection.unwrap().close()
        if fault:
            self.control.shutdown(socket.SHUT_RDWR)
            return
        self.reply("226 Transfer complete")


class Socks4(socketserver.BaseRequestHandler):
    def handle(self):
        self.request.settimeout(8)
        address, host, port, user = read_connect(self.request)
        target = self.server.target
        with target.lock:
            allowed = port == target.server_address[1] or port in target.passive_ports
        allowed = allowed and user == b"proxy-fixture" and (
            (address, host) == ("127.0.0.1", None)
            or (address, host) == ("0.0.0.1", "ftp.invalid")
        )
        if not allowed:
            self.request.sendall(reply_code(False))
            return
        with socket.create_connection(("127.0.0.1", port), timeout=8) as forward:
            self.request.sendall(reply_code(True))
            channels = {self.request: forward, forward: self.request}
            while channels:
                ready, _, _ = select.select(list(channels), [], [], 8)
                if not ready:
                    return
                for direction in ready:
                    peer = channels[direction]
                    data = direction.recv(32768)
                    if data:
                        peer.sendall(data)
                    else:
                        channels.pop(direction)
                        with contextlib.suppress(OSError):
                            peer.shutdown(socket.SHUT_WR)


class FtpHttpForward(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def do_GET(self):
        url = urlsplit(self.path)
        if (url.scheme, url.netloc) != ("ftp", "files.example.invalid:2121"):
            self.send_error(400, "Expected an absolute FTP URL without credentials")
            return
        for header, credentials, status in (
            ("Proxy-Authorization", b"proxy-fixture:proxy-only", 407),
            ("Authorization", b"arco:test-only", 401),
        ):
            if status == 407 and not getattr(self.server, "proxy_auth", True):
                continue
            if self.headers.get(header) != "Basic " + base64.b64encode(credentials).decode():
                self.send_response(status)
                self.send_header("Proxy-Authenticate" if status == 407 else "WWW-Authenticate",
                                 'Basic realm="FTP fixture"')
                self.send_header("Content-Length", "0")
                self.end_headers()
                return
        entries = {
            "/home/": b"""<html><head><base href="ftp://ignored.invalid/"></head>
<body><a href="navigation">navigation</a><pre>
<a href="../">parent</a>
drwxr-xr-x 2 arco staff 0 Nov 15 10:10 <a href="nested/">nested</a>
-rw-r--r-- 1 arco staff 8 Nov 15 10:11 <a href="report%20%26%20notes.bin">report &amp; notes.bin</a>
<a href="ftp://files.example.invalid:2121/home/unknown.txt;type=i">unknown.txt</a>
<a href="ftp://ignored.invalid/home/wrong.txt">unrelated host</a>
<a href="%2fetc">escaped slash</a><a href="../different/sibling">other directory</a>
lrwxr-xr-x 1 arco staff 3 Nov 15 10:12 <a href="link">link -&gt; abc</a>
<script> '<a href="script.txt">embedded script</a>' </script>
</pre></body></html>""",
            "/home/nested/": b'<html><pre><a href="data.bin">data.bin</a></pre></html>',
            "/home/nested/data.bin": b"nested\x00",
            "/home/report & notes.bin": b"arco\x00bin",
            "/home/unknown.txt": b"unknown\r\nsize\n",
        }
        path = unquote(url.path)
        if path == "/home/redirect.bin":
            self.send_response(302)
            self.send_header("Location", "ftp://127.0.0.1:1/")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        if path not in entries:
            self.send_error(404)
            return
        body = entries[path]
        self.send_response(200)
        kind = "text/html; charset=UTF-8" if path.endswith("/") else "application/octet-stream"
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def verify_forward_script(executable, profile, url):
    root = profile.parent / "downloads"
    root.mkdir()
    profile.write_text(json.dumps({"ftp_profiles": {"Forward fixture": {
        "address": "ftp://files.example.invalid:2121/home/",
        "user": "arco",
        "security": "plain",
        "proxy": url,
        "http_forward": True,
    }}}), encoding="utf-8")
    script = (
        f'lcd "{root}"\nopen "Forward fixture"\nls\n'
        'get "report & notes.bin"\nget -recursive "nested"\n'
    )
    result = subprocess.run([
        str(executable.resolve()), "--profile", str(profile), "--script", "-",
        "--password-env", "ARCO_TEST_FTP_FORWARD_PASSWORD",
    ], input=script.encode(), capture_output=True, timeout=30,
        env=dict(os.environ, ARCO_TEST_FTP_FORWARD_PASSWORD="test-only"))
    assert result.returncode == 0, (result.stdout, result.stderr)
    assert (root / "report & notes.bin").read_bytes() == b"arco\x00bin"
    assert (root / "nested" / "data.bin").read_bytes() == b"nested\x00"
    print("FTP HTTP-Forward: CLI listing, single-file and recursive downloads passed.")


def verify_resume_script(executable, profile, server, content):
    root = profile.parent / "resume"
    root.mkdir()
    server.files = {"resume-cli-get.bin": content, "resume-cli-put.bin": content[:32_768]}
    server.resume_faults = {
        ("RETR", "resume-cli-get.bin", 0): ("cut", 98_304),
        ("APPE", "resume-cli-put.bin", 32_768): ("cut", 65_536),
    }
    profile.write_text(json.dumps({"ftp_profiles": {"Resume fixture": {
        "address": server.url(), "user": "arco", "security": "plain",
    }}}), encoding="utf-8")
    script = (
        f'lcd "{root}"\nopen "Resume fixture"\n'
        'get -resume "resume-cli-get.bin" "output.bin"\n'
        'put -resume "output.bin" "resume-cli-put.bin"\n'
    )
    result = subprocess.run([
        str(executable.resolve()), "--profile", str(profile), "--script", "-",
        "--password-env", "ARCO_TEST_FTP_RESUME_PASSWORD",
    ], input=script.encode(), capture_output=True, timeout=30,
        env=dict(os.environ, ARCO_TEST_FTP_RESUME_PASSWORD="test-only"))
    assert result.returncode == 0, (result.stdout, result.stderr)
    assert not server.resume_faults, server.resume_faults
    assert (root / "output.bin").read_bytes() == server.files["resume-cli-put.bin"] == content
    print("FTP resume: CLI get and put recovered after interrupted transfers.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--openssl", required=True, type=Path)
    parser.add_argument("--harness", type=Path, help="Already built arco-windows lib test harness")
    parser.add_argument("--app-harness", type=Path, help="Already built arco-commander test harness for FTPS editing")
    parser.add_argument("--arco-net", type=Path, help="Run the CLI workflow with this arco-net binary")
    parser.add_argument("--client-cert", action="store_true",
                        help="Temporarily import a generated client certificate into CurrentUser\\My")
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
        resume_servers = []
        resume_data = bytes(index % 251 for index in range(270_000))
        for mode in ("PLAIN", "EXPLICIT", "IMPLICIT"):
            protected = mode != "PLAIN"
            server = start(tls=tls if protected else None, implicit=mode == "IMPLICIT",
                           require_protection=protected)
            server.files = {
                "resume-download.bin": resume_data,
                "resume-changed.bin": resume_data,
                "resume-exhausted.bin": resume_data,
                "resume-upload.bin": resume_data[:32_768],
                "resume-ack.bin": resume_data[:32_768],
                "resume-truncated.bin": resume_data[:32_768],
            }
            server.resume_faults = {
                ("RETR", "resume-download.bin", 0): ("cut", 98_304),
                ("RETR", "resume-changed.bin", 0): ("change", 98_304),
                ("RETR", "resume-exhausted.bin", 0): ("cut", 98_304),
                ("RETR", "resume-exhausted.bin", 98_304): ("cut", 32_768),
                ("RETR", "resume-exhausted.bin", 131_072): ("cut", 32_768),
                ("APPE", "resume-upload.bin", 32_768): ("cut", 65_536),
                ("APPE", "resume-ack.bin", 32_768): ("ack", 0),
                ("APPE", "resume-truncated.bin", 32_768): ("truncate", 65_536),
            }
            resume_servers.append(server)
            env[f"ARCO_TEST_FTP_RESUME_{mode}"] = server.url("localhost" if protected else "127.0.0.1")
        if args.client_cert:
            thumbprint, client_pem = stack.enter_context(
                reference_client_certificate(args.openssl.resolve(), Path(folder), flags)
            )
            mutual_tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            mutual_tls.load_cert_chain(cert, key)
            mutual_tls.load_verify_locations(cafile=client_pem)
            mutual_tls.minimum_version = ssl.TLSVersion.TLSv1_2
            mutual_tls.verify_mode = ssl.CERT_REQUIRED
            env["ARCO_TEST_FTPS_CLIENT_CERT"] = thumbprint
            env["ARCO_TEST_FTPS_MUTUAL_EXPLICIT"] = start(tls=mutual_tls).url("localhost")
            env["ARCO_TEST_FTPS_MUTUAL_IMPLICIT"] = start(
                tls=mutual_tls, implicit=True,
            ).url("localhost")
        env["ARCO_TEST_FTP_OTP"] = start(otp=True).url()
        env["ARCO_TEST_FTP_SKEY"] = start(skey=True).url()
        env["ARCO_TEST_FTP_GATEWAY"] = start(gateway=True).url()
        ftp_socks = start()
        socks_proxy = stack.enter_context(Server(Socks4))
        socks_proxy.target = ftp_socks
        thread = threading.Thread(target=socks_proxy.serve_forever, daemon=True)
        thread.start()
        stack.callback(socks_proxy.shutdown)
        env["ARCO_TEST_FTP_SOCKS4_TARGET"] = ftp_socks.url()
        env["ARCO_TEST_FTP_SOCKS4_PORT"] = str(socks_proxy.server_address[1])
        for scheme in ("http", "https"):
            http_proxy = stack.enter_context(http.server.ThreadingHTTPServer(
                ("127.0.0.1", 0), FtpHttpForward,
            ))
            if scheme == "https":
                http_proxy.socket = tls.wrap_socket(http_proxy.socket, server_side=True)
            thread = threading.Thread(target=http_proxy.serve_forever, daemon=True)
            thread.start()
            stack.callback(http_proxy.shutdown)
            env[f"ARCO_TEST_{scheme.upper()}_FORWARD"] = f"{scheme}://localhost:{http_proxy.server_port}"
        host = "files.example.invalid:2121"
        login = [("USER", "arco", "331 Server password"), ("PASS", "test-only", "230 Server login")]
        proxy = [("USER", "gateway", "331 Gateway password"), ("PASS", "proxy-only", "230 Gateway login")]
        user_host = [("USER", f"arco@{host}", "331 Server password"), login[1]]
        account = ("ACCT", "proxy-only", "230 Gateway account accepted")
        gateway_steps = {
            "SITE": [("SITE", host, "220 Server selected"), *login],
            "SITE_LOGIN": [*proxy, ("SITE", host, "200 Server selected"), *login],
            "AUTH_USER_HOST": [*proxy, *user_host],
            "OPEN": [("OPEN", host, "220 Server selected"), *login],
            "OPEN_LOGIN": [*proxy, ("OPEN", host, "200 Server selected"), *login],
            "AUTH_USER": [*proxy, *login],
            "PROXY_HOST": [("USER", f"gateway@{host}", "331 Gateway password"), proxy[1], *login],
            "USER_HOST_PROXY": [
                ("USER", f"arco@{host} gateway", "331 Server password"),
                ("PASS", "test-only", "332 Gateway account"), account,
            ],
            "USER_PROXY_HOST": [
                ("USER", f"arco@gateway@{host}", "331 Combined password"),
                ("PASS", "test-only@proxy-only", "332 Gateway account"), account,
            ],
        }
        for mode, sequence in gateway_steps.items():
            env[f"ARCO_TEST_FTP_GATEWAY_{mode}"] = start(gateway_steps=sequence).url()
        for checksum in ("XSHA1", "XMD5", "MD5", "XCRC"):
            env[f"ARCO_TEST_FTP_{checksum}"] = start(checksum=checksum).url()
        fxp_content = bytes(index % 251 for index in range(131_079))
        fxp_pairs = []

        def start_fxp(version):
            ipv6 = version == 6
            source, target = start(ipv6=ipv6), start(ipv6=ipv6)
            source.fxp_target = target
            target.fxp_passive_target = True
            source.files = dict.fromkeys(("fxp-input.bin", "fxp-denied.bin", "fxp-short.bin", "fxp-collision.bin"), fxp_content)
            target.files = {"occupied.bin": b"keep this file"}
            env[f"ARCO_TEST_FXP_V{version}_SOURCE"] = source.url("[::1]" if ipv6 else "127.0.0.1")
            env[f"ARCO_TEST_FXP_V{version}_TARGET"] = target.url("localhost")
            fxp_pairs.append((version, source, target))

        start_fxp(4)
        try:
            env["ARCO_TEST_FTP_IPV6"] = start(ipv6=True).url("[::1]")
            start_fxp(6)
        except OSError:
            print("IPv6 loopback is unavailable; IPv6 verification skipped.")
        command = [str(args.harness.resolve()), "ftp::reference::"] if args.harness else [
            "cargo", "test", "-p", "arco-windows", "--lib", "--locked", "ftp::reference::", "--",
        ]
        try:
            subprocess.run([*command, "--ignored", "--test-threads=1", "--nocapture"], env=env, check=True)
        except subprocess.CalledProcessError:
            for server in resume_servers:
                print(server.url(), server.resume_attempts, flush=True)
            raise
        for server in resume_servers:
            assert not server.resume_faults, server.resume_faults
            assert ("RETR", "resume-download.bin", 98_304) in server.resume_attempts
            assert ("RETR", "resume-changed.bin", 98_304) not in server.resume_attempts
            appends = {}
            for verb, name, offset in server.resume_attempts:
                if verb == "APPE":
                    appends.setdefault(name, []).append(offset)
            assert appends == {
                "resume-upload.bin": [32_768, 98_304],
                "resume-ack.bin": [32_768],
                "resume-truncated.bin": [32_768],
            }, appends
            assert server.files["resume-upload.bin"] == server.files["resume-ack.bin"] == resume_data
            assert server.files["resume-truncated.bin"] == resume_data[:16_384]
        for version, source, target in fxp_pairs:
            verb, address = ("PORT", "127.0.0.1") if version == 4 else ("EPRT", "::1")
            expected = [(verb, address, denied) for denied in (False, True, False, False)]
            files = {"occupied.bin": b"keep this file", "direct.bin": fxp_content, "raced.bin": b"arrived during transfer"}
            downloaded = ["fxp-input.bin", "fxp-short.bin", "fxp-collision.bin"]
            if version == 6:
                expected.append((verb, address, False))
                files["explicit.bin"] = fxp_content
                downloaded.append("fxp-input.bin")
            assert source.fxp_commands == expected, source.fxp_commands
            assert source.fxp_source_data == [("RETR", name) for name in downloaded]
            assert {name: data for name, data in target.files.items() if not name.startswith(".arco-fxp-")} == files
            staged = [data for name, data in target.files.items() if name.startswith(".arco-fxp-")]
            assert sorted(staged, key=len) == [fxp_content[:len(fxp_content) // 2], fxp_content]
            print(f"IPv{version} FXP: direct data connection, refusal, short transfer, and late collision verified.")
        if args.app_harness:
            explicit = start(tls=tls, require_protection=True)
            implicit = start(tls=tls, implicit=True, require_protection=True)
            editor_env = dict(env, ARCO_TEST_FTPS_EDIT_EXPLICIT=explicit.url("localhost"),
                              ARCO_TEST_FTPS_EDIT_IMPLICIT=implicit.url("localhost"))
            result = subprocess.run([
                str(args.app_harness.resolve()),
                "app::ftp_edit::tests::ftps_drafts_detect_competing_publications",
                "--exact", "--ignored", "--nocapture", "--test-threads=1",
            ], env=editor_env, timeout=90, creationflags=flags, stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT, encoding="utf-8", errors="replace")
            print(result.stdout, end="", flush=True)
            result.check_returncode()
            for server in (explicit, implicit):
                assert server.files == {"note with spaces.txt": b"later update"}, server.files
        if args.arco_net:
            verify_resume_script(args.arco_net, Path(folder) / "resume-profile.json", start(), resume_data)
            cli_proxy = stack.enter_context(http.server.ThreadingHTTPServer(
                ("127.0.0.1", 0), FtpHttpForward,
            ))
            cli_proxy.proxy_auth = False
            thread = threading.Thread(target=cli_proxy.serve_forever, daemon=True)
            thread.start()
            stack.callback(cli_proxy.shutdown)
            verify_forward_script(args.arco_net, Path(folder) / "profile.json",
                                  f"http://127.0.0.1:{cli_proxy.server_port}")


if __name__ == "__main__":
    main()
