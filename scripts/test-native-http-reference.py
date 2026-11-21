"""Exercise Arco's native WebDAV and S3 sessions with disposable local servers."""

import argparse
import base64
from contextlib import contextmanager
from contextlib import ExitStack
from datetime import datetime, timedelta, timezone
import hashlib
import hmac
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import io
import json
import os
from pathlib import Path
import ssl
import subprocess
import tempfile
import threading
from urllib.parse import parse_qs, unquote, urlencode, urlsplit
from xml.etree import ElementTree

import boto3
from botocore.auth import S3SigV4Auth, SigV4Auth
from botocore.awsrequest import AWSRequest
from botocore.credentials import Credentials
from moto.server import DomainDispatcherApplication, create_backend_app
from werkzeug.serving import WSGIRequestHandler, make_server
from werkzeug.wsgi import LimitedStream
from wsgidav.wsgidav_app import WsgiDAVApp
from windows_tls_fixture import reference_client_certificate


DIRECT_TOKEN = "arco-direct-" + "T" * 3500
DIRECT_ROLE = "arn:aws:iam::123456789012:role/arco-direct-reference"


class Quiet(WSGIRequestHandler):
    def log_request(self, code="-", size="-"):
        pass


@contextmanager
def serve(app, tls=None):
    server = make_server("127.0.0.1", 0, app, threaded=True, request_handler=Quiet, ssl_context=tls)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"https://localhost:{server.server_port}" if tls else f"http://127.0.0.1:{server.server_port}"
    finally:
        server.shutdown()
        thread.join()
        server.server_close()


def bounded_dav_input(app):
    def request(environ, start_response):
        if environ.get("CONTENT_LENGTH"):
            environ["wsgi.input"] = LimitedStream(environ["wsgi.input"], int(environ["CONTENT_LENGTH"]))
        return app(environ, start_response)
    return request


def replace_xml_body(app, environ, start_response, edit):
    def capture(status, headers, exc_info=None):
        nonlocal reply
        reply = status, headers, exc_info

    reply = None
    response = app(environ, capture)
    try:
        body = b"".join(response)
    finally:
        if hasattr(response, "close"):
            response.close()
    status, headers, exc_info = reply
    if status.startswith(("200 ", "207 ")):
        xml = ElementTree.fromstring(body)
        edit(xml)
        body = ElementTree.tostring(xml)
    headers = [(key, value) for key, value in headers if key.lower() != "content-length"]
    start_response(status, headers + [("Content-Length", str(len(body)))], exc_info)
    return [body]


class WebDavInteropProbe:
    folder = "interop"
    name = "Prüfdatei 100% [alt].aspx"

    def __init__(self):
        self.href = "raw"
        self.downloads = []

    def wrap(self, app):
        def request(environ, start_response):
            path = unquote(urlsplit(environ["RAW_URI"]).path)
            base = f"/demo/{self.folder}/"
            method = environ["REQUEST_METHOD"]
            if path == base and method == "PROPFIND":
                def change_href(xml):
                    for node in xml.findall(".//{DAV:}href"):
                        if unquote(urlsplit(node.text).path) == base + self.name:
                            if self.href == "raw":
                                node.text = f"http://{environ['HTTP_HOST']}{base}{self.name}"
                            elif self.href == "other-host":
                                node.text = f"http://localhost.invalid{base}{self.name}"
                            elif self.href == "outside-folder":
                                node.text = f"/demo/{self.name}"
                            elif self.href == "escaped-slash":
                                node.text = f"{base}falsch%2F{self.name}"
                return replace_xml_body(app, environ, start_response, change_href)
            if path == base + self.name and method == "GET":
                self.downloads.append(environ.get("HTTP_TRANSLATE"))
                if self.downloads[-1] != "f":
                    start_response("403 Forbidden", [("Content-Length", "0")])
                    return []
            return app(environ, start_response)
        return request


def signed_s3(app, accepted_keys=None):
    def request(environ, start_response):
        authorization = environ.get("HTTP_AUTHORIZATION", "")
        scheme, _, parameters = authorization.partition(" ")
        parts = dict(value.strip().split("=", 1) for value in parameters.split(",") if "=" in value)
        key, _, _ = parts.get("Credential", "").partition("/")
        expected_token = {
            "ARCOACCESS": None, "ARCOAWSACCESS": "temporary-test-token",
            "ARCOEXPLICIT": DIRECT_TOKEN,
        }
        if (
            scheme == "AWS4-HMAC-SHA256"
            and key in expected_token
            and environ.get("HTTP_X_AMZ_SECURITY_TOKEN") == expected_token[key]
            and "SignedHeaders" in parts
        ):
            headers = {}
            for name in parts["SignedHeaders"].split(";"):
                field = name.replace("-", "_").upper()
                source = field if field in ("CONTENT_LENGTH", "CONTENT_TYPE") else "HTTP_" + field
                if source in environ:
                    headers[name] = environ[source]
            url = f"http://{environ['HTTP_HOST']}{environ['RAW_URI']}"
            wire = AWSRequest(method=environ["REQUEST_METHOD"], url=url, headers=headers)
            wire.context["timestamp"] = headers["x-amz-date"]
            if expected_token[key] and headers.get("x-amz-security-token") != expected_token[key]:
                start_response("403 Forbidden", [("Content-Length", "0")])
                return []
            signer = S3SigV4Auth(Credentials(key, "fixture-only"), "s3", "eu-central-1")
            expected = signer.signature(signer.string_to_sign(wire, signer.canonical_request(wire)), wire)
            if hmac.compare_digest(expected, parts.get("Signature", "")):
                if accepted_keys is not None:
                    accepted_keys.append(key)
                return app(environ, start_response)
        start_response("403 Forbidden", [("Content-Length", "0")])
        return []
    return request


class MultipartProbe:
    bucket = "arco-resume-fixture"

    def __init__(self):
        self.failed = set()
        self.events = []

    def wrap(self, app):
        def request(environ, start_response):
            prefix = f"/{self.bucket}/"
            path = environ["PATH_INFO"]
            if not path.startswith(prefix):
                return app(environ, start_response)
            name = path.removeprefix(prefix)
            method = environ["REQUEST_METHOD"]
            query = parse_qs(environ["QUERY_STRING"], keep_blank_values=True)
            if "uploads" in query:
                self.events.append((name, "start", ""))
            if "uploadId" in query:
                upload_id = query["uploadId"][0]
                if method == "PUT":
                    part = query["partNumber"][0]
                    self.events.append((name, "part", part))
                    if part == "3" and name not in self.failed:
                        self.failed.add(name)
                        environ["wsgi.input"].read(int(environ["CONTENT_LENGTH"]))
                        start_response("503 Service Unavailable", [("Content-Length", "0")])
                        return []
                elif method == "GET":
                    marker = query.get("part-number-marker", ["0"])[0]
                    self.events.append((name, "list", marker))
                    # Vary the service's page size after the fixture checked the signature.
                    environ = dict(environ)
                    query["max-parts"] = ["1"]
                    environ["QUERY_STRING"] = urlencode(query, doseq=True)
                elif method == "DELETE":
                    self.events.append((name, "abort", upload_id))
            return app(environ, start_response)
        return request


class LargeVersionCopyProbe:
    bucket = "arco-large-version-fixture"
    name = "Büro & Plus+.bin"
    logical_part = 1024 ** 3
    real_part = 5 * 1024 ** 2
    tail = b"last part of the old revision"
    display_size = 5 * logical_part + len(tail)

    def __init__(self):
        self.version = None
        self.source_tag = None
        self.fail_part = False
        self.conflict_at_completion = False
        self.parts = []
        self.aborted = []
        self.conditions = []

    def wrap(self, app):
        def request(environ, start_response):
            bucket_path = f"/{self.bucket}"
            path = unquote(urlsplit(environ["RAW_URI"]).path)
            if self.version is None or not path.startswith(bucket_path):
                return app(environ, start_response)
            query = parse_qs(environ["QUERY_STRING"], keep_blank_values=True)
            method = environ["REQUEST_METHOD"]
            if path.rstrip("/") == bucket_path and "versions" in query:
                def resize_listing(xml):
                    for item in xml.findall("{*}Version"):
                        if item.findtext("{*}VersionId") == self.version:
                            item.find("{*}Size").text = str(self.display_size)
                return replace_xml_body(app, environ, start_response, resize_listing)
            if path != f"{bucket_path}/{self.name}":
                return app(environ, start_response)
            if method == "HEAD" and query.get("versionId") == [self.version]:
                assert environ.get("HTTP_IF_MATCH") == self.source_tag
                def resize_head(status, headers, exc_info=None):
                    if status.startswith("200 "):
                        headers = [
                            (key, str(self.display_size) if key.lower() == "content-length" else value)
                            for key, value in headers
                        ]
                    return start_response(status, headers, exc_info)
                return app(environ, resize_head)
            if method == "POST" and "uploads" in query:
                assert parse_qs(environ["HTTP_X_AMZ_TAGGING"], keep_blank_values=True) == {
                    "label": ["old + revision"], "empty": [""],
                }
                assert environ["HTTP_X_AMZ_META_REVISION"] == "old version"
            if method == "PUT" and "partNumber" in query:
                part = int(query["partNumber"][0])
                start = (part - 1) * self.logical_part
                end = min(start + self.logical_part, self.display_size) - 1
                source = urlsplit(environ["HTTP_X_AMZ_COPY_SOURCE"])
                assert unquote(source.path) == path
                assert parse_qs(source.query) == {"versionId": [self.version]}
                assert environ["HTTP_X_AMZ_COPY_SOURCE_IF_MATCH"] == self.source_tag
                assert environ["HTTP_X_AMZ_COPY_SOURCE_RANGE"] == f"bytes={start}-{end}"
                assert not environ.get("CONTENT_LENGTH") or environ["CONTENT_LENGTH"] == "0"
                self.parts.append(part)
                if self.fail_part and part == 2:
                    body = b"<Error><Code>FixtureCopyFailed</Code></Error>"
                    start_response("200 OK", [("Content-Length", str(len(body)))])
                    return [body]
                # Scale the signed range down only after checking it. Moto performs the real copy.
                environ = dict(environ)
                start = (part - 1) * self.real_part
                length = self.real_part if part <= 5 else len(self.tail)
                environ["HTTP_X_AMZ_COPY_SOURCE_RANGE"] = f"bytes={start}-{start + length - 1}"
            if "uploadId" in query:
                if method == "POST":
                    self.conditions.append((environ.get("HTTP_IF_MATCH"), environ.get("HTTP_IF_NONE_MATCH")))
                    if self.conflict_at_completion:
                        environ["wsgi.input"].read(int(environ["CONTENT_LENGTH"]))
                        start_response("412 Precondition Failed", [("Content-Length", "0")])
                        return []
                if method == "DELETE":
                    self.aborted.append(query["uploadId"][0])
            return app(environ, start_response)
        return request


class MfaDeleteProbe:
    bucket = "arco-mfa-delete-fixture"
    serial = "arn:aws:iam::123456789012:mfa/arco-fixture"
    code = "012345"

    def __init__(self):
        self.enabled = False
        self.sent = []

    def wrap(self, app):
        def request(environ, start_response):
            prefix = f"/{self.bucket}"
            path = environ["PATH_INFO"].rstrip("/")
            if path != prefix and not path.startswith(prefix + "/"):
                return app(environ, start_response)
            query = parse_qs(environ["QUERY_STRING"], keep_blank_values=True)
            method = environ["REQUEST_METHOD"]
            if path == prefix and "versioning" in query:
                if method == "GET":
                    def show_state(xml):
                        assert xml.find("{*}MfaDelete") is None
                        ElementTree.SubElement(xml, "MfaDelete").text = "Enabled" if self.enabled else "Disabled"
                    return replace_xml_body(app, environ, start_response, show_state)
                assert method == "PUT"
                payload = environ["wsgi.input"].read(int(environ["CONTENT_LENGTH"]))
                assert environ["HTTP_CONTENT_MD5"] == base64.b64encode(hashlib.md5(payload).digest()).decode()
                versioning = ElementTree.fromstring(payload)
                status = versioning.findtext("{*}Status")
                mfa_change = versioning.findtext("{*}MfaDelete")
                assert status in ("Enabled", "Suspended") and mfa_change in (None, "Enabled", "Disabled")
                environ = dict(environ, **{"wsgi.input": io.BytesIO(payload)})
                if self.enabled or mfa_change is not None:
                    if not self.authorize(environ, method, mfa_change or status):
                        start_response("403 Forbidden", [("Content-Length", "0")])
                        return []
                def update_state(status_line, headers, exc_info=None):
                    if status_line.startswith("20") and mfa_change is not None:
                        self.enabled = mfa_change == "Enabled"
                    return start_response(status_line, headers, exc_info)
                return app(environ, update_state)
            if method == "DELETE" and query.get("versionId") and self.enabled:
                if not self.authorize(environ, method, query["versionId"][0]):
                    start_response("403 Forbidden", [("Content-Length", "0")])
                    return []
            return app(environ, start_response)
        return request

    def authorize(self, environ, method, subject):
        header = environ.get("HTTP_X_AMZ_MFA")
        if header is None:
            self.sent.append((method, subject, False))
            return False
        assert environ["wsgi.url_scheme"] == "https"
        assert "x-amz-mfa" in environ["HTTP_AUTHORIZATION"].split("SignedHeaders=", 1)[1].split(",", 1)[0].split(";")
        authorized = hmac.compare_digest(header, f"{self.serial} {self.code}")
        self.sent.append((method, subject, authorized))
        return authorized


class VirtualHostProbe:
    bucket = "arco-virtual-fixture"

    def __init__(self):
        self.requests = []
        self.refused = 0

    def wrap(self, app):
        def request(environ, start_response):
            host, _, port = environ["HTTP_HOST"].partition(":")
            if host != f"{self.bucket}.localhost":
                return app(environ, start_response)
            if (
                environ.get("HTTP_X_AMZ_REQUEST_PAYER") != "requester"
                or "x-amz-request-payer" not in environ["HTTP_AUTHORIZATION"]
            ):
                self.refused += 1
                start_response("403 Forbidden", [("Content-Length", "0")])
                return []

            self.requests.append((environ["REQUEST_METHOD"], environ["RAW_URI"]))
            # Moto receives path-style requests. The wrapper already verified SigV4
            # over the original virtual-host URL and the payment header.
            state = dict(environ)
            prefix = f"/{self.bucket}"
            path, question, query = state["RAW_URI"].partition("?")
            raw = prefix + (path if path != "/" else "") + question + query
            state["HTTP_HOST"] = f"127.0.0.1:{port}"
            state["PATH_INFO"] = prefix + (state["PATH_INFO"] if path != "/" else "")
            state["RAW_URI"] = state["REQUEST_URI"] = raw
            return app(state, start_response)

        return request


def aws_credentials_server(environ, start_response):
    expiration = (datetime.now(timezone.utc) + timedelta(minutes=5)).strftime("%Y-%m-%dT%H:%M:%SZ")
    body = json.dumps({
        "AccessKeyId": "ARCOAWSACCESS",
        "SecretAccessKey": "fixture-only",
        "Token": "temporary-test-token",
        "Expiration": expiration,
    }).encode("utf-8")
    start_response("200 OK", [("Content-Type", "application/json"), ("Content-Length", str(len(body)))])
    return [body]


def aws_role_server(environ, start_response, accepted_roles=None):
    body = environ["wsgi.input"].read(int(environ.get("CONTENT_LENGTH") or 0))
    authorization = environ.get("HTTP_AUTHORIZATION", "")
    direct = "Credential=ARCOEXPLICIT/" in authorization
    if (
        environ["REQUEST_METHOD"] != "POST"
        or b"Action=AssumeRole" not in body
        or not (direct or "Credential=ARCOROLESOURCE/" in authorization)
    ):
        start_response("403 Forbidden", [("Content-Length", "0")])
        return []
    if direct:
        fields = parse_qs(body.decode("ascii"))
        _, _, parameters = authorization.partition(" ")
        parts = dict(value.strip().split("=", 1) for value in parameters.split(",") if "=" in value)
        headers = {}
        for name in parts["SignedHeaders"].split(";"):
            field = name.replace("-", "_").upper()
            key = field if field in ("CONTENT_LENGTH", "CONTENT_TYPE") else "HTTP_" + field
            headers[name] = environ[key]
        wire = AWSRequest(method="POST", url=f"http://{environ['HTTP_HOST']}{environ['RAW_URI']}",
                          data=body, headers=headers)
        wire.context["timestamp"] = headers["x-amz-date"]
        signer = SigV4Auth(Credentials("ARCOEXPLICIT", "fixture-only", DIRECT_TOKEN), "sts", "eu-central-1")
        signature = signer.signature(signer.string_to_sign(wire, signer.canonical_request(wire)), wire)
        if (
            fields.get("RoleArn") != [DIRECT_ROLE]
            or fields.get("RoleSessionName") != ["arco-commander"]
            or headers.get("x-amz-security-token") != DIRECT_TOKEN
            or not hmac.compare_digest(signature, parts.get("Signature", ""))
        ):
            start_response("403 Forbidden", [("Content-Length", "0")])
            return []
    if accepted_roles is not None:
        accepted_roles.append("ARCOEXPLICIT" if direct else "ARCOROLESOURCE")
    expiration = (datetime.now(timezone.utc) + timedelta(minutes=5)).strftime("%Y-%m-%dT%H:%M:%SZ")
    response = f"""<AssumeRoleResponse xmlns="https://sts.amazonaws.com/doc/2011-06-15/">
    <AssumeRoleResult>
      <AssumedRoleUser>
        <AssumedRoleId>ARCOREFTST:arco-role-check</AssumedRoleId>
        <Arn>arn:aws:sts::123456789012:assumed-role/arco-reference/arco-role-check</Arn>
      </AssumedRoleUser>
      <Credentials>
        <AccessKeyId>ARCOAWSACCESS</AccessKeyId>
        <SecretAccessKey>fixture-only</SecretAccessKey>
        <SessionToken>temporary-test-token</SessionToken>
        <Expiration>{expiration}</Expiration>
      </Credentials>
    </AssumeRoleResult>
    <ResponseMetadata><RequestId>arco-reference</RequestId></ResponseMetadata>
    </AssumeRoleResponse>""".encode("utf-8")
    start_response("200 OK", [("Content-Type", "text/xml"), ("Content-Length", str(len(response)))])
    return [response]


def encrypted_transfers(executable, work, dav_url, s3_url):
    import boto3

    client = boto3.client("s3", endpoint_url=s3_url, aws_access_key_id="ARCOACCESS",
                          aws_secret_access_key="fixture-only", region_name="eu-central-1")
    bucket = "arco-encrypted-reference"
    client.create_bucket(Bucket=bucket, CreateBucketConfiguration={"LocationConstraint": "eu-central-1"})
    profiles = {
        "Dav": {"address": dav_url, "security": "web-dav", "user": "arco"},
        "S3": {"address": f"{s3_url}/{bucket}/", "security": "s3",
               "user": "ARCOACCESS", "s3_region": "eu-central-1"},
    }
    profile = work / "encrypted-profiles.json"
    profile.write_text(json.dumps({"ftp_profiles": profiles}), encoding="utf-8")
    source = work / "Prüfdatei.txt"
    source.write_bytes(b"Confidential synthetic reference data.\x00" * 4000)
    env = dict(os.environ, ARCO_REFERENCE_LOGIN="fixture-only",
               ARCO_REFERENCE_PASSPHRASE="synthetic-reference-passphrase")

    def run(provider, command):
        process = subprocess.run(
            [str(executable.resolve()), "--profile", str(profile), "--script", "-",
             "--password-env", "ARCO_REFERENCE_LOGIN"],
            cwd=work, env=env, input=f"open {provider}\n{command}\n",
            capture_output=True, encoding="utf-8", timeout=90,
            creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
        )
        assert process.returncode == 0, process.stderr
        return json.loads(process.stdout.splitlines()[-1])["event"]

    for provider in profiles:
        destination = work / f"decoded-{provider}"
        destination.mkdir()
        event = run(provider, f'put-encrypted "{source.name}" ARCO_REFERENCE_PASSPHRASE')
        name = event["uploaded"]
        assert event["encrypted"] and name.endswith(".arco.age")
        assert source.name not in name
        event = run(provider, f"get-encrypted {name} ARCO_REFERENCE_PASSPHRASE {destination.name}")
        assert event["encrypted"] and (destination / source.name).read_bytes() == source.read_bytes()
        print(f"{provider} client-side encrypted round trip passed")


def run_dav_script(executable, work, profile, *commands, authenticated_password=False, expected=0):
    settings = work / "dav-profile.json"
    settings.write_text(json.dumps({"ftp_profiles": {"Dav": {"security": "web-dav", **profile}}}), encoding="utf-8")
    command = [str(executable.resolve()), "--profile", str(settings), "--script", "-"]
    environment = dict(os.environ)
    if authenticated_password:
        environment["ARCO_DAV_REFERENCE_PASSWORD"] = "fixture-only"
        command.extend(["--password-env", "ARCO_DAV_REFERENCE_PASSWORD"])
    result = subprocess.run(
        command, env=environment, cwd=work, input="\n".join(("open Dav", *commands, "")),
        capture_output=True, encoding="utf-8", timeout=35,
        creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
    )
    assert result.returncode == expected, (result.stdout, result.stderr)
    return result.stdout if expected == 0 else result.stderr


def reference_dav_interop(executable, root, work, base_url, probe):
    fixtures = root / probe.folder
    fixtures.mkdir()
    contents = b"source as stored on the WebDAV server\r\n"
    (fixtures / probe.name).write_bytes(contents)
    profiles = {"address": f"{base_url}/demo/{probe.folder}/", "user": "arco"}
    error = run_dav_script(executable, work, profiles, "ls", authenticated_password=True, expected=1)
    assert "nicht kodierte Sonderzeichen" in error, error
    profiles["webdav_liberal_escaping"] = True
    run_dav_script(executable, work, profiles, f'get "{probe.name}" source.aspx', authenticated_password=True)
    assert (work / "source.aspx").read_bytes() == contents
    assert probe.downloads and set(probe.downloads) == {"f"}, probe.downloads
    for shape, reason in [
        ("other-host", "host differs"),
        ("outside-folder", "außerhalb des Ordners"),
        ("escaped-slash", "expected a single FTP name"),
    ]:
        probe.href = shape
        error = run_dav_script(executable, work, profiles, "ls", authenticated_password=True, expected=1)
        assert reason in error, (shape, error)
    probe.href = "raw"
    print("WebDAV recovered raw Unicode, percent and brackets in a resource URL, used IIS Translate: f, and rejected out-of-scope addresses")


def reference_windows_negotiate(executable, work, external_url=None):
    steps = []
    body = b'<D:multistatus xmlns:D="DAV:"></D:multistatus>'
    fixture_user = "arco"
    authorization = "Basic " + base64.b64encode(b"arco:fixture-only").decode("ascii")

    class WindowsDav(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"
        offer_negotiate = True

        def log_message(self, *_):
            pass

        def do_PROPFIND(self):
            self.rfile.read(int(self.headers.get("Content-Length", 0)))
            received = self.headers.get("Authorization", "")
            steps.append(received.partition(" ")[0])
            content = body if hmac.compare_digest(received, authorization) else b""
            self.send_response(207 if content else 401)
            if not content:
                if self.offer_negotiate:
                    self.send_header("WWW-Authenticate", "Negotiate")
                self.send_header("WWW-Authenticate", 'Basic realm="WebDAV reference"')
            self.send_header("Content-Type", "application/xml")
            self.send_header("Content-Length", str(len(content)))
            self.end_headers()
            self.wfile.write(content)

    with ThreadingHTTPServer(("127.0.0.1", 0), WindowsDav) as server:
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            address = f"http://localhost:{server.server_port}/dav/"
            profile = {"address": address, "user": fixture_user}
            protected = dict(profile, webdav_windows_auth=True)
            error = run_dav_script(executable, work, protected, "ls", authenticated_password=True, expected=1)
            assert "HTTP 401" in error and steps and set(steps) <= {"", "Negotiate"}, (steps, error)
            # With a Basic-only challenge, the ordinary mode may use a configured password.
            steps.clear()
            server.RequestHandlerClass.offer_negotiate = False
            run_dav_script(executable, work, profile, "ls", authenticated_password=True)
            assert "Basic" in steps, steps
        finally:
            server.shutdown()
            thread.join()
    print("WebDAV Windows authentication did not fall back to Basic when challenged with both schemes")
    if external_url:
        run_dav_script(executable, work, {"address": external_url, "webdav_windows_auth": True}, "ls")
        print("WebDAV connected successfully to the supplied Windows Negotiate reference server")


def reference_dav_client_certificate(executable, root, work, address, ca, thumbprint):
    source = b"WebDAV client certificate fixture"
    (root / "mutual-tls.txt").write_bytes(source)
    profile = {"address": address + "/demo/", "user": "arco", "ca_bundle": str(ca)}
    run_dav_script(executable, work, profile, "ls", authenticated_password=True, expected=1)
    profile["client_certificate"] = thumbprint
    run_dav_script(executable, work, profile, "get mutual-tls.txt received-mtls.txt", authenticated_password=True)
    assert (work / "received-mtls.txt").read_bytes() == source
    print("WebDAVS required a client certificate and downloaded with the selected Windows identity")


def reference_dav_redirects(executable, root, work, dav, tls=None, harness=None):
    fixtures = root / "redirected"
    fixtures.mkdir()
    (fixtures / "alias").mkdir()
    target = fixtures / "actual"
    target.mkdir()
    data = b"the redirected file\x00"
    for name in ("start.bin", "foreign.bin", "loop.bin"):
        (fixtures / name).write_bytes(data)
    (fixtures / "final.bin").write_bytes(data)
    (target / "existing.bin").write_bytes(data)
    (work / "redirect-upload.bin").write_bytes(data)

    trusted_auth = "Basic " + base64.b64encode(b"arco:fixture-only").decode("ascii")
    hits = []
    foreign = []
    routes = {}
    root_path = "/demo/redirected/"

    def before_redirect(environ, start_response):
        path = unquote(urlsplit(environ["RAW_URI"]).path)
        key = (environ["REQUEST_METHOD"], path)
        redirect = routes.get(key)
        if redirect is None:
            return dav(environ, start_response)
        auth = environ.get("HTTP_AUTHORIZATION", "")
        if not hmac.compare_digest(auth, trusted_auth):
            start_response("401 Unauthorized", [("WWW-Authenticate", 'Basic realm="redirect reference"')])
            return []
        hits.append(key)
        code, location = redirect
        content = b"ignored redirect body" * 4096
        start_response(code, [("Location", location), ("Content-Length", str(len(content)))])
        return [content]

    anonymous = WsgiDAVApp({
        "provider_mapping": {"/public": str(fixtures)},
        "simple_dc": {"user_mapping": {"*": True}},
        "verbose": 0,
    })

    def other_host(environ, start_response):
        method, path = environ["REQUEST_METHOD"], environ["RAW_URI"]
        foreign.append((method, path))
        if environ.get("HTTP_AUTHORIZATION") or environ.get("HTTP_COOKIE"):
            start_response("403 Forbidden", [("Content-Length", "0")])
            return []
        if method == "GET" and path == "/served.bin?ticket=one%2Btwo":
            start_response("200 OK", [("Content-Length", str(len(data)))])
            return [data]
        return anonymous(environ, start_response)

    with ExitStack() as stack:
        base = stack.enter_context(serve(bounded_dav_input(before_redirect)))
        other = stack.enter_context(serve(bounded_dav_input(other_host))).replace("127.0.0.1", "localhost")
        profile = {"address": base + "/entry/", "user": "arco"}
        routes.update({
            ("PROPFIND", "/entry/"): ("301 Moved Permanently", root_path),
            ("PROPFIND", root_path + "alias/"): ("307 Temporary Redirect", "../actual/"),
            ("GET", root_path + "start.bin"): ("302 Found", "second.bin"),
            ("GET", root_path + "second.bin"): ("303 See Other", "final.bin?name=check%2Bme"),
            ("GET", root_path + "loop.bin"): ("308 Permanent Redirect", "again.bin"),
            ("GET", root_path + "again.bin"): ("307 Temporary Redirect", "loop.bin"),
            ("GET", root_path + "foreign.bin"): ("302 Found", other + "/served.bin?ticket=one%2Btwo"),
            ("PROPFIND", "/external/"): ("307 Temporary Redirect", other + "/public/"),
            ("MKCOL", root_path + "collection"): ("301 Moved Permanently", root_path + "collection/"),
        })
        def run(*commands, expected=0, options=None):
            return run_dav_script(executable, work, dict(profile, **(options or {})), *commands,
                                  authenticated_password=True, expected=expected)

        run('get start.bin redirected.bin', 'cd alias', 'get existing.bin nested.bin',
            'put redirect-upload.bin uploaded.bin', 'mv uploaded.bin renamed.bin')
        assert (work / "redirected.bin").read_bytes() == data and (work / "nested.bin").read_bytes() == data
        assert (target / "renamed.bin").read_bytes() == data and not (fixtures / "alias" / "renamed.bin").exists()
        run('mkdir collection')
        assert (fixtures / "collection").is_dir() and ("MKCOL", root_path + "collection") in hits
        assert {("PROPFIND", "/entry/"), ("PROPFIND", root_path + "alias/"),
                ("GET", root_path + "start.bin"), ("GET", root_path + "second.bin")} <= set(hits)

        if harness:
            direct = "/demo/restricted/"
            (root / "restricted" / "actual").mkdir(parents=True)
            (root / "restricted" / "actual" / "existing.bin").write_bytes(data)
            routes[("PROPFIND", direct)] = ("403 Forbidden", root_path)
            subprocess.run(
                [str(harness.resolve()), "--ignored", "--exact",
                 "ftp::webdav::tests::reference_deferred_webdav_redirect"],
                check=True, timeout=40,
                env=dict(os.environ, ARCO_TEST_DAV_REDIRECT_DEFERRED=profile["address"],
                         ARCO_TEST_DAV_DIRECT_SUBFOLDER=base + direct),
            )
        failure = run('get loop.bin failed.bin', expected=1)
        assert "Schleife" in failure and not (work / "failed.bin").exists(), failure

        routes[("PROPFIND", root_path + "alias/")] = ("307 Temporary Redirect", "/demo/")
        assert "aus dem geöffneten Stammordner" in run('cd alias', expected=1)
        routes[("MKCOL", root_path + "wrong/")] = ("307 Temporary Redirect", root_path + "moved/")
        routes[("MKCOL", root_path + "wrong")] = routes[("MKCOL", root_path + "wrong/")]
        refused = run('mkdir wrong', expected=1)
        assert "Ressource umgeleitet" in refused and not (fixtures / "moved").exists(), refused

        assert "anderen Host oder Port" in run('get foreign.bin denied.bin', expected=1)
        assert not foreign and not (work / "denied.bin").exists()
        enabled = {"webdav_cross_domain_redirects": True}
        run('get foreign.bin accepted.bin', options=enabled)
        assert (work / "accepted.bin").read_bytes() == data
        assert foreign == [("GET", "/served.bin?ticket=one%2Btwo")]
        run('get final.bin remote-root.bin', options=dict(enabled, address=base + "/external/"))
        assert (work / "remote-root.bin").read_bytes() == data
        assert ("PROPFIND", "/public/") in foreign and ("GET", "/public/final.bin") in foreign

        if tls:
            cert, _ = tls
            secure = stack.enter_context(serve(bounded_dav_input(before_redirect), tls))
            secure_profile = dict(enabled, address=secure + "/entry/", ca_bundle=str(cert))
            before = len(foreign)
            assert "unverschlüsseltes HTTP" in run('get foreign.bin no-downgrade.bin', options=secure_profile, expected=1)
            assert len(foreign) == before
            run('get foreign.bin allowed-downgrade.bin', options=dict(secure_profile, webdav_unencrypted_redirects=True))
            assert (work / "allowed-downgrade.bin").read_bytes() == data
    print("WebDAV followed collection and file redirects, checked write scope, and enforced origin and TLS policy without forwarding credentials")


def s3_script_runner(executable, work, s3_url, bucket, *, profile_options=None, open_option=None, **overrides):
    environment = dict(os.environ, ARCO_REFERENCE_LOGIN="fixture-only", **overrides)
    profile = work / "profiles.json"
    profile.write_text(json.dumps({"ftp_profiles": {"reference": {
        "address": s3_url + "/", "user": "ARCOACCESS", "security": "s3",
        "s3_region": "eu-central-1",
        **(profile_options or {}),
    }}}), encoding="utf-8")
    command = [str(executable.resolve()), "--profile", str(profile), "--script", "-",
               "--password-env", "ARCO_REFERENCE_LOGIN"]

    def run(*steps, expected=0, select_bucket=True):
        if select_bucket:
            steps = (f"cd {bucket}", *steps)
        open_command = "open reference" + (f" {open_option}" if open_option else "")
        result = subprocess.run(
            command, env=environment, cwd=work, input="\n".join((open_command, *steps, "")),
            capture_output=True, encoding="utf-8", timeout=60,
            creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
        )
        if open_option:
            assert DIRECT_TOKEN not in result.stdout and DIRECT_TOKEN not in result.stderr
        assert result.returncode == expected, (result.stdout, result.stderr)
        if expected:
            return result.stderr
        return [json.loads(line)["event"] for line in result.stdout.splitlines()]

    return run


def reference_multipart_resume(executable, root, s3_url, probe):
    work = root / "multipart-resume"
    work.mkdir()
    state = work / "state"
    state.mkdir()
    run = s3_script_runner(executable, work, s3_url, probe.bucket, LOCALAPPDATA=str(state))
    source = work / "source.bin"
    part_size = 16 * 1024 * 1024
    with source.open("wb") as stream:
        stream.write(b"A" * part_size)
        stream.write(b"B" * part_size)
        stream.write(b"Arco multipart resume")

    run(f"mkdir {probe.bucket}", select_bucket=False)
    resume = "put -resume source.bin resumed.bin"
    assert "HTTP 503" in run(resume, expected=1)
    receipts = state / "Arco Commander" / "s3-multipart"
    assert len(list(receipts.glob("*.json"))) == 1
    with source.open("r+b") as stream:
        stream.write(b"X")
    assert "weicht vom begonnenen S3-Upload ab" in run(resume, expected=1)
    with source.open("r+b") as stream:
        stream.write(b"A")
    run(resume, "get resumed.bin result.bin")
    assert (work / "result.bin").read_bytes() == source.read_bytes()
    assert not list(receipts.glob("*.json"))
    activity = [(action, field) for name, action, field in probe.events if name == "resumed.bin"]
    assert activity == [
        ("start", ""), ("part", "1"), ("part", "2"), ("part", "3"),
        ("list", "0"), ("list", "1"), ("part", "3"),
    ], activity

    discard = "put -resume source.bin discarded.bin"
    assert "HTTP 503" in run(discard, expected=1)
    (work / "replacement.txt").write_bytes(b"existing destination")
    run("put replacement.txt discarded.bin")
    assert "bereits vorhanden" in run(discard, expected=1)
    assert run("abort-upload discarded.bin")[-1]["discarded"]
    assert not run("abort-upload discarded.bin")[-1]["discarded"]
    run("get discarded.bin kept.txt")
    assert (work / "kept.txt").read_bytes() == b"existing destination"
    assert not list(receipts.glob("*.json"))
    assert len([item for item in probe.events if item[:2] == ("discarded.bin", "abort")]) == 1
    print("S3 multipart survived a failed part and new processes, paged confirmed parts, and preserved a conflicting object")


def reference_s3_version_commands(executable, root, s3_url):
    work = root / "script-versions"
    work.mkdir()
    bucket = "arco-script-versions"
    run = s3_script_runner(executable, work, s3_url, bucket)
    run(f"mkdir {bucket}", select_bucket=False)
    status = run("s3-versioning", "s3-versioning enable", "s3-versioning")
    assert [item["versioning"] for item in status[-3:]] == ["unversioned", "enabled", "enabled"]

    before = b"first version"
    after = b"a newer version of this file"
    (work / "first.txt").write_bytes(before)
    (work / "second.txt").write_bytes(after)
    listing = run("put first.txt report.txt", "rm report.txt",
                  "put second.txt report.txt", "rm report.txt", "s3-versions report.txt")[-1]["versions"]
    old = next(item["id"] for item in listing if item["size"] == len(before))
    marker = next(item["id"] for item in listing if item["latest"] and item["delete_marker"])
    run(f's3-get-version report.txt "{old}" saved.txt')
    assert (work / "saved.txt").read_bytes() == before
    assert "-permanent" in run(f's3-delete-version report.txt "{old}"', expected=1)
    removed_marker = run(f's3-delete-version -permanent report.txt "{marker}"',
                         "get report.txt visible.txt")
    assert removed_marker[-2]["delete_marker"]
    assert (work / "visible.txt").read_bytes() == after

    restored = run(f's3-restore-version report.txt "{old}"', "get report.txt restored.txt")
    new_id = restored[-2]["version"]
    assert new_id != old and (work / "restored.txt").read_bytes() == before
    remaining = run(f's3-delete-version -permanent report.txt "{old}"',
                    "s3-versions report.txt")[-1]["versions"]
    assert [item["id"] for item in remaining if item["latest"]] == [new_id]
    assert {item["id"] for item in remaining} == ({item["id"] for item in listing} - {old, marker}) | {new_id}
    assert run("s3-versioning suspend", "s3-versioning")[-1]["versioning"] == "suspended"
    print("S3 version commands enabled and suspended versioning, restored a revision, and deleted only selected version IDs")


def reference_large_version_copy(executable, root, s3_url, probe):
    work = root / "large-version-copy"
    work.mkdir()
    run = s3_script_runner(executable, work, s3_url, probe.bucket)
    run(f"mkdir {probe.bucket}", select_bucket=False)
    run("s3-versioning enable")
    sdk = boto3.client(
        "s3", endpoint_url=s3_url, region_name="eu-central-1",
        aws_access_key_id="ARCOACCESS", aws_secret_access_key="fixture-only",
    )
    target = dict(Bucket=probe.bucket, Key=probe.name)
    content = b"".join(bytes([letter]) * probe.real_part for letter in b"abcde") + probe.tail
    source = sdk.put_object(
        **target, Body=content, Metadata={"revision": "old version"},
        ContentType="application/vnd.arco.reference", CacheControl="max-age=60",
        Tagging=urlencode({"label": "old + revision", "empty": ""}),
    )
    replacement = sdk.put_object(**target, Body=b"newest revision")
    probe.version = source["VersionId"]
    probe.source_tag = source["ETag"]
    advertised = sdk.head_object(**target, VersionId=probe.version, IfMatch=probe.source_tag)
    assert (advertised.get("VersionId"), advertised["ContentLength"]) == (probe.version, probe.display_size), advertised
    command = f's3-restore-version "{probe.name}" "{probe.version}"'

    probe.fail_part = True
    failure = run(command, expected=1)
    assert "FixtureCopyFailed" in failure, failure
    assert probe.parts == [1, 2] and len(probe.aborted) == 1 and not probe.conditions
    assert sdk.head_object(**target)["VersionId"] == replacement["VersionId"]
    assert not sdk.list_multipart_uploads(Bucket=probe.bucket).get("Uploads")

    probe.fail_part = False
    probe.parts.clear()
    probe.conflict_at_completion = True
    conflict = run(command, expected=1)
    assert "HTTP 412" in conflict, conflict
    assert probe.parts == list(range(1, 7)) and len(probe.aborted) == 2
    assert probe.conditions == [(replacement["ETag"], None)]
    assert sdk.head_object(**target)["VersionId"] == replacement["VersionId"]
    assert not sdk.list_multipart_uploads(Bucket=probe.bucket).get("Uploads")

    probe.conflict_at_completion = False
    probe.parts.clear()
    probe.conditions.clear()
    restored = run(command)[-1]["version"]
    assert probe.parts == list(range(1, 7))
    assert probe.conditions == [(replacement["ETag"], None)]
    assert restored not in (probe.version, replacement["VersionId"])
    result = sdk.get_object(**target)
    assert result["VersionId"] == restored and result["Body"].read() == content
    assert result["Metadata"] == {"revision": "old version"}
    assert result["ContentType"] == "application/vnd.arco.reference"
    assert result["CacheControl"] == "max-age=60"
    assert {item["Key"]: item["Value"] for item in sdk.get_object_tagging(**target)["TagSet"]} == {
        "label": "old + revision", "empty": "",
    }
    assert sdk.get_object(**target, VersionId=replacement["VersionId"])["Body"].read() == b"newest revision"

    marker = sdk.delete_object(**target)["VersionId"]
    recovered = run(command)[-1]["version"]
    assert recovered not in (restored, marker)
    assert probe.conditions[-1] == (None, "*")
    assert sdk.get_object(**target)["Body"].read() == content
    print("S3 restored a version advertised above 5 GiB with six real byte-range copies, preserved tags and metadata, and aborted on a part failure or completion conflict")


def reference_s3_mfa_delete(executable, root, http_url, https_url, ca, probe):
    work = root / "s3-mfa-delete"
    work.mkdir()
    options = {"ca_bundle": str(ca)}
    run = s3_script_runner(
        executable, work, https_url, probe.bucket, profile_options=options, ARCO_REFERENCE_MFA=probe.code,
    )
    run(f"mkdir {probe.bucket}", select_bucket=False)
    flags = f"-mfa-serial={probe.serial} -mfa-code-env=ARCO_REFERENCE_MFA"
    initial = run("s3-versioning")[-1]
    assert initial["versioning"] == "unversioned" and not initial["mfa_delete"]
    assert "benötigt einen S3-MFA-Code" in run("s3-versioning mfa-enable", expected=1)
    assert run(f"s3-versioning mfa-enable {flags}")[-1]["mfa_delete"]
    assert run("s3-versioning")[-1]["versioning"] == "enabled"

    (work / "input.txt").write_text("test version", encoding="utf-8")
    versions = run("put input.txt versioned.txt", "rm versioned.txt", "s3-versions versioned.txt")[-1]["versions"]
    old = next(item["id"] for item in versions if not item["delete_marker"])
    marker = next(item["id"] for item in versions if item["delete_marker"])
    plain = f's3-delete-version -permanent versioned.txt "{old}"'
    authorized = f's3-delete-version -permanent {flags} versioned.txt "{old}"'
    assert "HTTP 403" in run(plain, expected=1)

    rejected = s3_script_runner(
        executable, work, https_url, probe.bucket, profile_options=options, ARCO_REFERENCE_MFA="987654",
    )
    message = rejected(authorized, expected=1)
    assert "HTTP 403" in message and "987654" not in message
    cleartext = s3_script_runner(executable, work, http_url, probe.bucket, ARCO_REFERENCE_MFA=probe.code)
    seen = len(probe.sent)
    assert "benötigt eine HTTPS-Verbindung" in cleartext(authorized, expected=1)
    assert "benötigt eine HTTPS-Verbindung" in cleartext(f"s3-versioning mfa-disable {flags}", expected=1)
    assert len(probe.sent) == seen

    # A missing variable or a malformed code never reaches the server.
    assert "nicht verfügbar" in run(authorized.replace("ARCO_REFERENCE_MFA", "ARCO_ABSENT_TEST_MFA"), expected=1)
    malformed = s3_script_runner(
        executable, work, https_url, probe.bucket, profile_options=options, ARCO_REFERENCE_MFA="76543",
    )
    message = malformed(authorized, expected=1)
    assert "sechsstelligen" in message and "76543" not in message
    assert len(probe.sent) == seen

    assert run(authorized)[-1]["version"] == old
    removed_marker = run(f's3-delete-version -permanent {flags} versioned.txt "{marker}"')[-1]
    assert removed_marker["delete_marker"] and removed_marker["version"] == marker
    assert "benötigt einen S3-MFA-Code" in run("s3-versioning suspend", expected=1)
    suspended = run(f"s3-versioning suspend {flags}")[-1]
    assert suspended["versioning"] == "suspended" and suspended["mfa_delete"]
    disabled = run(f"s3-versioning mfa-disable {flags}")[-1]
    assert disabled["versioning"] == "suspended" and not disabled["mfa_delete"]
    assert run("s3-versioning enable")[-1]["versioning"] == "enabled"
    assert probe.sent == [
        ("PUT", "Enabled", True), ("DELETE", old, False), ("DELETE", old, False),
        ("DELETE", old, True), ("DELETE", marker, True), ("PUT", "Suspended", True),
        ("PUT", "Disabled", True),
    ], probe.sent
    print("S3 used signed MFA Delete over HTTPS for versioning and targeted deletion, and rejected missing, invalid and cleartext authorization")


def reference_s3_virtual_host(executable, root, s3_url, probe, ca=None):
    work = root / "virtual-host"
    work.mkdir()
    base = s3_url.replace("127.0.0.1", "localhost")
    options = {"s3_virtual_host": True, "s3_requester_pays": True}
    if ca is not None:
        options["ca_bundle"] = str(ca)
    run = s3_script_runner(executable, work, base, probe.bucket, profile_options=options)
    run(f"mkdir {probe.bucket}", select_bucket=False)
    assert ("PUT", "/") in probe.requests
    (work / "local.txt").write_bytes(b"virtual bucket and signed requester header")
    report = "Büro & Plus+.txt"
    run("s3-versioning enable", f'put local.txt "{report}"', f'get "{report}" result.txt')
    assert (work / "local.txt").read_bytes() == (work / "result.txt").read_bytes()
    assert any(
        method == "GET" and path.startswith("/B") and "%C3%BC" in path and "%2B" in path
        for method, path in probe.requests
    ), probe.requests
    assert all(not path.startswith(f"/{probe.bucket}/") for _, path in probe.requests)
    options["s3_requester_pays"] = False
    run_with_no_payer = s3_script_runner(executable, work, base, probe.bucket, profile_options=options)
    assert "HTTP 403" in run_with_no_payer("ls", expected=1)
    assert probe.refused == 1
    print(f"S3 virtual host signed the correct host, object path and Requester Pays header over {base.split(':', 1)[0]}")


def reference_direct_s3_credentials(executable, root, s3_url, sts_url, accepted_keys, accepted_roles, ca=None):
    work = root / "s3-credentials"
    work.mkdir()
    tls = {}
    if ca is not None:
        tls["ca_bundle"] = str(ca)
        s3_url = s3_url.replace("127.0.0.1", "localhost")
    options = {"user": "ARCOEXPLICIT", "s3_temporary": True, **tls}
    run_without_token = s3_script_runner(executable, work, s3_url, "", profile_options=options)
    assert "fehlt das Sitzungstoken" in run_without_token("ls", select_bucket=False, expected=1)
    run_wrong_token = s3_script_runner(
        executable, work, s3_url, "", profile_options=options,
        open_option="-s3-token-env=ARCO_REFERENCE_TOKEN", ARCO_REFERENCE_TOKEN="wrong-fixture-token",
    )
    assert "HTTP 403" in run_wrong_token("ls", select_bucket=False, expected=1)
    run_token = s3_script_runner(
        executable, work, s3_url, "", profile_options=options,
        open_option="-s3-token-env=ARCO_REFERENCE_TOKEN", ARCO_REFERENCE_TOKEN=DIRECT_TOKEN,
    )
    before = len(accepted_keys)
    run_token("ls", select_bucket=False)
    assert accepted_keys[before:] == ["ARCOEXPLICIT"] * 2
    options["s3_role_arn"] = DIRECT_ROLE
    run_role = s3_script_runner(
        executable, work, s3_url, "", profile_options=options,
        open_option="-s3-token-env=ARCO_REFERENCE_TOKEN", ARCO_REFERENCE_TOKEN=DIRECT_TOKEN,
        AWS_ENDPOINT_URL_STS=sts_url, AWS_EC2_METADATA_DISABLED="true",
    )
    before, assumed = len(accepted_keys), len(accepted_roles)
    run_role("ls", "ls", select_bucket=False)
    assert accepted_keys[before:] == ["ARCOAWSACCESS"] * 3
    assert accepted_roles[assumed:] == ["ARCOEXPLICIT"]

    credentials = work / "credentials"
    credentials.write_text(
        f"[base]\naws_access_key_id = ARCOEXPLICIT\naws_secret_access_key = fixture-only\naws_session_token = {DIRECT_TOKEN}\n",
        encoding="ascii",
    )
    config = work / "aws-config"
    config.write_text("", encoding="ascii")
    sdk_role = s3_script_runner(
        executable, work, s3_url, "",
        profile_options={"user": "", "s3_aws_profile": "base", "s3_role_arn": DIRECT_ROLE, **tls},
        AWS_ENDPOINT_URL_STS=sts_url, AWS_SHARED_CREDENTIALS_FILE=str(credentials),
        AWS_CONFIG_FILE=str(config), AWS_EC2_METADATA_DISABLED="true",
    )
    before, assumed = len(accepted_keys), len(accepted_roles)
    sdk_role("ls", select_bucket=False)
    assert accepted_keys[before:] == ["ARCOAWSACCESS"] * 2
    assert accepted_roles[assumed:] == ["ARCOEXPLICIT"]
    print("S3 signed a direct temporary token, then used it to assume a configured IAM role on local STS")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--harness", type=Path, help="Already built arco-windows lib test harness")
    parser.add_argument("--app-harness", type=Path, help="Already built arco-commander unit test harness")
    parser.add_argument("--net-exe", type=Path, help="Already built arco-net CLI for encrypted and resumable transfers")
    parser.add_argument("--openssl", type=Path, help="Test HTTPS using a temporary certificate")
    parser.add_argument("--digest", action="store_true", help="Require WebDAV Digest authentication")
    parser.add_argument("--windows-negotiate-url", help="WebDAV reference server that requires Windows Negotiate")
    parser.add_argument("--client-cert", action="store_true", help="Temporarily import a generated certificate into CurrentUser\\My (requires --openssl and --net-exe)")
    parser.add_argument("--aws-credentials", choices=("profile", "container", "role"), help="Exercise an SDK credential provider on loopback")
    args = parser.parse_args()
    if args.client_cert and not (args.openssl and args.net_exe):
        parser.error("--client-cert requires --openssl and --net-exe")
    if args.windows_negotiate_url and not args.net_exe:
        parser.error("--windows-negotiate-url requires --net-exe")
    with tempfile.TemporaryDirectory(prefix="arco-http-reference-") as folder:
        root = Path(folder) / "files"
        root.mkdir()
        dav = WsgiDAVApp({
            "provider_mapping": {"/demo": str(root)},
            "simple_dc": {"user_mapping": {"*": {"arco": {"password": "fixture-only"}}}},
            "http_authenticator": {
                "accept_basic": not args.digest,
                "accept_digest": True,
                "default_to_digest": args.digest,
            },
            "verbose": 0,
        })
        dav_interop = WebDavInteropProbe()
        os.environ["MOTO_S3_DEFAULT_MAX_KEYS"] = "2"
        s3 = DomainDispatcherApplication(create_backend_app)
        multipart = MultipartProbe()
        large_copy = LargeVersionCopyProbe()
        mfa_delete = MfaDeleteProbe()
        virtual_host = VirtualHostProbe()
        accepted_s3_keys = []
        accepted_roles = []
        with ExitStack() as servers:
            dav_url = servers.enter_context(serve(bounded_dav_input(dav_interop.wrap(dav))))
            s3_app = signed_s3(multipart.wrap(large_copy.wrap(mfa_delete.wrap(virtual_host.wrap(s3)))), accepted_s3_keys)
            s3_url = servers.enter_context(serve(s3_app))
            virtual_url, s3_ca = s3_url, None
            environment = dict(os.environ, ARCO_TEST_WEBDAV=dav_url + "/demo/", ARCO_TEST_S3=s3_url + "/")
            if args.aws_credentials:
                environment = {key: value for key, value in environment.items() if not key.upper().startswith("AWS_")}
                config = Path(folder) / "aws-config"
                credentials = Path(folder) / "aws-credentials"
                config.write_text("[default]\nregion = eu-central-1\n", encoding="utf-8")
                credentials.write_text("", encoding="utf-8")
                environment.update(AWS_CONFIG_FILE=str(config), AWS_SHARED_CREDENTIALS_FILE=str(credentials),
                                   AWS_EC2_METADATA_DISABLED="true", ARCO_TEST_AWS_PROFILE="")
                if args.aws_credentials == "profile":
                    credentials.write_text(
                        "[arco-test]\naws_access_key_id = ARCOAWSACCESS\n"
                        "aws_secret_access_key = fixture-only\naws_session_token = temporary-test-token\n",
                        encoding="utf-8",
                    )
                    environment.update(ARCO_TEST_AWS_PROFILE="arco-test",
                                       AWS_ACCESS_KEY_ID="WRONGFORNAMEDPROFILE", AWS_SECRET_ACCESS_KEY="wrong")
                elif args.aws_credentials == "container":
                    aws_url = servers.enter_context(serve(aws_credentials_server))
                    environment["AWS_CONTAINER_CREDENTIALS_FULL_URI"] = aws_url + "/credentials"
                else:
                    sts_url = servers.enter_context(serve(aws_role_server))
                    config.write_text(
                        "[profile arco-role]\nrole_arn = arn:aws:iam::123456789012:role/arco-reference\n"
                        "source_profile = role-source\nregion = eu-central-1\n",
                        encoding="utf-8",
                    )
                    credentials.write_text(
                        "[role-source]\naws_access_key_id = ARCOROLESOURCE\naws_secret_access_key = fixture-only\n",
                        encoding="utf-8",
                    )
                    environment.update(ARCO_TEST_AWS_PROFILE="arco-role", AWS_ENDPOINT_URL_STS=sts_url)
            if args.openssl:
                cert, key = Path(folder) / "cert.pem", Path(folder) / "key.pem"
                subprocess.run([
                    str(args.openssl.resolve()), "req", "-x509", "-newkey", "rsa:2048", "-nodes",
                    "-days", "2", "-subj", "/CN=localhost", "-addext",
                    f"subjectAltName=DNS:localhost,DNS:{virtual_host.bucket}.localhost",
                    "-keyout", str(key), "-out", str(cert),
                ], capture_output=True, check=True, creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
                https = servers.enter_context(serve(bounded_dav_input(dav), (str(cert), str(key))))
                environment.update(ARCO_TEST_WEBDAV_TLS=https + "/demo/", ARCO_TEST_WEBDAV_CA=str(cert))
                if args.net_exe:
                    virtual_url = servers.enter_context(serve(s3_app, (str(cert), str(key))))
                    s3_ca = cert
            command = [str(args.harness.resolve()), "reference_native_http_"] if args.harness else [
                "cargo", "test", "--locked", "-p", "arco-windows", "--lib", "reference_native_http_", "--",
            ]
            subprocess.run(
                [*command, "--ignored", "--nocapture", "--test-threads=1"],
                env=environment, check=True, timeout=300,
            )
            if args.net_exe:
                reference_dav_interop(args.net_exe, root, Path(folder), dav_url, dav_interop)
                reference_windows_negotiate(args.net_exe, Path(folder), args.windows_negotiate_url)
                if not args.digest:
                    reference_dav_redirects(args.net_exe, root, Path(folder), dav, (cert, key) if args.openssl else None, args.harness)
                if args.client_cert:
                    thumbprint, client_cert = servers.enter_context(reference_client_certificate(
                        args.openssl.resolve(), Path(folder), getattr(subprocess, "CREATE_NO_WINDOW", 0),
                    ))
                    mutual_tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
                    mutual_tls.load_cert_chain(cert, key)
                    mutual_tls.load_verify_locations(cafile=client_cert)
                    mutual_tls.verify_mode = ssl.CERT_REQUIRED
                    mutual_url = servers.enter_context(serve(bounded_dav_input(dav), mutual_tls))
                    reference_dav_client_certificate(args.net_exe, root, Path(folder), mutual_url, cert, thumbprint)
                encrypted_transfers(args.net_exe, Path(folder), dav_url + "/demo/", s3_url)
                reference_multipart_resume(args.net_exe, Path(folder), s3_url, multipart)
                reference_s3_version_commands(args.net_exe, Path(folder), s3_url)
                reference_large_version_copy(args.net_exe, Path(folder), s3_url, large_copy)
                reference_s3_virtual_host(args.net_exe, Path(folder), virtual_url, virtual_host, s3_ca)
                if s3_ca is not None:
                    reference_s3_mfa_delete(args.net_exe, Path(folder), s3_url, virtual_url, s3_ca, mfa_delete)
                direct_sts_url = servers.enter_context(
                    serve(lambda environ, send_response: aws_role_server(environ, send_response, accepted_roles))
                )
                reference_direct_s3_credentials(
                    args.net_exe, Path(folder), virtual_url, direct_sts_url, accepted_s3_keys, accepted_roles, s3_ca,
                )
            if args.app_harness:
                for test in ("app::ftp_sync::tests::webdav_and_s3_search_sync_and_relay",
                             "app::ftp_edit::tests::http_drafts_publish_and_keep_conflicting_edits"):
                    result = subprocess.run(
                        [str(args.app_harness.resolve()), test, "--exact", "--ignored", "--nocapture"],
                        env=environment, timeout=120, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                        encoding="utf-8", errors="replace", creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
                    )
                    print(result.stdout)
                    result.check_returncode()
                    assert "1 passed" in result.stdout, result.stdout


if __name__ == "__main__":
    main()
