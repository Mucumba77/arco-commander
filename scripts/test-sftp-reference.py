"""Run Arco's opt-in SFTP workflow against a disposable Paramiko server."""

import argparse
import base64
from contextlib import ExitStack, contextmanager
import hashlib
import json
import logging
import os
from pathlib import Path
import shlex
import socket
import socketserver
import struct
import subprocess
import tempfile
import threading
import time
import uuid
from urllib.parse import quote, urlencode

import paramiko
from cryptography.hazmat.primitives.asymmetric import ec, ed25519
from cryptography.hazmat.primitives.serialization import (
    BestAvailableEncryption, Encoding, NoEncryption, PrivateFormat,
)


APP_TEST = "app::ftp_sync::tests::ssh_search_and_bidirectional_sync"
WATCH_TEST = "app::network_watch::tests::reference_network_watch"
WATCH_DELETE_TEST = "app::network_watch::tests::reference_network_watch_deletes_only_confirmed_files"


def reference_script(executable, work, remote_root, environment):
    profile = work / "script-profile.json"
    profile.write_text(json.dumps({"ftp_profiles": {"Reference site": {
        "address": environment["ARCO_SFTP_TEST_URL"],
        "security": "sftp",
        "user": "arco",
        "ssh_fingerprint": environment["ARCO_SFTP_TEST_FINGERPRINT"],
    }}}), encoding="utf-8")
    local = work / "script files"
    local.mkdir()
    payload = b"native SFTP script"
    (local / "source.txt").write_bytes(payload)
    script = work / "sftp-script.txt"
    script.write_text(
        f'echo "Upload #1"\nlcd "{local}"\nlls *.TXT\nopen "Reference site"\nmkdir "script round trip"\n'
        'cd "script round trip"\nput source.txt "Büro #1.txt"\nls\n'
        'stat "Büro #1.txt"\nchecksum "Büro #1.txt"\nchmod 0640 "Büro #1.txt"\n'
        'get "Büro #1.txt" downloaded.txt\nmv "Büro #1.txt" moved.txt\n'
        'rm moved.txt\ncd ..\nrmdir "script round trip"\nclose\nexit\n',
        encoding="utf-8",
    )
    args = [str(executable.resolve()), "--profile", str(profile), "--script", str(script),
            "--password-env", "ARCO_REFERENCE_SCRIPT_PASSWORD"]
    environment = dict(environment, ARCO_REFERENCE_SCRIPT_PASSWORD="fixture-only")
    result = subprocess.run(args, env=environment, check=True, timeout=45, capture_output=True,
                            encoding="utf-8", creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
    assert (local / "downloaded.txt").read_bytes() == payload
    events = [json.loads(line)["event"] for line in result.stdout.splitlines()]
    assert events[0]["message"] == "Upload #1"
    assert [entry["name"] for entry in events[2]["entries"]] == ["source.txt"]
    listing = next(event["entries"] for event in events if "entries" in event and "local" not in event)
    assert listing[0]["name"] == "Büro #1.txt"
    assert next(event["entry"] for event in events if "entry" in event)["size"] == len(payload)
    assert next(event["checksum"] for event in events if "checksum" in event) == hashlib.sha256(payload).hexdigest()
    assert next(event["permissions"] for event in events if "permissions" in event) == "0640"
    assert "fixture-only" not in result.stdout + result.stderr
    assert not (remote_root / "script round trip").exists()
    script.write_text('open "Reference site"\nmkdir should-not-run\ninval id\n', encoding="utf-8")
    result = subprocess.run(args, env=environment, timeout=15, capture_output=True,
                            encoding="utf-8", creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
    assert result.returncode == 1 and not (remote_root / "should-not-run").exists()
    script.write_text('open "arco://session/Reference%20site?path=%2F"\npwd\n', encoding="utf-8")
    result = subprocess.run(args, env=environment, check=True, timeout=15, capture_output=True,
                            encoding="utf-8", creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
    assert json.loads(result.stdout.splitlines()[0])["event"]["open"] == "Reference site"
    digest = environment["ARCO_SFTP_TEST_FINGERPRINT"].removeprefix("SHA256:")
    digest = digest.replace("+", "-").replace("/", "_")
    address = environment["ARCO_SFTP_TEST_URL"].removeprefix("sftp://")
    script.write_text(f'open "winscp-sftp://arco;fingerprint=ssh-rsa-{digest}@{address}"\npwd\n', encoding="utf-8")
    result = subprocess.run(args, env=environment, check=True, timeout=15, capture_output=True,
                            encoding="utf-8", creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
    assert json.loads(result.stdout.splitlines()[-1])["event"]["remote"] == "/"
    file_name = "Grüße #2.txt"
    (remote_root / file_name).write_bytes(payload)
    file_path = "/" + quote(file_name)
    profile_url = "arco://session/Reference%20site?" + urlencode({"path": file_path})
    script.write_text(f'open "{profile_url}"\npwd\n', encoding="utf-8")
    result = subprocess.run(args, env=environment, check=True, timeout=15, capture_output=True,
                            encoding="utf-8", creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
    events = [json.loads(line)["event"] for line in result.stdout.splitlines()]
    assert events[0]["file"] == file_name and events[1]["remote"] == "/"
    direct_url = f"winscp-sftp://arco;fingerprint=ssh-rsa-{digest}@{address.rstrip('/')}{file_path}"
    script.write_text(f'lcd "{local}"\nget-url "{direct_url}" from-url.txt\n', encoding="utf-8")
    for expected in (0, 1):
        result = subprocess.run(args, env=environment, timeout=15, capture_output=True,
                                encoding="utf-8", creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
        assert result.returncode == expected, result.stderr
        assert (local / "from-url.txt").read_bytes() == payload
    print("File URLs resolved Unicode names and downloaded without replacing a local file")
    kex_url = environment["ARCO_GSS_KEX_ONLY_URL"]
    pin = urlencode({"fingerprint": environment["ARCO_SFTP_TEST_FINGERPRINT"]})
    script.write_text(f'open "{kex_url}?{pin}"\n', encoding="utf-8")
    result = subprocess.run(args, env=environment, timeout=15, capture_output=True,
                            encoding="utf-8", creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
    assert result.returncode == 1 and "bietet GSSAPI-Schlüsselaustausch an" in json.loads(result.stderr)["error"]
    print("An SSH server offering only GSSAPI key exchange was diagnosed before login")
    config = json.loads(profile.read_text(encoding="utf-8"))
    config["ftp_profiles"]["Reference site"].update(
        ssh_kerberos="host/fixture.invalid", ssh_kerberos_kex=True,
    )
    profile.write_text(json.dumps(config), encoding="utf-8")
    script.write_text('open "Reference site"\n', encoding="utf-8")
    result = subprocess.run(args, env=environment, timeout=15, capture_output=True,
                            encoding="utf-8", creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
    assert result.returncode == 1 and "gss-group14-sha256" in json.loads(result.stderr)["error"]
    print("A profile requiring GSSAPI did not fall back to ordinary SSH key exchange")
    settings = config["ftp_profiles"]["Reference site"]
    settings["address"] = environment["ARCO_GSS_KEX_NULL_URL"]
    profile.write_text(json.dumps(config), encoding="utf-8")
    result = subprocess.run(args, env=environment, timeout=15, capture_output=True,
                            encoding="utf-8", creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
    assert result.returncode == 1 and "verlangt den hinterlegten Fingerabdruck" in json.loads(result.stderr)["error"], result.stderr
    settings.update(ssh_fingerprint="", ssh_kerberos="")
    profile.write_text(json.dumps(config), encoding="utf-8")
    result = subprocess.run(args, env=environment, timeout=15, capture_output=True,
                            encoding="utf-8", creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
    assert result.returncode == 1 and "Dienstprinzipal ausdrücklich an" in json.loads(result.stderr)["error"], result.stderr
    print("Kerberos-only host trust requires an explicit service principal and never bypasses a pin")
    print("SFTP script parsed before transfer, copied both ways, and produced JSON events")


def reference_sync(executable, work, root, environment):
    local = work / "local sync"
    local.mkdir()
    remote = root / "script sync"
    remote.mkdir()
    (local / "upload.txt").write_bytes(b"local")
    (remote / "download.txt").write_bytes(b"remote")
    profile = work / "sync-profile.json"
    settings = {
        "address": environment["ARCO_SFTP_TEST_URL"] + "script%20sync/",
        "security": "sftp",
        "user": "arco",
        "ssh_fingerprint": environment["ARCO_SFTP_TEST_FINGERPRINT"],
        "transfer": {"masks": "*.txt | secret*.txt"},
    }

    presets = []

    def run(*steps, expected=0):
        profile.write_text(json.dumps({"ftp_profiles": {"Sync": settings},
                                       "network_transfer_presets": presets}), encoding="utf-8")
        result = subprocess.run(
            [str(executable.resolve()), "--profile", str(profile), "--script", "-",
             "--password-env", "ARCO_REFERENCE_SCRIPT_PASSWORD"],
            input="\n".join([f'lcd "{local}"', "open Sync", *steps]) + "\n",
            env=dict(environment, ARCO_REFERENCE_SCRIPT_PASSWORD="fixture-only",
                     ARCO_TEST_AGE="synthetic-encryption-fixture",
                     ARCO_TEST_WRONG_AGE="wrong-key"),
            timeout=45, capture_output=True, encoding="utf-8",
            creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
        )
        assert result.returncode == expected, result.stderr
        if expected:
            return json.loads(result.stderr)["error"]
        return [json.loads(line)["event"] for line in result.stdout.splitlines()][-1]

    event = run("synchronize both -preview")
    assert {(row["path"], row["action"]) for row in event["changes"]} == {
        ("upload.txt", "upload"), ("download.txt", "download"),
    }, event
    assert not (local / "download.txt").exists() and not (remote / "upload.txt").exists()
    assert run("synchronize both")["completed"] == 2
    assert (remote / "upload.txt").read_bytes() == b"local"
    assert (local / "download.txt").read_bytes() == b"remote"
    (remote / "orphan.txt").write_bytes(b"remote only")
    (remote / "secret.txt").write_bytes(b"excluded by preset")
    event = run("synchronize remote -delete -preview")
    assert event["changes"] == [{"path": "orphan.txt", "action": "delete-remote"}], event
    run("synchronize remote -delete")
    assert not (remote / "orphan.txt").exists() and (remote / "secret.txt").is_file()
    (local / "orphan.txt").write_bytes(b"local only")
    assert run("synchronize local -delete -preview")["changes"] == [
        {"path": "orphan.txt", "action": "delete-local"},
    ]
    run("synchronize local -delete")
    assert not (local / "orphan.txt").exists()
    (local / "upload.txt").write_bytes(b"alter")
    run("synchronize remote -criteria=checksum")
    assert (remote / "upload.txt").read_bytes() == b"alter"
    print("Script synchronization compared content, previewed both directions and deleted only reviewed entries")

    settings["transfer"].update(speed_kib=16, preserve_time=True)
    payload = b"arco" * 6144
    source = local / "limited.txt"
    source.write_bytes(payload)
    stamp = 1_700_000_031
    os.utime(source, (stamp, stamp))
    start = time.monotonic()
    run("put limited.txt", "get limited.txt copy.txt")
    assert time.monotonic() - start >= 2.5
    assert (local / "copy.txt").read_bytes() == payload
    assert int((local / "copy.txt").stat().st_mtime) == stamp
    assert int((remote / "limited.txt").stat().st_mtime) == stamp
    assert "Transfermaske" in run("get secret.txt", expected=1)
    print("Profile rate limit, timestamp and transfer mask passed against SFTP")

    # Use different payloads of the same size. A timestamp-only run must preserve both.
    pair_left, pair_right = local / "stamp.txt", remote / "stamp.txt"
    pair_left.write_bytes(b"local")
    pair_right.write_bytes(b"other")
    os.utime(pair_left, (stamp + 40, stamp + 40))
    os.utime(pair_right, (stamp, stamp))
    (local / "only.txt").write_bytes(b"left only")
    assert run("synchronize both -timestamps -filemask=stamp.txt -preview")["changes"] == [
        {"path": "stamp.txt", "action": "timestamp-remote"},
    ]
    assert run("synchronize both -timestamps -filemask=stamp.txt")["completed"] == 1
    assert int(pair_right.stat().st_mtime) == stamp + 40 and pair_right.read_bytes() == b"other"
    os.utime(pair_right, (stamp, stamp))
    assert run("synchronize local -timestamps -filemask=stamp.txt")["completed"] == 1
    assert int(pair_left.stat().st_mtime) == stamp and pair_left.read_bytes() == b"local"
    assert not (remote / "only.txt").exists()
    assert run("synchronize both -timestamps -filemask=stamp.txt -preview")["changes"] == []
    pair_right.write_bytes(b"different size")
    os.utime(pair_right, (stamp - 40, stamp - 40))
    assert run("synchronize both -timestamps -filemask=stamp.txt -preview")["changes"] == [
        {"path": "stamp.txt", "action": "timestamp-skipped"},
    ]
    assert run("synchronize both -timestamps -all-sizes -filemask=stamp.txt")["completed"] == 1
    assert int(pair_right.stat().st_mtime) == stamp and pair_right.read_bytes() == b"different size"
    print("SFTP timestamp-only synchronization changed both sides without copying file data")

    sub = remote / "rules"
    sub.mkdir()
    (sub / "allowed.dat").write_bytes(b"yes")
    (sub / "manual.txt").write_bytes(b"manual")
    (remote / "allowed.dat").write_bytes(b"wrong directory")
    presets.extend([
        {"name": "Automatic", "settings": {"masks": "*.dat"},
         "rule": {"host": "127.0.0.1", "user": "arco", "remote": "/script sync/rules",
                  "local": local.as_posix()}},
        {"name": "Manual copy", "settings": {"masks": "*.txt"}},
        {"name": "Fallback", "settings": {"masks": "*.txt | secret*.txt"}, "rule": {}},
    ])
    assert "Transfermaske" in run("get allowed.dat", expected=1)
    run("cd rules", "get allowed.dat")
    assert (local / "allowed.dat").read_bytes() == b"yes"
    assert "Transfermaske" in run("cd rules", "get manual.txt", expected=1)
    run("cd rules", 'get manual.txt "-preset=Manual copy"')
    assert (local / "manual.txt").read_bytes() == b"manual"
    assert "fehlt" in run("cd rules", "get manual.txt -preset=missing", expected=1)
    settings["transfer_preset"] = "Manual copy"
    assert "Transfermaske" in run("cd rules", "get allowed.dat", expected=1)
    run("cd rules", "get manual.txt fixed.txt")
    assert (local / "fixed.txt").read_bytes() == b"manual"
    print("Named SFTP presets observed all rule fields, priority, directory changes and explicit selection")

    presets.clear()
    settings.pop("transfer_preset")
    settings["transfer"] = {"masks": "*.txt"}
    inputs = local / "batch source"
    outputs = local / "batch result"
    newest = local / "newest result"
    for folder in (inputs, outputs, newest):
        folder.mkdir()
    (inputs / "first.txt").write_bytes(b"alpha")
    (inputs / "second.txt").write_bytes(b"beta")
    (inputs / "ignored.bin").write_bytes(b"skip")
    os.utime(inputs / "first.txt", (stamp, stamp))
    os.utime(inputs / "second.txt", (stamp + 80, stamp + 80))
    result = run("mkdir inbox", 'put "batch source/*.txt" "batch source/first.txt" inbox/')
    assert result == {"uploaded": ["first.txt", "second.txt"], "completed": 2}, result
    assert sorted(p.name for p in (remote / "inbox").iterdir()) == ["first.txt", "second.txt"]
    (outputs / "second.txt").write_bytes(b"keep")
    assert "Transferziel" in run('get inbox/first.txt inbox/second.txt "batch result/"', expected=1)
    assert not (outputs / "first.txt").exists() and (outputs / "second.txt").read_bytes() == b"keep"
    (outputs / "second.txt").unlink()
    result = run('get inbox/first.txt inbox/second.txt "batch result/"')
    assert result["completed"] == 2
    assert (outputs / "first.txt").read_bytes() == b"alpha"
    assert (outputs / "second.txt").read_bytes() == b"beta"
    assert run('get -latest inbox/*.txt "newest result/"')["completed"] == 1
    assert [p.name for p in newest.iterdir()] == ["second.txt"]
    assert run('get inbox/*.bin "newest result/"')["completed"] == 0
    assert "Keine übertragbare Datei" in run('get -failonnomatch inbox/*.bin "newest result/"', expected=1)
    assert "Keine übertragbare Datei" in run('option failonnomatch on', 'mget inbox/*.bin "newest result/"', expected=1)
    assert run('option failonnomatch on', 'option failonnomatch off', 'recv inbox/*.bin "newest result/"')["completed"] == 0
    (inputs / "resume.txt").write_bytes(b"resume fixture" * 2048)
    run('put -resume "batch source/resume.txt"')
    run('get -resume resume.txt "newest result/"')
    assert (newest / "resume.txt").read_bytes() == (inputs / "resume.txt").read_bytes()
    print("SFTP batch transfer checked masks, deduplication, target preflight, and latest selection")

    upload_tree = local / "sample tree"
    (upload_tree / "nested" / "empty").mkdir(parents=True)
    (upload_tree / "nested" / "report.txt").write_bytes(b"recursive payload")
    (upload_tree / "nested" / "exclude.bin").write_bytes(b"remove from selection")
    downloads = local / "trees"
    downloads.mkdir()
    assert run('put "sample tree" inbox/ -filemask=*.txt')["completed"] == 4
    remote_tree = remote / "inbox" / "sample tree"
    assert (remote_tree / "nested/empty").is_dir()
    assert (remote_tree / "nested/report.txt").read_bytes() == b"recursive payload"
    assert not (remote_tree / "nested/exclude.bin").exists()
    listed = run('dir "/script sync/inbox/sample tree/nested/*.txt"')["entries"]
    assert [entry["name"] for entry in listed] == ["report.txt"]
    assert run("ls missing*.txt")["entries"] == []
    assert "keine Treffer" in run("option failonnomatch on", "ls missing*.txt", expected=1)
    assert run('get "inbox/sample tree" trees/')["completed"] == 4
    downloaded_tree = downloads / "sample tree"
    assert (downloaded_tree / "nested/empty").is_dir()
    assert (downloaded_tree / "nested/report.txt").read_bytes() == b"recursive payload"
    (local / "wild trees").mkdir()
    assert run('get -recursive "inbox/sample*" "wild trees/"')["completed"] == 4
    assert (local / "wild trees/sample tree/nested/report.txt").read_bytes() == b"recursive payload"
    with (remote_tree / "nested" / "late.txt").open("wb") as file:
        file.write(b"must not be published after a preflight collision")
    assert "Transferziel" in run('get "inbox/sample tree" trees/', expected=1)
    assert not (downloaded_tree / "nested/late.txt").exists()

    settings["recycle_remote"] = True
    event = run("cd inbox", 'rmdir "sample tree"')
    slot = event["slot"]
    recycle = remote / ".arco-trash" / slot / "sample tree"
    assert not remote_tree.exists() and (recycle / "nested/report.txt").is_file()
    assert any(record["name"] == slot for record in run("trash")["slots"])
    remote_tree.mkdir()
    assert "geändert" in run(f"trash restore {slot} inbox", expected=1)
    assert (recycle / "nested/report.txt").is_file()
    remote_tree.rmdir()
    assert run(f"trash restore {slot} inbox")["restored"] == "sample tree"
    assert (remote_tree / "nested/report.txt").read_bytes() == b"recursive payload"
    assert not recycle.exists()
    (remote / "permanent.txt").write_bytes(b"delete")
    assert run("rm -permanent permanent.txt") == {"removed": "permanent.txt"}
    assert not (remote / "permanent.txt").exists()
    (local / "mirror local").mkdir()
    (remote / "mirror target").mkdir()
    (remote / "mirror target/drop.txt").write_bytes(b"mirror recycled")
    assert run('synchronize remote -delete "mirror local" "/script sync/mirror target"')["completed"] == 1
    assert not (remote / "mirror target/drop.txt").exists()
    assert any(path.read_bytes() == b"mirror recycled" for path in (remote / ".arco-trash").glob("*/drop.txt"))
    print("Recursive SFTP transfers and remote recycle restoration passed")

    settings.update(recycle_remote=False, recycle_overwrites=True)
    overwrite_source, overwrite_target = local / "overwrite source", remote / "overwrite target"
    overwrite_source.mkdir()
    overwrite_target.mkdir()
    (overwrite_source / "overwrite.txt").write_bytes(b"new transfer")
    (overwrite_target / "overwrite.txt").write_bytes(b"retained previous version")
    sync = 'synchronize remote -mirror -criteria=checksum "overwrite source" "/script sync/overwrite target"'
    assert run(f"{sync} -filemask=overwrite.txt")["completed"] == 1
    saved, = (remote / ".arco-trash").glob("*/overwrite.txt")
    assert saved.read_bytes() == b"retained previous version"
    assert (overwrite_target / "overwrite.txt").read_bytes() == b"new transfer"
    assert run(f"trash restore {saved.parent.name} inbox")["restored"] == "overwrite.txt"
    assert (remote / "inbox/overwrite.txt").read_bytes() == b"retained previous version"
    assert (overwrite_target / "overwrite.txt").read_bytes() == b"new transfer"
    failed = "publication-blocked.txt"
    (overwrite_source / failed).write_bytes(b"staged")
    (overwrite_target / failed).write_bytes(b"kept after rename failed")
    run(f"{sync} -filemask={failed}", expected=1)
    assert (overwrite_target / failed).read_bytes() == b"kept after rename failed"
    assert not list((remote / ".arco-trash").glob(f"*/{failed}"))
    assert not list(overwrite_target.glob("arco-upload-*.part"))
    blocked = "recycle-bin-blocked.txt"
    (overwrite_source / blocked).write_bytes(b"new data")
    (overwrite_target / blocked).write_bytes(b"keep existing target")
    bin_path, hidden_bin = remote / ".arco-trash", remote / "moved recycle bin"
    bin_path.rename(hidden_bin)
    bin_path.write_bytes(b"occupied")
    try:
        assert "belegt" in run(f"{sync} -filemask={blocked}", expected=1)
        assert (overwrite_target / blocked).read_bytes() == b"keep existing target"
        assert not list(overwrite_target.glob("arco-upload-*.part"))
    finally:
        bin_path.unlink()
        hidden_bin.rename(bin_path)
    print("SFTP overwrites retained an independent version and restored the previous file after a publication failure")

    name = "private datei.txt"
    payload = b"Confidential synthetic reference data. " * 2_300
    (local / name).write_bytes(payload)
    encrypted = run(f'put-encrypted "{name}" ARCO_TEST_AGE')
    package = remote / encrypted["uploaded"]
    assert encrypted["encrypted"] and package.name.endswith(".arco.age") and name not in package.name
    ciphertext = package.read_bytes()
    assert ciphertext.startswith(b"age-encryption.org/v1\n")
    assert name.encode() not in ciphertext and payload[:50] not in ciphertext
    assert not (remote / name).exists()
    (local / "decoded").mkdir()
    get = f"get-encrypted {package.name}"
    assert "Passphrase" in run(f"{get} ARCO_TEST_WRONG_AGE decoded", expected=1)
    assert not list((local / "decoded").iterdir())
    package.write_bytes(ciphertext[:-1] + bytes([ciphertext[-1] ^ 1]))
    run(f"{get} ARCO_TEST_AGE decoded", expected=1)
    assert not list((local / "decoded").iterdir())
    package.write_bytes(ciphertext)
    assert run(f"{get} ARCO_TEST_AGE decoded")["encrypted"]
    assert (local / "decoded" / name).read_bytes() == payload
    run(f"{get} ARCO_TEST_AGE decoded", expected=1)
    assert (local / "decoded" / name).read_bytes() == payload
    print("Encrypted SFTP package preserved the original name and rejected wrong passwords, altered data and existing outputs")


def reference_script_watch(executable, work, root, environment):
    import queue

    local, remote = work / "watch script", root / "watch script"
    local.mkdir()
    remote.mkdir()
    for folder in (local, remote):
        (folder / "initial.txt").write_bytes(b"confirmed file")
    (remote / "foreign.txt").write_bytes(b"keep on server")
    profile = work / "watch-profile.json"
    profile.write_text(json.dumps({"ftp_profiles": {"Watch": {
        "address": environment["ARCO_SFTP_TEST_URL"],
        "security": "sftp", "user": "arco", "recycle_remote": True,
        "ssh_fingerprint": environment["ARCO_SFTP_TEST_FINGERPRINT"],
    }}}), encoding="utf-8")
    command = [str(executable.resolve()), "--profile", str(profile), "--script", "-",
               "--password-env", "ARCO_REFERENCE_SCRIPT_PASSWORD"]
    with subprocess.Popen(
        command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        env=dict(environment, ARCO_REFERENCE_SCRIPT_PASSWORD="fixture-only"), encoding="utf-8",
        creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
    ) as process:
        events = queue.Queue()

        def read():
            for line in process.stdout:
                events.put(json.loads(line)["event"])
            events.put(None)

        reader = threading.Thread(target=read, daemon=True)
        reader.start()
        process.stdin.write(f'open Watch\nkeepuptodate -interval=2 -duration=7 -delete '
                            f'-filemask=*.txt "{local}" "/watch script"\necho returned\n')
        process.stdin.close()
        try:
            first = events.get(timeout=10)
            assert first.get("open") == "Watch", first
            started = events.get(timeout=10)
            assert started["keepuptodate"] == "started", started
            (local / "new.txt").write_bytes(b"watch publication")
            (local / "ignored.bin").write_bytes(b"excluded")
            (local / "initial.txt").unlink()
            completed = []
            while (event := events.get(timeout=20)) is not None:
                completed.append(event)
            assert process.wait(timeout=5) == 0, process.stderr.read()
            assert any(event.get("uploaded") == 1 and event.get("deleted") == 1 for event in completed), completed
            assert completed[-2:] == [{"keepuptodate": "completed"}, {"message": "returned"}]
        finally:
            if process.poll() is None:
                process.kill()
            reader.join(timeout=5)
    assert (remote / "new.txt").read_bytes() == b"watch publication"
    assert (remote / "foreign.txt").read_bytes() == b"keep on server"
    assert not (remote / "initial.txt").exists() and not (remote / "ignored.bin").exists()
    assert any(path.read_bytes() == b"confirmed file" for path in (root / ".arco-trash").glob("*/initial.txt"))
    print("Script watch published the mask match, recycled the confirmed removal, and continued after its timer")

    (local / "before-start.txt").write_bytes(b"initial sync")
    initial = subprocess.run(
        command, input=f'open Watch\nkeepuptodate -initial-sync -duration=1 -filemask=*.txt '
                       f'"{local}" "/watch script"\n',
        env=dict(environment, ARCO_REFERENCE_SCRIPT_PASSWORD="fixture-only"),
        capture_output=True, encoding="utf-8", timeout=20,
        creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
    )
    assert initial.returncode == 0, initial.stderr
    assert (remote / "before-start.txt").read_bytes() == b"initial sync"
    assert (remote / "foreign.txt").read_bytes() == b"keep on server"
    print("Script watch published existing source files during its optional initial comparison")


class Login(paramiko.ServerInterface):
    def __init__(self, server):
        self.server = server
        self.forwarded_channels = set()
        self.key_accepted = False
        self.round = 0
        self.terminal_sizes = {}

    def get_allowed_auths(self, username):
        if username == "interactive" or (username == "mfa" and self.key_accepted):
            return "keyboard-interactive"
        if username == "mfa":
            return "publickey"
        return "password,publickey"

    def check_auth_password(self, username, password):
        self.server.auth_attempts.append(username)
        if (username, password) == ("arco", "fixture-only"):
            return paramiko.AUTH_SUCCESSFUL
        return paramiko.AUTH_FAILED

    def check_auth_publickey(self, username, key):
        self.server.auth_attempts.append(username)
        if username == "mfa" and key in self.server.client_keys:
            self.key_accepted = True
            return paramiko.AUTH_PARTIALLY_SUCCESSFUL
        if username == self.server.user and key in self.server.client_keys:
            return paramiko.AUTH_SUCCESSFUL
        return paramiko.AUTH_FAILED

    def check_auth_interactive(self, username, submethods):
        if username != "interactive" and (username != "mfa" or not self.key_accepted):
            return paramiko.AUTH_FAILED
        self.server.auth_attempts.append(username)
        self.round = 1
        return paramiko.InteractiveQuery("Fixture", "Login for the test account", ("Password", False))

    def check_auth_interactive_response(self, responses):
        if self.round == 1 and responses == ["fixture-password"]:
            self.round = 2
            return paramiko.InteractiveQuery(
                "Fixture MFA", "", ("Account", True), ("One-time code", False),
            )
        if self.round == 2 and responses == ["test account", "123456"]:
            return paramiko.AUTH_SUCCESSFUL
        return paramiko.AUTH_FAILED

    def check_channel_exec_request(self, channel, command):
        if self.server.allow_terminal and channel.get_id() in self.terminal_sizes:
            try:
                parts = shlex.split(command.decode("utf-8"))
                program = shlex.split(parts[3]) if parts[:3] == ["exec", "/bin/sh", "-c"] else []
            except (ValueError, UnicodeDecodeError, IndexError):
                return False
            if program != ["cd", "/Büro's Ablage", "&&", "exec", "${SHELL:-/bin/sh}", "-i"]:
                return False
            return self.check_channel_shell_request(channel)
        if not self.server.allow_scp:
            return False
        threading.Thread(target=serve_scp, args=(self.server.root, channel, command), daemon=True).start()
        return True

    def check_channel_pty_request(self, channel, term, width, height, *unused):
        if not self.server.allow_terminal or (term, width, height) != (b"xterm", 80, 24):
            return False
        self.terminal_sizes[channel.get_id()] = (width, height)
        return True

    def check_channel_window_change_request(self, channel, width, height, *unused):
        if channel.get_id() not in self.terminal_sizes:
            return False
        self.terminal_sizes[channel.get_id()] = (width, height)
        return True

    def check_channel_shell_request(self, channel):
        if channel.get_id() not in self.terminal_sizes:
            return False

        def echo():
            with channel:
                channel.settimeout(4)
                data = b""
                while not data.endswith(b"\r"):
                    part = channel.recv(1024)
                    if not part:
                        return
                    data += part
                width, height = self.terminal_sizes[channel.get_id()]
                channel.sendall(f"PTY {width}×{height}: ".encode() + data)
                channel.send_exit_status(0)

        threading.Thread(target=echo, daemon=True).start()
        return True

    def check_channel_request(self, kind, channel_id):
        if kind == "session" and self.server.forward is None:
            return paramiko.OPEN_SUCCEEDED
        return paramiko.OPEN_FAILED_ADMINISTRATIVELY_PROHIBITED

    def check_channel_direct_tcpip_request(self, channel_id, origin, destination):
        if destination == self.server.forward:
            self.forwarded_channels.add(channel_id)
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
        self.login = server

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
        handle = (
            DropOnce(flags, self.login, path.name == "sftp-reconnect.bin")
            if path.name == "sftp-reconnect.bin" or path.name.endswith(".filepart")
            else paramiko.SFTPHandle(flags)
        )
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
        if self.local(before).name.startswith("arco-upload-") and self.local(after).name == "publication-blocked.txt":
            return paramiko.SFTP_PERMISSION_DENIED
        self.local(before).rename(self.local(after))
        return paramiko.SFTP_OK

    @sftp_operation
    def chattr(self, remote, attrs):
        paramiko.SFTPServer.set_file_attr(str(self.local(remote)), attrs)
        return paramiko.SFTP_OK


class DropOnce(paramiko.SFTPHandle):
    def __init__(self, flags, login, interrupt_read):
        super().__init__(flags)
        self.login = login
        self.interrupt_read = interrupt_read

    def read(self, offset, length):
        if self.interrupt_read and offset >= 96 * 1024 and not self.login.server.interrupted:
            self.login.server.interrupted = True
            self.login.connection.close()
            return paramiko.SFTP_EOF
        return super().read(offset, length)

    def write(self, offset, data):
        result = super().write(offset, data)
        if not self.interrupt_read and offset >= 96 * 1024 and not self.login.server.interrupted_upload:
            self.login.server.interrupted_upload = True
            self.login.connection.close()
        return result


class BinaryHandles(paramiko.SFTPServer):
    def _send_handle_response(self, request_number, handle, folder=False):
        if not isinstance(handle, paramiko.SFTPHandle):
            return super()._send_handle_response(request_number, handle, folder)
        token = self.next_handle.to_bytes(8, "big") + b"\xff"
        self.next_handle += 1
        handle._set_name(token)
        table = self.folder_table if folder else self.file_table
        table[token] = handle
        self._response(request_number, paramiko.sftp.CMD_HANDLE, token)


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
            for key in self.server.extra_host_keys:
                connection.add_server_key(key)
            if self.server.forward is None and not self.server.allow_scp:
                connection.set_subsystem_handler("sftp", BinaryHandles, Files, root=self.server.root)
            login = Login(self.server)
            login.connection = connection
            try:
                connection.start_server(server=login)
            except (EOFError, OSError, paramiko.SSHException):
                return
            while connection.is_active():
                channel = connection.accept(5)
                if channel and channel.get_id() in login.forwarded_channels:
                    with channel:
                        forward(channel, self.server.forward)
                elif channel:
                    while connection.is_active() and not channel.closed:
                        time.sleep(0.1)


def bridge(source, destination):
    try:
        while data := source.recv(32768):
            destination.sendall(data)
    except OSError:
        pass
    finally:
        try:
            destination.shutdown(socket.SHUT_WR)
        except OSError:
            pass


def forward(client, address, ready=b""):
    with socket.create_connection(address, timeout=5) as target:
        target.settimeout(None)
        client.sendall(ready)
        relay = threading.Thread(target=bridge, args=(target, client), daemon=True)
        relay.start()
        bridge(client, target)
        try:
            target.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        relay.join(5)


class Proxy(socketserver.BaseRequestHandler):
    def handle(self):
        self.request.settimeout(5)
        port = self.server.forward[1]
        credentials = getattr(self.server, "credentials", None)
        with self.request.makefile("rb") as data:
            if self.server.scheme == "http":
                assert data.readline(8192) == f"CONNECT 127.0.0.1:{port} HTTP/1.1\r\n".encode()
                assert data.readline(8192) == f"Host: 127.0.0.1:{port}\r\n".encode()
                header = data.readline(8192)
                if credentials:
                    expected = b"Proxy-Authorization: Basic " + base64.b64encode(b":".join(credentials)) + b"\r\n"
                    if header != expected:
                        self.request.sendall(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n")
                        return
                    header = data.readline(8192)
                assert header == b"\r\n"
                ready = b"HTTP/1.1 200 Connection established\r\n\r\n"
            else:
                method = b"\x02" if credentials else b"\x00"
                assert data.read(3) == b"\x05\x01" + method
                self.request.sendall(b"\x05" + method)
                if credentials:
                    version, user_length = data.read(2)
                    user = data.read(user_length)
                    password_length = data.read(1)[0]
                    password = data.read(password_length)
                    if version != 1 or (user, password) != credentials:
                        self.request.sendall(b"\x01\x01")
                        return
                    self.request.sendall(b"\x01\x00")
                prefix = b"\x05\x01\x00\x03\x0b"
                assert data.read(len(prefix)) == prefix
                assert data.read(11) == b"ssh.invalid"
                assert data.read(2) == port.to_bytes(2, "big")
                ready = b"\x05\x00\x00\x01\x00\x00\x00\x00\x00\x00"
        self.request.settimeout(None)
        forward(self.request, self.server.forward, ready)


class GssKexOnly(socketserver.BaseRequestHandler):
    def handle(self):
        # Send only identification and KEXINIT. This peer never reaches SSH authentication.
        self.request.settimeout(3)
        self.request.sendall(b"SSH-2.0-ArcoKexReference\r\n")
        algorithms = (
            b"gss-group14-sha256-toWM5Slw5Ew8Mqkay+al2g==", getattr(self.server, "gss_host_key", b"ssh-ed25519"),
            b"aes128-ctr", b"aes128-ctr", b"hmac-sha2-256", b"hmac-sha2-256",
            b"none", b"none", b"", b"",
        )
        payload = b"\x14" + bytes(16) + b"".join(
            struct.pack(">I", len(name)) + name for name in algorithms
        ) + bytes(5)
        padding = 8 - ((len(payload) + 5) % 8)
        if padding < 4:
            padding += 8
        self.request.sendall(struct.pack(">IB", len(payload) + padding + 1, padding) + payload + bytes(padding))
        try:
            while self.request.recv(8192):
                pass
        except (TimeoutError, ConnectionError):
            pass


@contextmanager
def loopback(handler, **settings):
    with Server(("127.0.0.1", 0), handler) as server:
        vars(server).update(settings)
        server.auth_attempts = []
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            yield server
        finally:
            server.shutdown()
            thread.join()


def peer(host_key, client_keys, *, user="arco", root=None, forward=None, extra_host_keys=(), allow_scp=False, allow_terminal=False):
    return loopback(
        Peer, root=root, forward=forward, host_key=host_key, client_keys=client_keys,
        user=user, extra_host_keys=extra_host_keys, interrupted=False, interrupted_upload=False,
        allow_scp=allow_scp, allow_terminal=allow_terminal,
    )


def serve_scp(root, channel, command):
    """Expose only the SCP operations and fixed publication commands used by the fixture."""
    def local(argument):
        path = root.joinpath(*argument.split("/")).resolve()
        assert path.is_relative_to(root)
        return path

    def bridge_scp(args):
        executable = Path(os.environ["SystemRoot"]) / "System32" / "OpenSSH" / "scp.exe"
        with subprocess.Popen(
            [str(executable), args[1], "--", str(local(args[3]))],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
        ) as process:
            def relay_input():
                try:
                    while data := channel.recv(32768):
                        process.stdin.write(data)
                        process.stdin.flush()
                except (OSError, EOFError):
                    pass
                finally:
                    process.stdin.close()
            relay = threading.Thread(target=relay_input, daemon=True)
            relay.start()
            while data := process.stdout.read1(32768):
                channel.sendall(data)
            return process.wait(timeout=10)

    # Paramiko sends the channel-request success when the exec callback returns.
    time.sleep(0.02)
    status = 1
    try:
        parts = shlex.split(command.decode("utf-8"))
        if len(parts) == 4 and parts[0] == "scp" and parts[1] in ("-t", "-f") and parts[2] == "--":
            status = bridge_scp(parts)
        elif parts[:3] == ["umask", "077;", "mkdir"] and parts[3] == "--" and len(parts) == 5:
            local(parts[4]).mkdir()
            status = 0
        elif len(parts) == 3 and parts[:2] == ["mkdir", "--"]:
            local(parts[2]).mkdir()
            status = 0
        elif len(parts) == 9 and parts[:2] == ["LC_ALL=C", "find"] and parts[3:8] == ["-mindepth", "1", "-maxdepth", "1", "-printf"]:
            assert parts[8] == r"%f\0%y\0%s\0%T@\0%i\0"
            for item in local(parts[2]).iterdir():
                info = item.stat()
                kind = "d" if item.is_dir() else "f"
                channel.sendall(f"{item.name}\0{kind}\0{info.st_size}\0{info.st_mtime:.6f}\0{info.st_ino}\0".encode())
            status = 0
        elif len(parts) == 8 and parts[:5] == ["touch", "-h", "-c", "-m", "-d"] and parts[6] == "--":
            file = local(parts[7])
            if file.exists() and not file.is_symlink():
                os.utime(file, (file.stat().st_atime, int(parts[5].removeprefix("@"))))
            status = 0
        elif len(parts) == 16 and parts[:4] == ["mv", "-T", "-n", "--"]:
            local(parts[4]).rename(local(parts[5]))
            status = 0
        elif len(parts) == 3 and parts[:2] == ["rm", "--"]:
            local(parts[2]).unlink()
            status = 0
        elif len(parts) == 3 and parts[:2] == ["rmdir", "--"]:
            local(parts[2]).rmdir()
            status = 0
        elif len(parts) == 9 and parts[:3] == ["test", "!", "-e"] and parts[4:8] == ["&&", "test", "!", "-L"]:
            status = int(local(parts[3]).exists() or local(parts[8]).is_symlink())
        elif len(parts) == 14 and parts[:2] == ["test", "-f"] and parts[9:12] == ["ln", "-T", "--"]:
            os.link(local(parts[12]), local(parts[13]))
            status = 0
        elif len(parts) == 8 and parts[:3] == ["rm", "-f", "--"] and parts[4:7] == ["&&", "rmdir", "--"]:
            local(parts[3]).unlink(missing_ok=True)
            local(parts[7]).rmdir()
            status = 0
        else:
            raise AssertionError(f"Unexpected test command: {parts}")
    except OSError:
        pass
    finally:
        channel.send_exit_status(status)
        channel.close()


def openssh_key(path, key, parser, password=None):
    encryption = NoEncryption() if password is None else BestAvailableEncryption(password.encode())
    path.write_bytes(key.private_bytes(Encoding.PEM, PrivateFormat.OpenSSH, encryption))
    return parser.from_private_key_file(str(path), password=password)


def properties(server, prefix):
    fingerprint = base64.b64encode(hashlib.sha256(server.host_key.asbytes()).digest())
    return {
        f"{prefix}URL": f"sftp://127.0.0.1:{server.server_address[1]}/",
        f"{prefix}FINGERPRINT": "SHA256:" + fingerprint.decode("ascii").rstrip("="),
    }


def main():
    logging.getLogger("paramiko.transport").setLevel(logging.CRITICAL)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--harness", type=Path, help="Already built arco-windows lib test harness")
    parser.add_argument("--app-harness", type=Path, help="Already built arco-commander unit test harness")
    parser.add_argument("--net-exe", type=Path, help="Already built arco-net console executable")
    parser.add_argument("--backend-only", action="store_true", help="Run the arco-windows reference tests only")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="arco-native-sftp-") as fixture:
        work = Path(fixture).resolve()
        root = work / "files"
        root.mkdir()
        with ExitStack() as peers:
            key = work / "id_rsa"
            rsa_key = paramiko.RSAKey.generate(2048)
            rsa_key.write_private_key_file(str(key))
            ed_path = work / "id_ed25519"
            ed_key = openssh_key(ed_path, ed25519.Ed25519PrivateKey.generate(), paramiko.Ed25519Key)
            ec_path = work / "id_ecdsa"
            ec_key = openssh_key(
                ec_path, ec.generate_private_key(ec.SECP256R1()), paramiko.ECDSAKey, "key-passphrase",
            )
            server = peers.enter_context(peer(
                paramiko.RSAKey.generate(2048), (rsa_key, ed_key),
                root=root, extra_host_keys=(ed_key,), allow_terminal=True,
            ))
            ec_jump = peers.enter_context(peer(
                paramiko.ECDSAKey.generate(), (ec_key,), user="jump", forward=server.server_address,
            ))
            host_key = openssh_key(
                work / "host_ed", ed25519.Ed25519PrivateKey.generate(), paramiko.Ed25519Key,
            )
            ed_jump = peers.enter_context(peer(
                host_key, (ed_key,), user="jump", forward=ec_jump.server_address,
            ))
            http = peers.enter_context(loopback(Proxy, scheme="http", forward=ed_jump.server_address))
            socks = peers.enter_context(loopback(Proxy, scheme="socks5h", forward=server.server_address))
            login = (b"proxy-fixture", b'gate:"\\only')
            http_auth = peers.enter_context(loopback(Proxy, scheme="http", forward=server.server_address, credentials=login))
            socks_auth = peers.enter_context(loopback(Proxy, scheme="socks5h", forward=server.server_address, credentials=login))
            scp_root = work / "scp"
            scp_root.mkdir()
            scp = peers.enter_context(peer(host_key, (ed_key,), root=scp_root, allow_scp=True))
            gss_only = peers.enter_context(loopback(GssKexOnly))
            gss_null = peers.enter_context(loopback(GssKexOnly, gss_host_key=b"null"))
            environment = dict(
                os.environ,
                **properties(server, "ARCO_SFTP_TEST_"),
                **properties(ed_jump, "ARCO_SFTP_TEST_ED_"),
                **properties(ec_jump, "ARCO_SFTP_TEST_EC_"),
                **properties(scp, "ARCO_SCP_TEST_"),
                ARCO_SFTP_TEST_ROOT=str(root),
                ARCO_SFTP_TEST_KEY=str(key),
                ARCO_SFTP_TEST_ED_KEY=str(ed_path),
                ARCO_SFTP_TEST_EC_KEY=str(ec_path),
                ARCO_SFTP_TEST_HTTP=f"http://127.0.0.1:{http.server_address[1]}",
                ARCO_SFTP_TEST_SOCKS=f"socks5h://127.0.0.1:{socks.server_address[1]}",
                ARCO_SFTP_TEST_HTTP_AUTH=f"http://127.0.0.1:{http_auth.server_address[1]}",
                ARCO_SFTP_TEST_SOCKS_AUTH=f"socks5h://127.0.0.1:{socks_auth.server_address[1]}",
                ARCO_GSS_KEX_ONLY_URL=f"sftp://arco@127.0.0.1:{gss_only.server_address[1]}/",
                ARCO_GSS_KEX_NULL_URL=f"sftp://127.0.0.1:{gss_null.server_address[1]}/",
                SSH_AUTH_SOCK=rf"\\.\pipe\arco-sftp-test-{uuid.uuid4().hex}",
            )
            command = [str(args.harness.resolve())] if args.harness else [
                "cargo", "test", "--locked", "-p", "arco-windows", "--lib",
                "reference_sftp_", "--",
            ]
            if args.harness:
                command.append("reference_sftp_")
            subprocess.run(
                [*command, "--ignored", "--nocapture", "--test-threads=1"],
                env=environment, check=True, timeout=240,
            )
            scp_command = [str(args.harness.resolve()), "reference_scp_"] if args.harness else [
                "cargo", "test", "--locked", "-p", "arco-windows", "--lib", "reference_scp_", "--",
            ]
            subprocess.run([*scp_command, "--ignored", "--nocapture", "--test-threads=1"], env=environment, check=True, timeout=60)
            if not args.backend_only:
                for test in (APP_TEST, WATCH_TEST, WATCH_DELETE_TEST):
                    app_command = [str(args.app_harness.resolve()), test] if args.app_harness else [
                        "cargo", "test", "--locked", "-p", "arco-app", "--bin", "arco-commander",
                        test, "--",
                    ]
                    result = subprocess.run(
                        [*app_command, "--exact", "--ignored", "--nocapture"],
                        env=environment, check=True, timeout=240, capture_output=True,
                        encoding="utf-8", errors="replace",
                        creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
                    )
                    print(result.stdout)
                    assert "1 passed" in result.stdout
                for fixture in (root, scp_root):
                    uploaded = fixture / "round trip" / "outgoing" / "ä # export.txt"
                    assert uploaded.read_bytes() == b"from local"
                print("Application reference passed: recursive search and bidirectional SFTP and SCP sync")
            if args.net_exe:
                reference_script(args.net_exe, work, root, environment)
                reference_sync(args.net_exe, work, root, environment)
                reference_script_watch(args.net_exe, work, root, environment)
            assert set(server.auth_attempts) == {"arco", "interactive", "mfa"}, server.auth_attempts
            for jump in (ed_jump, ec_jump):
                assert set(jump.auth_attempts) == {"jump"}, jump.auth_attempts


if __name__ == "__main__":
    main()
