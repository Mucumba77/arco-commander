"""Prepare pinned 7-Zip console files without executing its installer."""
import argparse
import hashlib
import json
import subprocess
import urllib.request
from pathlib import Path

VERSION = "26.06"
FILES = {
    "7z2606-x64.exe": "9b697f65ecc2d9b0152f5ee96d72dde522a7827690349d9307935dacf40f42ec",
    "7z2606-src.7z": "e0364d39e8208557d939472c9a12d02ed1c12f45ca402e3b408bcaa63b36dd93",
}
MEMBERS = ("7z.exe", "7z.dll", "7z.sfx", "License.txt", "readme.txt")


def prepare(extractor, output):
    output.mkdir(parents=True, exist_ok=True)
    sources = output / "source"
    sources.mkdir(exist_ok=True)
    for name, expected in FILES.items():
        path = sources / name
        url = f"https://github.com/ip7z/7zip/releases/download/{VERSION}/{name}"
        if not path.exists():
            with urllib.request.urlopen(url, timeout=90) as response:
                data = response.read(32 * 1024 * 1024 + 1)
            if hashlib.sha256(data).hexdigest() != expected:
                raise ValueError(f"Unexpected download hash: {name}")
            path.write_bytes(data)
        if hashlib.sha256(path.read_bytes()).hexdigest() != expected:
            raise ValueError(f"Unexpected cached file hash: {name}")
    # The installer is only opened as an archive by the existing console reader.
    subprocess.run([str(extractor.resolve()), "x", "-y", f"-o{output.resolve()}",
                    str((sources / "7z2606-x64.exe").resolve()), *MEMBERS],
                   check=True, capture_output=True, timeout=60,
                   creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
    manifest = {"version": VERSION, "upstream": "https://github.com/ip7z/7zip",
                "source_archive": "source/7z2606-src.7z", "files": {}}
    for name in (*MEMBERS, "source/7z2606-src.7z"):
        manifest["files"][name] = hashlib.sha256((output / name).read_bytes()).hexdigest()
    # Do not distribute the installer: only the verified extracted runtime and source.
    (sources / "7z2606-x64.exe").unlink()
    (output / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    print(f"Prepared 7-Zip {VERSION}, console runtime and corresponding source: {output}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--extractor", type=Path, required=True)
    parser.add_argument("--output", type=Path, default=Path("target/tools/7zip-26.06"))
    args = parser.parse_args()
    prepare(args.extractor, args.output)
