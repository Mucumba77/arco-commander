"""Prepare pinned upstream archive fixtures and independent payload hashes.

Only console readers (7-Zip and Windows bsdtar) are run.
Neither Arco Commander nor any file contained in an archive is executed.
Downloads and generated files stay under target/area4/corpus by default.
"""
import argparse
import binascii
import hashlib
import json
import os
from pathlib import Path
import subprocess
import urllib.parse
import urllib.request

LIBARCHIVE = "a94150bafc7b5e91f6394d160486f4988a99e79b"
ACEFILE = "eb3ee7b1541a5d641fdfc115df06c3ba478e09e0"
FIXTURES = [
    "test_read_format_cab_1.cab", "test_read_format_cab_2.cab", "test_read_format_cab_3.cab",
    "test_read_format_cab_lzx_16bit.cab", "test_read_format_cab_lzx_e8.cab",
    "test_read_format_cab_multi.p1.cab", "test_read_format_cab_multi.p2.cab",
    *[f"test_read_format_lha_header{i}.lzh" for i in range(4)],
    *[f"test_read_format_lha_lh{i}.lzh" for i in [0, 6, 7]],
    "test_read_format_rar5_compressed.rar", "test_read_format_rar5_solid.rar",
    "test_read_format_rar4_encrypted_filenames.rar", "test_read_format_rar4_solid_encrypted.rar",
    "test_read_format_rar4_solid_encrypted_filenames.rar", "test_read_format_rar5_encrypted_filenames.rar",
    "test_read_format_rar5_solid_encrypted.rar", "test_read_format_rar5_solid_encrypted_filenames.rar",
    *[f"test_read_format_rar5_multiarchive_solid.part{i:02}.rar" for i in range(1, 5)],
    *[f"test_read_format_rar_multivolume.part{i:04}.rar" for i in range(1, 5)],
]


def fetch(url, target):
    if not target.exists():
        with urllib.request.urlopen(url, timeout=45) as response:
            data = response.read(16 * 1024 * 1024 + 1)
        if len(data) > 16 * 1024 * 1024:
            raise ValueError("reference download exceeds 16 MiB")
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
    return target.read_bytes()


def decode_uu(data):
    lines = data.splitlines()
    begin = next(index for index, line in enumerate(lines) if line.startswith(b"begin "))
    output = bytearray()
    for line in lines[begin + 1:]:
        if line == b"end":
            break
        if line:
            output.extend(binascii.a2b_uu(line))
    return output


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path("target/area4/corpus"))
    parser.add_argument("--seven-zip", type=Path, default=Path("target/debug/7zip/7z.exe"))
    parser.add_argument("--archive-python", type=Path, default=Path("target/debug/archive-python/Scripts/python.exe"))
    args = parser.parse_args()
    root = args.root.resolve()
    root.mkdir(parents=True, exist_ok=True)
    seven = args.seven_zip.resolve()
    if not seven.is_file():
        raise SystemExit("The 7-Zip console helper is missing.")
    flags = 0x08000000 if os.name == "nt" else 0

    def run(arguments, password=None):
        secret = b"" if password is None else (password + "\n").encode()
        result = subprocess.run([str(seven), *arguments], input=secret, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE, timeout=90, creationflags=flags)
        if result.returncode:
            raise RuntimeError(result.stderr.decode("utf-8", errors="replace").strip() or f"7-Zip exit {result.returncode}")
        return result.stdout

    origins, cases, unavailable = [], [], []
    directory = root / "libarchive"
    directory.mkdir(exist_ok=True)
    for name in FIXTURES:
        url = f"https://raw.githubusercontent.com/libarchive/libarchive/{LIBARCHIVE}/libarchive/test/{name}.uu"
        encoded = fetch(url, directory / (name + ".uu"))
        archive = decode_uu(encoded)
        path = directory / name
        path.write_bytes(archive)
        origins.append({"file": str(path.relative_to(root)), "url": url,
                        "sha256": hashlib.sha256(archive).hexdigest()})
    for name in FIXTURES:
        if name.endswith(".p2.cab") or ".part" in name and not (".part01." in name or ".part0001." in name):
            continue
        path = directory / name
        password = "password" if "encrypted" in name else None
        try:
            listing = run(["l", "-slt", "-ba", "-sccUTF-8", "--", str(path)], password).decode("utf-8-sig")
            rows = []
            for paragraph in listing.replace("\r", "").split("\n\n"):
                row = dict(line.split(" = ", 1) for line in paragraph.splitlines() if " = " in line)
                if "Path" in row:
                    rows.append(row)
            if any(row.get("Symbolic Link") or row.get("Hard Link") or row.get("Method") == "-lhd-" and "|" in row["Path"]
                   or any(value.startswith('l') and len(value) >= 10 for value in row.get('Attributes', '').split()) for row in rows):
                cases.append({"file": str(path.relative_to(root)), "password": password, "reject_links": True, "members": []})
                continue
            run(["t", "-bso0", "-bd", "--", str(path)], password)
            members = []
            for row in rows:
                directory_entry = row.get("Folder") == "+" or "D" in row.get("Attributes", "")
                member = {"name": row["Path"].replace("\\", "/"), "directory": directory_entry,
                          "size": 0 if directory_entry else int(row["Size"])}
                if not directory_entry:
                    data = run(["x", "-so", "-spd", "-bd", "--", str(path), row["Path"]], password)
                    if len(data) != member["size"]:
                        raise ValueError("independent extraction size mismatch")
                    member["sha256"] = hashlib.sha256(data).hexdigest()
                members.append(member)
            cases.append({"file": str(path.relative_to(root)), "password": password, "members": members})
        except (RuntimeError, ValueError) as error:
            # Two upstream cases expose valid formats unsupported by 7-Zip.
            # Windows' independently maintained libarchive reader supplies the
            # reference payloads; Arco's output is never its own oracle.
            if os.name == "nt" and name in {"test_read_format_cab_lzx_16bit.cab", "test_read_format_lha_header3.lzh"}:
                tar = str(Path(os.environ["SystemRoot"]) / "System32/tar.exe")
                def tar_run(*arguments):
                    result = subprocess.run([tar, *arguments], capture_output=True, timeout=90, creationflags=flags)
                    if result.returncode:
                        raise RuntimeError(result.stderr.decode(errors="replace"))
                    return result.stdout
                members = []
                for member_name in tar_run("-tf", str(path)).decode("utf-8").splitlines():
                    if not member_name:
                        continue
                    directory_entry = member_name.endswith("/")
                    member = {"name": member_name.rstrip("/"), "directory": directory_entry, "size": 0}
                    if not directory_entry:
                        data = tar_run("-xOf", str(path), member_name)
                        member.update(size=len(data), sha256=hashlib.sha256(data).hexdigest())
                    members.append(member)
                cases.append({"file": str(path.relative_to(root)), "password": None, "members": members, "oracle": "Windows bsdtar"})
            else:
                unavailable.append({"file": name, "reason": str(error)})

    # Real ACE 2.0 blocked data from acefile's upstream interoperability corpus.
    for relative, local in [
        ("codepage/ace32-cafe-filename_encoding=cp437_filenamecontains=café.ace", "cp437.ace"),
        ("blocked_unregistered/winappdbg-winappdbg_v1.6_plain.ace", "blocked.ace"),
    ]:
        url = f"https://raw.githubusercontent.com/droe/acefile-testdata/{ACEFILE}/" + urllib.parse.quote(relative)
        archive = fetch(url, root / "ace" / local)
        origins.append({"file": "ace/" + local, "url": url, "sha256": hashlib.sha256(archive).hexdigest()})

    report = {"libarchive_commit": LIBARCHIVE, "acefile_commit": ACEFILE,
              "origins": origins, "cases": cases, "unavailable": unavailable}
    (root / "manifest.json").write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"Prepared {len(cases)} cases and {len(origins)} source files; {len(unavailable)} explicitly unavailable.")
    for item in unavailable:
        print(f"  {item['file']}: {item['reason'][:180]}")


if __name__ == "__main__":
    main()
