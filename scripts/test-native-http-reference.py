"""Exercise Arco's native WebDAV and S3 sessions with disposable local servers."""

import argparse
from contextlib import contextmanager
from contextlib import ExitStack
from datetime import datetime, timedelta, timezone
import hmac
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading

from botocore.auth import S3SigV4Auth
from botocore.awsrequest import AWSRequest
from botocore.credentials import Credentials
from moto.server import DomainDispatcherApplication, create_backend_app
from werkzeug.serving import WSGIRequestHandler, make_server
from werkzeug.wsgi import LimitedStream
from wsgidav.wsgidav_app import WsgiDAVApp


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


def signed_s3(app):
    def request(environ, start_response):
        authorization = environ.get("HTTP_AUTHORIZATION", "")
        scheme, _, parameters = authorization.partition(" ")
        parts = dict(value.strip().split("=", 1) for value in parameters.split(",") if "=" in value)
        key, _, _ = parts.get("Credential", "").partition("/")
        expected_token = {"ARCOACCESS": None, "ARCOAWSACCESS": "temporary-test-token"}
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
                return app(environ, start_response)
        start_response("403 Forbidden", [("Content-Length", "0")])
        return []
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


def aws_role_server(environ, start_response):
    body = environ["wsgi.input"].read(int(environ.get("CONTENT_LENGTH") or 0))
    if (
        environ["REQUEST_METHOD"] != "POST"
        or b"Action=AssumeRole" not in body
        or "Credential=ARCOROLESOURCE/" not in environ.get("HTTP_AUTHORIZATION", "")
    ):
        start_response("403 Forbidden", [("Content-Length", "0")])
        return []
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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--harness", type=Path, help="Already built arco-windows lib test harness")
    parser.add_argument("--app-harness", type=Path, help="Already built arco-commander unit test harness")
    parser.add_argument("--net-exe", type=Path, help="Already built arco-net CLI for encrypted transfers")
    parser.add_argument("--openssl", type=Path, help="Test HTTPS using a temporary certificate")
    parser.add_argument("--digest", action="store_true", help="Require WebDAV Digest authentication")
    parser.add_argument("--aws-credentials", choices=("profile", "container", "role"), help="Exercise an SDK credential provider on loopback")
    args = parser.parse_args()
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
        os.environ["MOTO_S3_DEFAULT_MAX_KEYS"] = "2"
        s3 = DomainDispatcherApplication(create_backend_app)
        with ExitStack() as servers:
            dav_url = servers.enter_context(serve(bounded_dav_input(dav)))
            s3_url = servers.enter_context(serve(signed_s3(s3)))
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
                    "-days", "2", "-subj", "/CN=localhost", "-addext", "subjectAltName=DNS:localhost",
                    "-keyout", str(key), "-out", str(cert),
                ], capture_output=True, check=True, creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
                https = servers.enter_context(serve(bounded_dav_input(dav), (str(cert), str(key))))
                environment.update(ARCO_TEST_WEBDAV_TLS=https + "/demo/", ARCO_TEST_WEBDAV_CA=str(cert))
            command = [str(args.harness.resolve()), "reference_native_http_"] if args.harness else [
                "cargo", "test", "--locked", "-p", "arco-windows", "--lib", "reference_native_http_", "--",
            ]
            subprocess.run(
                [*command, "--ignored", "--nocapture", "--test-threads=1"],
                env=environment, check=True, timeout=300,
            )
            if args.net_exe:
                encrypted_transfers(args.net_exe, Path(folder), dav_url + "/demo/", s3_url)
            if args.app_harness:
                result = subprocess.run(
                    [str(args.app_harness.resolve()), "app::ftp_sync::tests::webdav_and_s3_search_sync_and_relay",
                     "--exact", "--ignored", "--nocapture"],
                    env=environment, timeout=120, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                    encoding="utf-8", errors="replace", creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
                )
                print(result.stdout)
                result.check_returncode()


if __name__ == "__main__":
    main()
