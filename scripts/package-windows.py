"""Package and smoke-test the Windows release binaries without opening windows."""

import argparse
import hashlib
import json
import os
import re
import shutil
import struct
import subprocess
import tempfile
import urllib.request
import zipfile
from io import BytesIO
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
TARGET = ROOT / "target"
X64 = "x86_64-pc-windows-msvc"
X86 = "i686-pc-windows-msvc"
WEBVIEW2_SDK = "1.0.3650.58"  # WebView2LoaderStatic in webview2-com-sys 0.39.1
WEBVIEW2_PACKAGE = "microsoft.web.webview2"
WEBVIEW2_GALLERY = f"https://www.nuget.org/packages/Microsoft.Web.WebView2/{WEBVIEW2_SDK}"
REPO = "https://github.com/Mucumba77/arco-commander"
DOCS = (
    "LICENSE", "README.md", "CONTRIBUTING.md", "SECURITY.md",
    "CHANGELOG.md", "FUNKTIONSMATRIX.md", "PLAN.md",
    "TOTAL-COMMANDER-REFERENZ.md", "docs/RELEASE.md",
    "docs/UI-PRUEFUNG.md", "docs/WEITERARBEIT.md",
    "docs/SUCHE-UND-WERKZEUGE.md", "docs/AREA3-ABSCHLUSS.md",
    "docs/NAVIGATION-UND-DATEIEN.md", "docs/BEREICH1-2-ABSCHLUSS.md",
    "docs/ARCHIVE.md", "docs/AREA4-ABSCHLUSS.md",
    "docs/branding/arco-commander.svg",
    "docs/screenshots/hauptfenster.jpg", "docs/screenshots/kopierfortschritt.jpg",
    "docs/screenshots/lister.jpg",
)
LICENSE_NAME = re.compile(
    r"^(?:licen[cs]es?|copying|copyright|notice|unlicense|third[-_ ]?party[-_ ]?notices)(?:$|[_. -])",
    re.IGNORECASE,
)


def require(condition, message):
    if not condition:
        raise SystemExit(message)


def git(*args):
    return subprocess.check_output(["git", *args], cwd=ROOT, text=True).strip()


def metadata(target):
    return json.loads(subprocess.check_output([
        "cargo", "metadata", "--format-version", "1", "--locked", "--offline",
        "--filter-platform", target,
    ], cwd=ROOT))


def dependencies(meta, workspace_names):
    packages = {package["id"]: package for package in meta["packages"]}
    nodes = {node["id"]: node for node in meta["resolve"]["nodes"]}
    pending = [member for member in meta["workspace_members"] if packages[member]["name"] in workspace_names]
    seen = set()
    while pending:
        package_id = pending.pop()
        if package_id in seen:
            continue
        seen.add(package_id)
        package = packages[package_id]
        if package["source"]:
            yield package
        pending.extend(
            dep["pkg"] for dep in nodes[package_id]["deps"]
            if any(kind["kind"] != "dev" for kind in dep["dep_kinds"])
        )


def write_notices(stage, packages):
    licenses = stage / "licenses"
    notice = [
        "# Drittanbieterhinweise", "",
        "Arco Commander verwendet [Slint](https://slint.dev/).", "",
        f"[Microsoft WebView2 SDK {WEBVIEW2_SDK}]({WEBVIEW2_GALLERY}). "
        f"Die Lizenztexte liegen in `licenses/microsoft-webview2-sdk-{WEBVIEW2_SDK}`.", "",
        "Die folgenden Rust-Pakete sind im Windows-Build oder seiner Erzeugung eingebunden. "
        "Mitgelieferte Lizenztexte liegen unter `licenses`.", "",
        "| Paket | Lizenzangabe |", "| --- | --- |",
    ]
    for (name, version), package in sorted(packages.items()):
        crate = Path(package["manifest_path"]).parent
        files = {path for path in crate.iterdir() if path.is_file() and LICENSE_NAME.match(path.name)}
        for folder in (crate / "LICENSES", crate / "licenses"):
            if folder.is_dir():
                files.update(path for path in folder.iterdir() if path.is_file())
        if package["license_file"]:
            path = (crate / package["license_file"]).resolve()
            if path.is_file() and path.is_relative_to(crate.resolve()):
                files.add(path)
        for path in sorted(files):
            destination = licenses / f"{name}-{version}" / path.relative_to(crate)
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(path, destination)
        license_label = (package["license"] or package["license_file"] or "Siehe Paketquelle").replace("|", " / ")
        notice.append(f"| [{name} {version}](https://crates.io/crates/{name}/{version}) | {license_label} |")

    sdk = licenses / f"microsoft-webview2-sdk-{WEBVIEW2_SDK}"
    sdk.mkdir(parents=True)
    nuget = f"{WEBVIEW2_PACKAGE}/{WEBVIEW2_SDK}/{WEBVIEW2_PACKAGE}.{WEBVIEW2_SDK}.nupkg"
    with urllib.request.urlopen(f"https://api.nuget.org/v3-flatcontainer/{nuget}", timeout=60) as response:
        with zipfile.ZipFile(BytesIO(response.read())) as archive:
            for name in ("LICENSE.txt", "NOTICE.txt"):
                (sdk / name).write_bytes(archive.read(name))
    (stage / "DRITTANBIETER.md").write_text("\n".join(notice) + "\n", encoding="utf-8")


def copy_executable(source, destination, machine, subsystem):
    data = source.read_bytes()
    offset = struct.unpack_from("<I", data, 0x3c)[0]
    require(data[:2] == b"MZ" and data[offset:offset + 4] == b"PE\0\0", f"Invalid PE header: {source}")
    require(struct.unpack_from("<H", data, offset + 4)[0] == machine, f"Unexpected architecture: {source}")
    require(subsystem is None or struct.unpack_from("<H", data, offset + 0x5c)[0] == subsystem, f"Unexpected subsystem: {source}")
    for local in (str(Path.home()), str(ROOT)):
        for path in (local, local.replace("\\", "/")):
            for encoding in ("utf-8", "utf-16-le"):
                require(path.encode(encoding) not in data, f"Local build path in {source.name}. Rebuild with path remapping.")
    destination.write_bytes(data)


def copy_seven_zip(directory, stage):
    manifest = json.loads((directory / "manifest.json").read_text(encoding="utf-8"))
    require(manifest["version"] == "26.06", "Prepare the pinned 7-Zip 26.06 runtime first")
    expected = {"7z.exe", "7z.dll", "7z.sfx", "License.txt", "readme.txt", "source/7z2606-src.7z"}
    require(set(manifest["files"]) == expected, "Unexpected 7-Zip manifest members")
    for name, digest in manifest["files"].items():
        source = directory / name
        require(hashlib.sha256(source.read_bytes()).hexdigest() == digest, f"7-Zip hash mismatch: {name}")
        destination = stage / "7zip" / name
        destination.parent.mkdir(parents=True, exist_ok=True)
        if name in {"7z.exe", "7z.dll", "7z.sfx"}:
            copy_executable(source, destination, 0x8664, 3 if name == "7z.exe" else None)
        else:
            shutil.copyfile(source, destination)
    shutil.copyfile(directory / "manifest.json", stage / "7zip" / "manifest.json")


def smoke_test(archive, name, version, *, run_app=True, run_sfx=True):
    def run(executable, *args, stdin=None):
        return subprocess.run(
            [str(executable), *map(str, args)], cwd=executable.parent, input=stdin,
            capture_output=True, timeout=25, creationflags=subprocess.CREATE_NO_WINDOW,
        )

    with tempfile.TemporaryDirectory(prefix="arco-release-check-") as directory:
        root = Path(directory)
        with zipfile.ZipFile(archive) as package:
            require(package.testzip() is None, "Release ZIP failed its CRC check")
            package.extractall(root)
        app = root / name / "arco-commander.exe"
        if run_app:
            command = run(app, "--version")
            require(command.returncode == 0 and command.stdout.strip() == f"Arco Commander {version}".encode(),
                    "The application binary does not report the release version")
            command = run(app, "--help")
            require(command.returncode == 0 and b"--portable" in command.stdout, "The application help failed")
        else:
            require(f"Arco Commander {version}".encode() in app.read_bytes(),
                    "The application binary does not contain the release version")

        protocols = set()
        for helper in ("arco-plugin-host.exe", "arco-plugin-host-x86.exe"):
            command = run(app.with_name(helper), stdin=b"{}\n")
            require(command.returncode == 0, f"{helper} failed to handle an invalid request")
            message = json.loads(command.stdout)
            require(message["result"].get("Err"), f"{helper} did not return a protocol error")
            protocols.add(message["version"])
        require(len(protocols) == 1, "The plugin hosts use different protocol versions")

        seven = app.parent / "7zip" / "7z.exe"
        command = run(seven, "i")
        require(command.returncode == 0 and b"7-Zip 26.06" in command.stdout, "Bundled 7-Zip version check failed")
        source = root / "probe.txt"
        source.write_bytes(b"Arco console archive check")
        command = run(seven, "a", "-tzip", "-spd", "--", root / "probe.zip", source)
        require(command.returncode == 0, "Bundled 7-Zip ZIP creation failed")
        require(run(seven, "t", "--", root / "probe.zip").returncode == 0, "Bundled 7-Zip ZIP test failed")
        if not run_sfx:
            return

        sfx = root / "probe.exe"
        shutil.copyfile(app.with_name("arco-zip-sfx.exe"), sfx)
        with zipfile.ZipFile(sfx, "a", compression=zipfile.ZIP_DEFLATED) as payload:
            payload.comment = b"Arco Commander ZIP SFX v1"
            payload.writestr("Größe/Probe.txt", b"Arco release ZIP-SFX")
        destination = root / "unpacked"
        destination.mkdir()
        command = run(sfx, "--extract-to", destination)
        result = destination / "Größe" / "Probe.txt"
        require(command.returncode == 0 and result.read_bytes() == b"Arco release ZIP-SFX", "ZIP-SFX extraction failed")
        result.write_bytes(b"keep")
        command = run(sfx, "--extract-to", destination)
        require(command.returncode != 0 and result.read_bytes() == b"keep", "ZIP-SFX did not protect an existing file")


def package_release(*, run_app=True, run_sfx=True, seven_zip_dir=TARGET / "tools" / "7zip-26.06"):
    require(os.name == "nt", "Run packaging and smoke tests on Windows")
    require(not git("status", "--porcelain"), "Commit the release sources before packaging")
    commit = git("rev-parse", "HEAD")
    native = metadata(X64)
    app = next(package for package in native["packages"] if package["name"] == "arco-app" and not package["source"])
    version = app["version"]
    name = f"arco-commander-v{version}-windows-x64"
    output = TARGET / "distribution" / f"v{version}"
    output.mkdir(parents=True, exist_ok=True)
    final_archive = output / f"{name}.zip"
    require(not final_archive.exists(), f"Release archive already exists: {final_archive}")

    native_binaries = TARGET / X64 / "release"
    wow_binaries = TARGET / X86 / "release"
    with tempfile.TemporaryDirectory(prefix="arco-package-", dir=output) as scratch:
        stage = Path(scratch) / name
        stage.mkdir()
        for filename, folder, machine, subsystem in (
            ("arco-commander.exe", native_binaries, 0x8664, 2),
            ("arco-zip-sfx.exe", native_binaries, 0x8664, 2),
            ("arco-plugin-host.exe", native_binaries, 0x8664, 3),
            ("arco-plugin-host-x86.exe", wow_binaries, 0x14c, 3),
        ):
            source_name = "arco-plugin-host.exe" if folder == wow_binaries else filename
            copy_executable(folder / source_name, stage / filename, machine, subsystem)
        copy_seven_zip(seven_zip_dir, stage)
        for document in (*DOCS, f"docs/releases/v{version}.md"):
            destination = stage / document
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / document, destination)
        (stage / "START-HIER.txt").write_text(f"""Arco Commander {version}
Windows 10 22H2 und Windows 11 (x64)

Voraussetzung: aktuelle Microsoft Visual C++ v14-Laufzeit (x64).
Fuer den 32-Bit-Plugin-Host wird zusaetzlich die x86-Laufzeit benoetigt.
Offizielle Downloads (beide Architekturen):
https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist/
Die Laufzeit-Installer sind nicht im ZIP enthalten.

1. Entpacke das gesamte ZIP in einen Ordner deiner Wahl.
2. Starte arco-commander.exe. Die drei Hilfsprogramme bleiben daneben.
3. Mit --portable speichert Arco das Profil im Programmordner. Standardmäßig
   verwendet Arco das Profil unter %LOCALAPPDATA%\\Arco Commander.

Für die formatierte Vorschau und Medien im Lister braucht Windows die
Microsoft Edge WebView2 Runtime. Optionale Archivwerkzeuge stehen im README.
https://developer.microsoft.com/microsoft-edge/webview2/#download

7-Zip 26.06 ist unter 7zip/ enthalten (Konsole, SFX-Modul, Lizenz und Quellcode).
ACE/UC2 benoetigen die optionale Python-Umgebung aus README.md.
Archiveinstellungen und Formatgrenzen: docs/ARCHIVE.md

Das Paket ist unsigniert. Release-Hinweise und SHA-256-Prüfsumme stehen auf GitHub.
{REPO}/releases/tag/v{version}

Quellstand: {REPO}/commit/{commit}
Funktionsumfang und offene Varianten: FUNKTIONSMATRIX.md
Drittanbieterhinweise: DRITTANBIETER.md und licenses/
""", encoding="utf-8-sig", newline="\r\n")

        packages = {}
        for meta, roots in ((native, {"arco-app", "arco-plugin-host"}), (metadata(X86), {"arco-plugin-host"})):
            for package in dependencies(meta, roots):
                packages[package["name"], package["version"]] = package
        write_notices(stage, packages)
        with (stage / "DRITTANBIETER.md").open("a", encoding="utf-8") as notice:
            notice.write("\n## 7-Zip 26.06\n\nCopyright (C) 1999–2026 Igor Pavlov. "
                         "LGPL mit den in `7zip/License.txt` genannten BSD-/unRAR-Bedingungen. "
                         "Die Dateien sind unverändert und separat austauschbar. "
                         "Zugehöriger Quellcode: `7zip/source/7z2606-src.7z`. "
                         "[Originalrelease](https://github.com/ip7z/7zip/releases/tag/26.06).\n")

        archive = Path(scratch) / final_archive.name
        with zipfile.ZipFile(archive, "x", compression=zipfile.ZIP_DEFLATED, compresslevel=6) as package:
            for path in sorted(stage.rglob("*")):
                if path.is_file():
                    package.write(path, path.relative_to(stage.parent).as_posix())
        smoke_test(archive, name, version, run_app=run_app, run_sfx=run_sfx)
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        archive.rename(final_archive)
        (output / "SHA256SUMS.txt").write_text(f"{digest}  {final_archive.name}\n", encoding="ascii")
    version_check = "application version/help" if run_app else "embedded version (application not started)"
    sfx_check = "ZIP-SFX execution" if run_sfx else "ZIP-SFX static PE check (not executed)"
    print(f"Verified {version_check}, both plugin hosts, 7-Zip, {sfx_check} and ZIP CRC: {final_archive}")
    print(f"Dependency notices: {len(packages)}. SHA-256: {digest}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--no-app-run", action="store_true",
                        help="Never execute arco-commander.exe; check its embedded version statically")
    parser.add_argument("--no-sfx-run", action="store_true", help="Check SFX statically without executing it")
    parser.add_argument("--seven-zip-dir", type=Path, default=TARGET / "tools" / "7zip-26.06")
    arguments = parser.parse_args()
    package_release(run_app=not arguments.no_app_run, run_sfx=not arguments.no_sfx_run, seven_zip_dir=arguments.seven_zip_dir)
