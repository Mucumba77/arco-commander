"""Temporary client-certificate identity for Windows loopback protocol tests."""

import contextlib
import hashlib
import os
from pathlib import Path
import ssl
import subprocess


@contextlib.contextmanager
def reference_client_certificate(openssl, folder, flags):
    cert = folder / "client.pem"
    key = folder / "client.key"
    pfx = folder / "client.pfx"
    subprocess.run([
        str(openssl), "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "2",
        "-subj", "/CN=Arco protocol loopback client",
        "-addext", "basicConstraints=critical,CA:FALSE",
        "-addext", "keyUsage=critical,digitalSignature",
        "-addext", "extendedKeyUsage=clientAuth",
        "-keyout", str(key), "-out", str(cert),
    ], check=True, capture_output=True, creationflags=flags)
    subprocess.run([
        str(openssl), "pkcs12", "-export", "-in", str(cert), "-inkey", str(key),
        "-out", str(pfx), "-passout", "pass:arco-loopback-only",
    ], check=True, capture_output=True, creationflags=flags)
    thumbprint = hashlib.sha1(ssl.PEM_cert_to_DER_cert(cert.read_text("ascii"))).hexdigest().upper()
    env = dict(os.environ, ARCO_TEST_PFX=str(pfx), ARCO_TEST_CERT_THUMBPRINT=thumbprint)
    env.pop("PSMODULEPATH", None)  # Use the module path for Windows PowerShell, even under pwsh.
    powershell = Path(os.environ["WINDIR"]) / "System32/WindowsPowerShell/v1.0/powershell.exe"

    def run(statement):
        result = subprocess.run(
            [str(powershell), "-NoProfile", "-NonInteractive", "-Command",
             "$ErrorActionPreference = 'Stop'; [Console]::OutputEncoding = [Text.Encoding]::UTF8; "
             "$path = 'Cert:\\CurrentUser\\My\\' + $env:ARCO_TEST_CERT_THUMBPRINT; " + statement],
            env=env, capture_output=True, encoding="utf-8", errors="replace",
            timeout=30, creationflags=flags,
        )
        if result.returncode:
            raise RuntimeError(f"Windows certificate fixture failed: {result.stderr.strip()}")

    run("if (Test-Path -LiteralPath $path) { throw 'Fixture certificate already exists' }")
    try:
        run(
            "$pass = ConvertTo-SecureString 'arco-loopback-only' -AsPlainText -Force; "
            "Import-PfxCertificate -FilePath $env:ARCO_TEST_PFX -Password $pass "
            "-CertStoreLocation 'Cert:\\CurrentUser\\My' | Out-Null; "
            "if (!(Get-Item -LiteralPath $path).HasPrivateKey) { throw 'Missing test key' }"
        )
        yield thumbprint, cert
    finally:
        run("if (Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path -DeleteKey }")
