"""Run Arco's opt-in SFTP workflow against a disposable Paramiko server."""

import argparse
import base64
from contextlib import ExitStack, contextmanager
import hashlib
import hmac
import json
import logging
import os
from pathlib import Path
import re
import shlex
import socket
import socketserver
import stat
import struct
import subprocess
import sys
import tempfile
import threading
import time
import uuid
from urllib.parse import quote, urlencode, urlsplit
from urllib.request import parse_http_list, parse_keqv_list
from socks4_fixture import read_connect, reply_code

import paramiko
from cryptography.hazmat.primitives.asymmetric import ec, ed25519
from cryptography.hazmat.primitives.serialization import (
    BestAvailableEncryption, Encoding, NoEncryption, PrivateFormat,
)


APP_TEST = "app::ftp_sync::tests::ssh_search_and_bidirectional_sync"
MAPPED_SYNC_TEST = "app::ftp_sync::tests::sftp_mapped_sync_rechecks_mirror_deletions"
WATCH_TEST = "app::network_watch::tests::reference_network_watch"
WATCH_DELETE_TEST = "app::network_watch::tests::reference_network_watch_deletes_only_confirmed_files"


def reference_script(executable, work, remote_root, environment):
    profile = work / "script-profile.json"
    actual_pin = environment["ARCO_SFTP_TEST_FINGERPRINT"]
    alternate_pin = "SHA256:" + base64.b64encode(bytes(32)).decode().rstrip("=")
    profile.write_text(json.dumps({"ftp_profiles": {"Reference site": {
        "address": environment["ARCO_SFTP_TEST_URL"],
        "security": "sftp",
        "user": "arco",
        "ssh_fingerprint": f"{alternate_pin};{actual_pin}",
    }}, "network_command_templates": {"Fixture arguments": "arco-fixture-args !& !/"}}), encoding="utf-8")
    local = work / "script files"
    local.mkdir()
    payload = b"native SFTP script"
    (local / "source.txt").write_bytes(payload)
    script = work / "sftp-script.txt"
    script.write_text(
        f'echo "Upload #1"\nlcd "{local}"\nlls *.TXT\nopen "Reference site"\nmkdir "script round trip"\n'
        'cd "script round trip"\nput source.txt "Büro #1.txt"\nls\n'
        'call "arco-fixture-print"\n'
        'call -template "arco-fixture-args !& !/" "-x.txt" "Büro\'s ; $x.txt"\n'
        'call -saved "Fixture arguments" "file with spaces.txt"\n'
        'call -template "arco-fixture-args !@ !U !# !N !S"\n'
        'stat "Büro #1.txt"\nchecksum "Büro #1.txt"\nchmod 0640 "Büro #1.txt"\n'
        'checksum sha-1 "/script round trip/Büro #1.txt"\n'
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
    assert {event["algorithm"]: event["checksum"] for event in events if "checksum" in event} == {
        "SHA-256": hashlib.sha256(payload).hexdigest(),
        "SHA-1": hashlib.sha1(payload).hexdigest(),
    }
    assert next(event["permissions"] for event in events if "permissions" in event) == "0640"
    assert next(event for event in events if "stdout" in event) == {
        "stdout": "native command\n", "stderr": "fixture diagnostic\n", "status": 0,
    }
    commands = [event["commands"][0] for event in events if "commands" in event]
    command = commands[0]
    assert json.loads(command["stdout"]) == ["./-x.txt", "./Büro's ; $x.txt", "/script round trip/"]
    assert command["status"] == 0
    assert json.loads(commands[1]["stdout"]) == ["./file with spaces.txt", "/script round trip/"]
    host, user, port, profile_name, address = json.loads(commands[2]["stdout"])
    parsed = urlsplit(address)
    assert (host, user, port, profile_name) == ("127.0.0.1", "arco", str(parsed.port), "Reference site")
    assert parsed.port == urlsplit(environment["ARCO_SFTP_TEST_URL"]).port
    assert (parsed.scheme, parsed.username, parsed.hostname, parsed.path) == (
        "sftp", "arco", "127.0.0.1", "/script%20round%20trip/",
    )
    assert parsed.password is None and parsed.query == "" and parsed.fragment == ""
    assert "fixture-only" not in result.stdout + result.stderr
    assert not (remote_root / "script round trip").exists()
    script.write_text('open "Reference site"\ncall "arco-fixture-fail"\nmkdir should-not-run\n', encoding="utf-8")
    result = subprocess.run(args, env=environment, timeout=15, capture_output=True,
                            encoding="utf-8", creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
    assert result.returncode == 1 and "Status 7" in result.stderr
    assert not (remote_root / "should-not-run").exists()
    script.write_text('open "Reference site"\nmkdir should-not-run\ninval id\n', encoding="utf-8")
    result = subprocess.run(args, env=environment, timeout=15, capture_output=True,
                            encoding="utf-8", creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
    assert result.returncode == 1 and not (remote_root / "should-not-run").exists()
    script.write_text('open "arco://session/Reference%20site?path=%2F"\npwd\n', encoding="utf-8")
    result = subprocess.run(args, env=environment, check=True, timeout=15, capture_output=True,
                            encoding="utf-8", creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
    assert json.loads(result.stdout.splitlines()[0])["event"]["open"] == "Reference site"
    script.write_text(
        f'open "Reference site" -hostkey="{actual_pin}"\npwd\n', encoding="utf-8",
    )
    result = subprocess.run(args, env=environment, check=True, timeout=15, capture_output=True,
                            encoding="utf-8", creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
    assert json.loads(result.stdout.splitlines()[-1])["event"]["remote"] == "/"
    unknown_pin = "SHA256:" + base64.b64encode(bytes([1]) * 32).decode().rstrip("=")
    script.write_text(f'open "Reference site" -hostkey="{unknown_pin}"\n', encoding="utf-8")
    result = subprocess.run(args, env=environment, timeout=15, capture_output=True,
                            encoding="utf-8", creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
    assert result.returncode == 1 and "außerhalb" in json.loads(result.stderr)["error"]
    print("open -hostkey narrowed saved host-key alternatives and refused an unknown key")
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

    removals = remote / "batch removals"
    removals.mkdir()
    for name in ("alpha [1].tmp", "beta.tmp", "UPPER.TMP", "saved.dat", "archived.dat"):
        (removals / name).write_bytes(name.encode())
    remove = 'rm -permanent "batch removals/alpha [1].t?p" "batch removals/*.tmp"'
    error = run("option failonnomatch on", f'{remove} "batch removals/missing?"', expected=1)
    assert "Keine Treffer" in error and (removals / "alpha [1].tmp").is_file()
    assert run(remove) == {"completed": 2, "entries": [
        {"removed": "alpha [1].tmp"}, {"removed": "beta.tmp"},
    ]}
    assert (removals / "UPPER.TMP").is_file()
    assert run('rm -permanent "batch removals/absent*"') == {"completed": 0, "entries": []}
    recycled = run('rm "batch removals/saved.dat" "batch removals/archived.dat"')
    assert recycled["completed"] == 2
    for record in recycled["entries"]:
        name = record["recycled"]
        assert not (removals / name).exists()
        assert (remote / ".arco-trash" / record["slot"] / name).read_bytes() == name.encode()

    for folder in ("empty A", "empty B", "tree/sub"):
        (removals / folder).mkdir(parents=True)
    (removals / "tree/sub/payload").write_bytes(b"recursive removal")
    error = run('cd "batch removals"', 'rmdir -permanent "empty A" tree "empty B"', expected=1)
    assert "nicht leer" in error and (removals / "empty A").is_dir()
    assert run('cd "batch removals"', 'rmdir -permanent "empty A" "empty B"')["completed"] == 2
    assert run('rm -permanent "/script sync/batch removals/tree/" "batch removals/tree/sub/payload"') == {
        "completed": 1, "entries": [{"removed": "tree"}],
    }
    assert not (removals / "tree").exists() and (removals / "UPPER.TMP").is_file()
    print("SFTP batch removal checked masks, preflight, recycling, and recursive removal")

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


def reference_script_watch(executable, work, root, environment, mapped=False):
    import queue

    folder_name = "mapped watch script" if mapped else "watch script"
    local, remote = work / folder_name, root / folder_name
    local.mkdir()
    remote.mkdir()
    local_name = "BUDGET%3F.TXT" if mapped else "initial.txt"
    remote_name = "mixed-question-file" if mapped else "initial.txt"
    local_file, remote_file = local / local_name, remote / remote_name
    local_file.write_bytes(b"confirmed file")
    remote_file.write_bytes(b"confirmed file")
    (remote / "foreign.txt").write_bytes(b"keep on server")
    profile = work / "watch-profile.json"
    settings = {
        "address": environment["ARCO_SFTP_TEST_URL"],
        "security": "sftp", "user": "arco", "recycle_remote": True,
        "ssh_fingerprint": environment["ARCO_SFTP_TEST_FINGERPRINT"],
    }
    profile.write_text(json.dumps({"ftp_profiles": {"Watch": settings}}), encoding="utf-8")
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
        naming = " -rawtransfersettings FileNameCase=1 ReplaceInvalidChars=1" if mapped else ""
        process.stdin.write(f'open Watch\nkeepuptodate -interval=2 -duration=7 -delete{naming} '
                            f'-filemask=*.txt "{local}" "/{folder_name}"\necho returned\n')
        process.stdin.close()
        try:
            first = events.get(timeout=10)
            assert first.get("open") == "Watch", first
            started = events.get(timeout=10)
            assert started["keepuptodate"] == "started", started
            (local / "new.txt").write_bytes(b"watch publication")
            (local / "ignored.bin").write_bytes(b"excluded")
            local_file.unlink()
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
    published = "NEW.TXT" if mapped else "new.txt"
    assert published in {entry.name for entry in remote.iterdir()}
    assert (remote / published).read_bytes() == b"watch publication"
    assert (remote / "foreign.txt").read_bytes() == b"keep on server"
    assert not remote_file.exists() and not (remote / "ignored.bin").exists()
    assert any(path.read_bytes() == b"confirmed file" for path in (root / ".arco-trash").glob(f"*/{remote_name}"))
    if mapped:
        print("Script watch paired Windows aliases, published the converted name and recycled the original server entry.")
        return
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
        if self.server.allow_terminal:
            try:
                parts = shlex.split(command.decode("utf-8"))
            except (ValueError, UnicodeDecodeError):
                return False
            if len(parts) > 5 and parts[0] == "cd" and parts[2:5] == ["||", "exit", "$?;"]:
                output, error, status = b"", b"", 0
                name, folder = parts[5], parts[1]
                if name == "arco-fixture-print" and folder == "/script round trip":
                    output, error = b"native command\n", b"fixture diagnostic\n"
                elif name == "arco-fixture-args" and folder == "/script round trip":
                    output = json.dumps(parts[6:]).encode("utf-8")
                elif name == "arco-fixture-fail" and folder == "/":
                    error, status = b"fixture failure\n", 7
                else:
                    return False

                def respond():
                    with channel:
                        channel.settimeout(4)
                        if channel.recv(1):
                            return
                        if output:
                            channel.sendall(output)
                        if error:
                            channel.sendall_stderr(error)
                        channel.send_exit_status(status)

                threading.Thread(target=respond, daemon=True).start()
                return True
        if not self.server.allow_scp:
            return False
        threading.Thread(target=serve_scp, args=(self.server, channel, command), daemon=True).start()
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
        aliases = self.login.server.transfer_aliases
        base = self.root.joinpath(*(aliases.get(segment, segment) for segment in remote.split("/"))).resolve()
        if not base.is_relative_to(self.root):
            raise PermissionError(13, "Outside test fixtures")
        return base

    def attributes(self, path):
        aliases = self.login.server.transfer_aliases
        name = next((name for name, storage in aliases.items() if storage == path.name), path.name)
        if path in self.login.server.symlinks:
            attrs = paramiko.SFTPAttributes()
            attrs.st_mode = stat.S_IFLNK | 0o777
            attrs.filename = name
            return attrs
        attrs = paramiko.SFTPAttributes.from_stat(path.lstat(), name)
        overrides = self.login.server.unix_metadata.get(path, {})
        if "mode" in overrides:
            attrs.st_mode = (attrs.st_mode & ~0o7777) | overrides["mode"]
        attrs.st_uid = overrides.get("uid", attrs.st_uid)
        attrs.st_gid = overrides.get("gid", attrs.st_gid)
        return attrs

    @sftp_operation
    def list_folder(self, remote):
        folder = self.local(remote)
        entries = [self.attributes(entry) for entry in folder.iterdir()]
        entries.extend(self.attributes(link) for link in self.login.server.symlinks if link.parent == folder)
        return entries

    @sftp_operation
    def stat(self, remote):
        return self.attributes(self.local(remote))

    lstat = stat

    @sftp_operation
    def open(self, remote, flags, attrs):
        path = self.local(remote)
        fd = os.open(path, flags | os.O_BINARY, attrs.st_mode or 0o600)
        writable = bool(flags & (os.O_WRONLY | os.O_RDWR))
        file = os.fdopen(fd, "r+b" if writable else "rb", buffering=0)
        if path.name == "sftp-reconnect.bin" or path.name.endswith(".filepart"):
            handle = DropOnce(flags, self.login, path.name == "sftp-reconnect.bin")
        elif not writable and path.name in ParallelRead.names:
            handle = ParallelRead(flags, self.login, path.name)
        elif not writable and path.name in ResumeRead.names:
            handle = ResumeRead(flags, self.login, path.name)
        elif writable and "arco-parallel-upload" in path.parts and path.name.startswith("arco-upload-"):
            handle = BatchFile(flags, self.login, "upload")
        elif not writable and "arco-parallel-download" in path.parts and path.name in {"a.bin", "b.bin"}:
            handle = BatchFile(flags, self.login, "download")
        else:
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
        folder = self.local(remote)
        if any(link.parent == folder for link in self.login.server.symlinks):
            return paramiko.SFTP_FAILURE
        folder.rmdir()
        return paramiko.SFTP_OK

    @sftp_operation
    def remove(self, remote):
        local = self.local(remote)
        if local not in self.login.server.symlinks:
            local.unlink()
        else:
            del self.login.server.symlinks[local]
        return paramiko.SFTP_OK

    @sftp_operation
    def symlink(self, target, remote):
        if self.login.server.link_path_first:
            target, remote = remote, target
        local = self.local(remote)
        if local.exists() or local in self.login.server.symlinks or not local.parent.is_dir():
            return paramiko.SFTP_FAILURE
        self.login.server.symlinks[local] = target
        return paramiko.SFTP_OK

    @sftp_operation
    def readlink(self, remote):
        return self.login.server.symlinks.get(self.local(remote), paramiko.SFTP_FAILURE)

    @sftp_operation
    def posix_rename(self, before, after):
        source, target = self.local(before), self.local(after)
        if source not in self.login.server.symlinks or target not in self.login.server.symlinks:
            return paramiko.SFTP_FAILURE
        if self.login.server.symlinks[source] == "publication denied":
            return paramiko.SFTP_PERMISSION_DENIED
        self.login.server.symlinks[target] = self.login.server.symlinks.pop(source)
        return paramiko.SFTP_OK

    @sftp_operation
    def hardlink(self, before, after):
        target = self.local(after)
        if target in self.login.server.symlinks:
            return paramiko.SFTP_FAILURE
        os.link(self.local(before), target)
        return paramiko.SFTP_OK

    @sftp_operation
    def rename(self, before, after):
        source, target = self.local(before), self.local(after)
        if target in self.login.server.symlinks:
            return paramiko.SFTP_FAILURE
        if source in self.login.server.symlinks:
            if target.exists():
                return paramiko.SFTP_FAILURE
            if self.login.server.symlinks[source] == "publication denied":
                return paramiko.SFTP_PERMISSION_DENIED
            self.login.server.symlinks[target] = self.login.server.symlinks.pop(source)
            return paramiko.SFTP_OK
        if source.name.startswith("arco-upload-") and target.name == "publication-blocked.txt":
            return paramiko.SFTP_PERMISSION_DENIED
        source.rename(target)
        return paramiko.SFTP_OK

    @sftp_operation
    def chattr(self, remote, attrs):
        path = self.local(remote)
        paramiko.SFTPServer.set_file_attr(str(path), attrs)
        overrides = self.login.server.unix_metadata.setdefault(path, {})
        if attrs.st_mode is not None:
            overrides["mode"] = attrs.st_mode
        if attrs.st_uid is not None:
            overrides["uid"] = attrs.st_uid
        if attrs.st_gid is not None:
            overrides["gid"] = attrs.st_gid
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


class ParallelRead(paramiko.SFTPHandle):
    names = {"sftp-segments.bin", "sftp-segments-cancel.bin",
             "sftp-segments-stale.bin", "sftp-segments-hash.bin", "sftp-segments-legacy.bin"}

    def __init__(self, flags, login, name):
        super().__init__(flags)
        self.login = login
        self.name = name
        self.reads = 0

    def stat(self):
        if self.name == "sftp-segments-legacy.bin":
            return paramiko.SFTP_OP_UNSUPPORTED
        return paramiko.SFTPAttributes.from_stat(os.fstat(self.readfile.fileno()))

    def read(self, offset, length):
        self.reads += 1
        if self.reads == 1:
            server = self.login.server
            barrier = server.segment_barriers.setdefault(self.name, threading.Barrier(2))
            barrier.wait(timeout=6)
            # The second range can fill the end of the staging file first.
            if offset == 0 and self.name == "sftp-segments.bin":
                time.sleep(0.2)
        if self.name == "sftp-segments-cancel.bin" and self.reads == 2:
            time.sleep(0.25)
        data = super().read(offset, length)
        if isinstance(data, bytes) and data and (
            self.name == "sftp-segments-stale.bin" and self.reads == 1 and offset > 0
            or self.name == "sftp-segments-hash.bin" and offset == 32 * 1024
        ):
            return bytes([data[0] ^ 0xff]) + data[1:]
        return data


class ResumeRead(paramiko.SFTPHandle):
    names = {"sftp-ranges-restart.bin", "sftp-ranges-retry.bin"}

    def __init__(self, flags, login, name):
        super().__init__(flags)
        self.login = login
        self.name = name
        self.first = True

    def stat(self):
        return paramiko.SFTPAttributes.from_stat(os.fstat(self.readfile.fileno()))

    def read(self, offset, length):
        server = self.login.server
        with server.range_lock:
            if self.first:
                self.first = False
                server.range_opens[self.name] = server.range_opens.get(self.name, 0) + 1
                initial = server.range_opens[self.name] <= 2
            else:
                initial = False
            fail = (self.name == "sftp-ranges-retry.bin" and offset == 96 * 1024
                    and not server.range_disconnected)
            if fail:
                server.range_disconnected = True
            else:
                with server.range_trace.open("a", encoding="utf-8") as trace:
                    trace.write(f"{self.name} {offset} {length}\n")
        if initial:
            barrier = server.segment_barriers.setdefault(self.name, threading.Barrier(2))
            barrier.wait(timeout=6)
        if fail:
            self.login.connection.close()
            return paramiko.SFTP_EOF
        time.sleep(0.003)
        return super().read(offset, length)


class BatchFile(paramiko.SFTPHandle):
    def __init__(self, flags, login, direction):
        super().__init__(flags)
        self.barrier = login.server.segment_barriers.setdefault(direction, threading.Barrier(2))
        self.first = True

    def wait_for_peer(self):
        if self.first:
            self.first = False
            self.barrier.wait(timeout=8)

    def read(self, offset, length):
        self.wait_for_peer()
        return super().read(offset, length)

    def write(self, offset, data):
        self.wait_for_peer()
        return super().write(offset, data)


class BinaryHandles(paramiko.SFTPServer):
    def _send_server_version(self):
        kind, payload = self._read_packet()
        assert kind == paramiko.sftp.CMD_INIT
        version = struct.unpack_from(">I", payload)[0]
        message = paramiko.Message()
        message.add_int(3)
        message.add("check-file", "md5,sha1", "hardlink@openssh.com", "1")
        if not self.server.login.server.link_path_first:
            message.add("posix-rename@openssh.com", "1")
        self._send_packet(paramiko.sftp.CMD_VERSION, message)
        return version

    def _process(self, packet_type, request_number, message):
        if packet_type == paramiko.sftp.CMD_EXTENDED:
            fields = paramiko.Message(message.get_remainder())
            extension = fields.get_text()
            if extension == "hardlink@openssh.com":
                self._send_status(request_number, self.server.hardlink(fields.get_text(), fields.get_text()))
                return
            if extension == "check-file-name":
                path = fields.get_text()
                algorithm = fields.get_text()
                segments = Path(path).name in (ParallelRead.names | ResumeRead.names) - {"sftp-segments-legacy.bin"}
                if (algorithm == "sha1" or algorithm == "sha256" and segments) and fields.get_remainder() == bytes(20):
                    digest = hashlib.new(algorithm, self.server.local(path).read_bytes()).digest()
                    reply = paramiko.Message()
                    reply.add_int(request_number)
                    reply.add_string(algorithm)
                    reply.add_bytes(digest)
                    self._send_packet(paramiko.sftp.CMD_EXTENDED_REPLY, reply)
                    return
        super()._process(packet_type, request_number, message)

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
        if self.server.scheme == "telnet":
            with self.request.makefile("rb", buffering=0) as data:
                expected = (*self.server.credentials, f"connect ssh.invalid {port}".encode())
                for line in expected:
                    if data.readline(4096).rstrip(b"\r\n") != line:
                        self.request.sendall(b"Relay rejected the command\r\n")
                        return
            self.request.settimeout(None)
            forward(self.request, self.server.forward, b"Relay connected\r\n")
            return
        if self.server.scheme in {"socks4", "socks4a"}:
            address, host, requested_port, user = read_connect(self.request)
            destination_matches = (
                (address, host) == ("127.0.0.1", None) if self.server.scheme == "socks4"
                else (address, host) == ("0.0.0.1", "ssh.invalid")
            )
            if requested_port != port or not destination_matches or user != b"proxy-fixture":
                self.request.sendall(reply_code(False))
                return
            self.request.settimeout(None)
            forward(self.request, self.server.forward, reply_code(True))
            return
        credentials = getattr(self.server, "credentials", None)
        with self.request.makefile("rb") as data:
            if self.server.scheme == "http":
                assert data.readline(8192) == f"CONNECT 127.0.0.1:{port} HTTP/1.1\r\n".encode()
                assert data.readline(8192) == f"Host: 127.0.0.1:{port}\r\n".encode()
                header = data.readline(8192)
                if credentials:
                    expected = b"Proxy-Authorization: Basic " + base64.b64encode(b":".join(credentials)) + b"\r\n"
                    if header != expected:
                        challenge = getattr(self.server, "challenge", b"")
                        self.request.sendall(
                            b"HTTP/1.1 407 Proxy Authentication Required\r\n"
                            b"Connection: close\r\n" + challenge + b"\r\n"
                        )
                        return
                    header = data.readline(8192)
                assert header == b"\r\n"
                ready = b"HTTP/1.1 200 Connection established\r\n\r\n"
            else:
                if credentials:
                    assert data.read(4) == b"\x05\x02\x03\x02"
                    method = b"\x03" if self.server.scheme == "socks-chap" else b"\x02"
                else:
                    assert data.read(3) == b"\x05\x01\x00"
                    method = b"\x00"
                self.request.sendall(b"\x05" + method)
                if method == b"\x03":
                    user, password = credentials
                    assert data.read(7) == bytes((1, 2, 0x11, 1, 0x85, 2, len(user)))
                    assert data.read(len(user)) == user
                    self.request.sendall(b"\x01\x01\x11\x01\x85")
                    challenge = os.urandom(21)
                    self.request.sendall(bytes((1, 2, 1, 1, 32, 3, len(challenge))) + challenge)
                    assert data.read(4) == b"\x01\x01\x04\x10"
                    proof = data.read(16)
                    if not hmac.compare_digest(proof, hmac.digest(password, challenge, "md5")):
                        self.request.sendall(b"\x01\x01\x00\x01\x01")
                        return
                    self.request.sendall(b"\x01\x02\x00\x01\x00\x01\x01\x20")
                elif method == b"\x02":
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


class DigestProxy(socketserver.BaseRequestHandler):
    realm = "Arco, proxy fixture"
    first_nonce = "nonce-one"
    fresh_nonce = "nonce-two"

    def challenge(self, nonce, *, stale=False, close=False):
        body = b"Authentication required"
        header = (
            "HTTP/1.1 407 Proxy Authentication Required\r\n"
            'Proxy-Authenticate: Basic realm="other, scheme", Digest '
            f'realm="{self.realm}", nonce="{nonce}", algorithm={self.server.algorithm}, '
            f'qop="{self.server.qop}", opaque="test-value", stale={str(stale).lower()}\r\n'
            f"Content-Length: {len(body)}\r\n"
        )
        if close:
            header += "Connection: close\r\n"
        self.request.sendall(header.encode() + b"\r\n" + body)

    def valid(self, credentials, target, nonce):
        config = self.server
        user, password = config.credentials
        if not credentials.startswith("Digest "):
            return False
        values = parse_keqv_list(parse_http_list(credentials[len("Digest "):]))
        algorithm = config.algorithm.removesuffix("-sess")
        name = {"MD5": "md5", "SHA-256": "sha256", "SHA-512-256": "sha512_256"}[algorithm]

        def digest(data):
            return hashlib.new(name, data).hexdigest()

        cnonce = values.get("cnonce", "")
        a1 = digest(user + b":" + self.realm.encode() + b":" + password)
        if config.algorithm.endswith("-sess"):
            a1 = digest(f"{a1}:{nonce}:{cnonce}".encode())
        a2 = f"CONNECT:{target}"
        if config.qop == "auth-int":
            a2 += ":" + digest(b"")
        response = digest(f"{a1}:{nonce}:00000001:{cnonce}:{config.qop}:{digest(a2.encode())}".encode())
        return cnonce != "" and all(values.get(key) == expected for key, expected in {
            "username": user.decode(),
            "realm": self.realm,
            "algorithm": config.algorithm,
            "uri": target,
            "nonce": nonce,
            "opaque": "test-value",
            "qop": config.qop,
            "nc": "00000001",
            "response": response,
        }.items())

    def handle(self):
        self.request.settimeout(5)
        target = f"ssh.invalid:{self.server.forward[1]}"
        nonce = self.first_nonce
        challenged = False
        with self.request.makefile("rb", buffering=0) as data:
            while True:
                first = data.readline(8192)
                if not first:
                    return
                assert first == f"CONNECT {target} HTTP/1.1\r\n".encode()
                assert data.readline(8192) == f"Host: {target}\r\n".encode()
                auth = data.readline(8192).decode().strip()
                if auth:
                    assert auth.startswith("Proxy-Authorization: ")
                    assert data.readline(8192) == b"\r\n"
                    auth = auth.removeprefix("Proxy-Authorization: ")
                if not auth:
                    self.challenge(nonce, close=self.server.close_challenge)
                    if self.server.close_challenge:
                        return
                    challenged = True
                    continue
                # A connected test peer after a Basic retry would expose a downgrade.
                if auth.startswith("Basic "):
                    break
                if not (challenged or self.server.close_challenge) or not self.valid(auth, target, nonce):
                    self.request.sendall(
                        b"HTTP/1.1 407 Proxy Authentication Required\r\n"
                        b'Proxy-Authenticate: Basic realm="other"\r\nContent-Length: 0\r\n\r\n'
                    )
                    continue
                if self.server.stale and nonce == self.first_nonce:
                    nonce = self.fresh_nonce
                    self.challenge(nonce, stale=True)
                    continue
                break
        self.request.settimeout(None)
        forward(self.request, self.server.forward, b"HTTP/1.1 200 Connection established\r\n\r\n")


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


def peer(host_key, client_keys, *, user="arco", root=None, forward=None, extra_host_keys=(), allow_scp=False, allow_terminal=False, link_path_first=False):
    return loopback(
        Peer, root=root, forward=forward, host_key=host_key, client_keys=client_keys,
        user=user, extra_host_keys=extra_host_keys, interrupted=False, interrupted_upload=False,
        allow_scp=allow_scp, allow_terminal=allow_terminal, unix_metadata={}, transfer_aliases={},
        symlinks={}, link_path_first=link_path_first, segment_barriers={},
    )


def scp_link_fixture(command):
    """Recognize the exact link operations used by the reference, including quoting."""
    if "'/arco-link-fixture" not in command:
        return False
    def quote(value):
        return "'" + value.replace("'", "'\\''") + "'"
    folder = "/arco-link-fixture/"
    link = quote(folder + "Arco's link #1")
    source = quote(folder + "source file.txt")
    target_folder = quote(folder + "target folder")
    folder_link = quote(folder + "folder alias")
    chain = quote(folder + "link chain")
    loop = quote(folder + "link loop")
    original = quote("../missing ; '# Deutsch ä")
    replacement = quote("source file.txt")
    if command in (
        f"ln -sT -- {original} {link}", f"ln -sT -- {original} {source}",
        f"readlink -n -- {link}", f"readlink -n -- {source}",
        f"test -f {source} && test ! -L {source} && ln -T -- {source} {link}",
        f"test -f {source} && test ! -L {source} && ln -T -- {source} {quote(folder + 'hard link.txt')}",
        f"ln -sT -- 'target folder' {folder_link}", f"ln -sT -- 'folder alias' {chain}",
        f"ln -sT -- 'link loop' {loop}",
        f"LC_ALL=C stat --printf '%f %s' -- {source}",
        f"LC_ALL=C stat --printf '%f %s' -- {target_folder}",
        "LC_ALL=C find '/arco-link-fixture' -mindepth 1 -maxdepth 1 -printf '%f\\0%y\\0%s\\0%T@\\0%i\\0'",
    ):
        return True
    if any(command in (f"test -L {name} && realpath -e -z -- {name}", f"test -L {name} && rm -- {name}")
           for name in (link, folder_link, chain, loop, source)):
        return True
    prefix = re.match(r"umask 077; mkdir -- '(/arco-link-fixture/\.arco-link-[A-Za-z0-9]{16})'", command)
    if not prefix:
        return False
    stage = quote(prefix[1])
    staged_link = quote(prefix[1] + "/link")
    for destination, expected, target in (
        (link, quote("source file.txt."), quote("stale write")),
        (link, quote("../missing ; '# Deutsch ä."), replacement),
        (source, quote("../missing ; '# Deutsch ä."), quote("Arco's link #1")),
    ):
        if command == (
            f"umask 077; mkdir -- {stage} || exit $?; \n"
            f"ln -sT -- {target} {staged_link} && test -L {destination} && \n"
            f'test "$(readlink -n -- {destination} && printf .)" = {expected} && \n'
            f"mv -fT -- {staged_link} {destination}; rc=$?; \n"
            f'rm -f -- {staged_link} && rmdir -- {stage} || exit $?; exit "$rc"'
        ):
            return True
    return False


def serve_scp(server, channel, command):
    """Expose only the SCP operations and fixed publication commands used by the fixture."""
    root = server.root
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
        text = command.decode("utf-8")
        parts = shlex.split(text)
        if scp_link_fixture(text):
            bash = Path(os.environ.get("ProgramFiles", "C:/Program Files")) / "Git/usr/bin/bash.exe"
            # Run the generated POSIX commands against this fixture only. Git Bash
            # can preserve a dangling symlink on Windows without administrator rights.
            git_root = "/" + root.drive[0].lower() + root.as_posix()[2:]
            assert "'" not in git_root
            result = subprocess.run(
                [str(bash), "--noprofile", "--norc", "-c",
                 "export PATH=/usr/bin:/bin; " + text.replace("'/arco-link-fixture", f"'{git_root}/arco-link-fixture")],
                capture_output=True, timeout=10,
                env=dict(os.environ, MSYS="winsymlinks:lnk", LC_ALL="C.UTF-8"),
                creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
            )
            output = result.stdout
            if "&& realpath -e -z" in text and result.returncode == 0:
                prefix = git_root.encode("utf-8") + b"/"
                assert output.startswith(prefix), output
                output = output[len(prefix) - 1:]
            channel.sendall(output)
            status = result.returncode
        elif len(parts) == 4 and parts[0] == "scp" and parts[1] in ("-t", "-f") and parts[2] == "--":
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
                info = item.lstat()
                kind = "l" if item.is_symlink() else ("d" if item.is_dir() else "f")
                channel.sendall(f"{item.name}\0{kind}\0{info.st_size}\0{info.st_mtime:.6f}\0{info.st_ino}\0".encode())
            status = 0
        elif parts[:3] == ["test", "!", "-L"] and parts[4:6] == ["&&", "test"] and parts[8] == "&&":
            assert parts[3] == parts[7] == parts[-1]
            raw = root.joinpath(*parts[3].split("/"))
            file = local(parts[3])
            assert not raw.is_symlink()
            assert file.is_dir() if parts[6] == "-d" else file.is_file()
            attrs = server.unix_metadata.setdefault(file, {})
            if parts[9] == "chmod" and parts[11] == "--":
                attrs["mode"] = int(parts[10], 8)
            elif parts[9] == "chown" and parts[10:12] == ["-h", "--"]:
                owner, _, group = parts[12].partition(":")
                attrs["uid"] = int(owner)
                if group:
                    attrs["gid"] = int(group)
            elif parts[9] == "chgrp" and parts[10:12] == ["-h", "--"]:
                attrs["gid"] = int(parts[12])
            else:
                raise AssertionError(f"Unexpected metadata command: {parts}")
            status = 0
        elif len(parts) == 6 and parts[:5] == ["LC_ALL=C", "stat", "-c", "%a %u %g", "--"]:
            file = local(parts[5])
            attrs = server.unix_metadata.get(file, {})
            mode = attrs.get("mode", file.stat().st_mode & 0o7777)
            channel.sendall(f"{mode:o} {attrs.get('uid', 0)} {attrs.get('gid', 0)}\n".encode())
            status = 0
        elif len(parts) == 5 and parts[0] == "LC_ALL=C" and parts[2:4] == ["--zero", "--"]:
            assert parts[1] in ("sha512sum", "sha384sum", "sha256sum", "sha224sum", "sha1sum", "md5sum")
            digest = hashlib.new(parts[1].removesuffix("sum"), local(parts[4]).read_bytes()).hexdigest()
            channel.sendall(f"{digest}  {parts[4]}\0".encode())
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


def reference_link_script(executable, work, server):
    settings = properties(server, "LINK_")
    profile = work / "link-profile.json"
    profile.write_text(json.dumps({"ftp_profiles": {"links": {
        "address": settings["LINK_URL"], "security": "sftp", "user": "arco",
        "ssh_fingerprint": settings["LINK_FINGERPRINT"],
    }}}), encoding="utf-8")
    commands = (
        'open links\nsymlink "old target" "/arco-link-fixture/script link"\n'
        'readlink "arco-link-fixture/script link"\n'
        'setlink "arco-link-fixture/script link" "old target" "source file.txt"\n'
        'ln -hard "source file.txt" "arco-link-fixture/script hard link.txt"\n'
        'readlink "arco-link-fixture/script link"\n'
        'cd /arco-link-fixture\nstat "script link"\nrm -permanent "script link"\n'
    )
    result = subprocess.run(
        [str(executable.resolve()), "--profile", str(profile), "--script", "-", "--password-env", "ARCO_LINK_FIXTURE_PASSWORD"],
        input=commands, env=dict(os.environ, ARCO_LINK_FIXTURE_PASSWORD="fixture-only"),
        encoding="utf-8", capture_output=True, timeout=30,
        creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
    )
    assert result.returncode == 0, result.stderr
    events = [json.loads(line)["event"] for line in result.stdout.splitlines()]
    assert events[2]["target"] == "old target"
    assert events[4]["hard"] and events[5]["target"] == "source file.txt"
    assert events[7]["entry"]["symlink"] and events[8]["removed"] == "script link"
    folder = server.root / "arco-link-fixture"
    assert (folder / "script hard link.txt").samefile(folder / "source file.txt")
    assert folder / "script link" not in server.symlinks
    print("arco-net server links passed with absolute and relative link names")


def reference_unix_metadata(executable, work, server, scheme):
    profile = work / f"{scheme}-metadata.json"
    address = properties(server, "TEST_")
    profile.write_text(json.dumps({"ftp_profiles": {"metadata": {
        "address": address["TEST_URL"].replace("sftp:", f"{scheme}:", 1),
        "security": scheme, "user": "arco", "ssh_fingerprint": address["TEST_FINGERPRINT"],
    }}}), encoding="utf-8")
    folder = server.root / "metadata tree"
    nested = folder / "nested"
    nested.mkdir(parents=True)
    file = nested / "Eintrag.txt"
    file.write_bytes(b"file in the selected tree")
    sibling = server.root / "outside.txt"
    sibling.write_bytes(b"untouched")
    try:
        (nested / "linked.txt").symlink_to(sibling)
    except OSError:
        pass
    commands = (
        'open metadata\nchmod -recursive 0755 "/metadata tree"\n'
        'chown -R 1234:4321 "metadata tree"\nchgrp -recursive 6789 "/metadata tree"\n'
        'stat "/metadata tree/nested/Eintrag.txt"\n'
        'checksum sha-256 "/metadata tree/nested/Eintrag.txt"\n'
    )
    result = subprocess.run(
        [str(executable.resolve()), "--profile", str(profile), "--script", "-", "--password-env", "ARCO_METADATA_FIXTURE_PASSWORD"],
        input=commands, env=dict(os.environ, ARCO_METADATA_FIXTURE_PASSWORD="fixture-only"),
        encoding="utf-8", capture_output=True, timeout=45,
        creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
    )
    assert result.returncode == 0, result.stderr
    events = [json.loads(line)["event"] for line in result.stdout.splitlines()]
    assert [event["completed"] for event in events[1:4]] == [3, 3, 3]
    metadata = events[4]["entry"]
    assert [metadata[key] for key in ("permissions", "uid", "gid")] == ["0755", 1234, 6789], metadata
    assert events[5]["algorithm"] == "SHA-256"
    assert events[5]["checksum"] == hashlib.sha256(file.read_bytes()).hexdigest()
    modified = {path: attrs for path, attrs in server.unix_metadata.items() if path == folder or folder in path.parents}
    assert list(modified) == [file, nested, folder], modified
    assert all(attrs == {"mode": 0o755, "uid": 1234, "gid": 6789} for attrs in modified.values()), modified
    assert sibling not in server.unix_metadata
    print(f"{scheme.upper()} recursive ownership and permissions passed with children before folders.")


def reference_transfer_names(executable, work, server, environment):
    aliases = {
        "C:": "colon-directory",
        "Budget?.txt": "mixed-question-file",
        "BUDGET?.TXT": "upper-question-file",
    }
    server.transfer_aliases.update(aliases)
    original = server.root / "names" / aliases["C:"]
    original.mkdir(parents=True)
    (original / aliases["Budget?.txt"]).write_bytes(b"remote Windows-incompatible file")
    (original / "Plain.TXT").write_bytes(b"explicit destination")
    roundtrip = server.root / "name-roundtrip"
    roundtrip.mkdir()
    local = work / "name-conversion"
    local.mkdir()
    (local / "flat downloads").mkdir()
    (local / "README.TXT").write_bytes(b"legacy DOS spelling")
    (local / "Mixed.TXT").write_bytes(b"explicit upload destination")
    for parent, name in (("source-a", "Straße.txt"), ("source-b", "Strasse.txt")):
        (local / parent).mkdir()
        (local / parent / name).write_bytes(name.encode("utf-8"))
    profile = work / "name-profile.json"
    profile.write_text(json.dumps({
        "ftp_profiles": {"names": {
            "address": environment["ARCO_SFTP_TEST_URL"], "security": "sftp", "user": "arco",
            "ssh_fingerprint": environment["ARCO_SFTP_TEST_FINGERPRINT"], "transfer_preset": "Windows names",
        }},
        "network_transfer_presets": [{"name": "Windows names", "settings": {
            "names": {"case": "upper", "windows": True},
        }}],
    }), encoding="utf-8")
    argv = [str(executable.resolve()), "--profile", str(profile), "--script", "-", "--password-env", "ARCO_NAME_FIXTURE_PASSWORD"]
    def run(lines, expected=0):
        result = subprocess.run(
            argv, input="open names\n" + lines, cwd=local, encoding="utf-8", capture_output=True,
            env=dict(os.environ, ARCO_NAME_FIXTURE_PASSWORD="fixture-only"), timeout=35,
            creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
        )
        assert result.returncode == expected, result.stderr
        return result

    run('get -recursive "/names/C:/" .\n')
    assert (local / "C%3A" / "BUDGET%3F.TXT").read_bytes() == b"remote Windows-incompatible file"
    run('get "/names/C:/Budget?.txt" "flat downloads"\n')
    assert (local / "flat downloads" / "BUDGET%3F.TXT").read_bytes() == b"remote Windows-incompatible file"
    run('get "/names/C:/Plain.TXT" "NameAsTyped.out"\n')
    assert (local / "NameAsTyped.out").read_bytes() == b"explicit destination"
    run('put -recursive -namecase=preserve "C%3A" "/name-roundtrip/"\n')
    assert (roundtrip / aliases["C:"] / aliases["BUDGET?.TXT"]).read_bytes() == b"remote Windows-incompatible file"
    run('cd name-roundtrip\nput -namecase=lower Mixed.TXT Explicit.TXT\n')
    assert (roundtrip / "Explicit.TXT").read_bytes() == b"explicit upload destination"
    run('put -namecase=lower-dos README.TXT "/name-roundtrip/"\n')
    assert (roundtrip / "readme.txt").read_bytes() == b"legacy DOS spelling"
    assert "readme.txt" in [path.name for path in roundtrip.iterdir()]
    before = list(roundtrip.iterdir())
    collision = run('put -recursive "source-a/Straße.txt" "source-b/Strasse.txt" "/name-roundtrip/"\n', 1)
    assert "Doppeltes Transferziel" in collision.stderr
    assert list(roundtrip.iterdir()) == before
    print("SFTP filename rules round-tripped reserved Windows names and rejected mapped collisions before writing.")

    def event(command):
        result = run(command + "\n")
        return json.loads(result.stdout.splitlines()[-1])["event"]

    pair = local / "sync pair"
    remote_pair = server.root / "mapped-sync-script"
    nested = remote_pair / aliases["C:"]
    pair.mkdir()
    nested.mkdir(parents=True)
    remote_file = nested / aliases["Budget?.txt"]
    remote_file.write_bytes(b"remote file")
    arguments = '"sync pair" "/mapped-sync-script"'
    preview = event(f"synchronize local -preview {arguments}")["changes"]
    assert preview[-1] == {
        "action": "download", "path": r"C%3A\BUDGET%3F.TXT",
        "local_path": r"C%3A\BUDGET%3F.TXT", "remote_path": "C:/Budget?.txt",
    }, preview
    assert event(f"synchronize local {arguments}")["completed"] == 2
    mapped_local = pair / "C%3A" / "BUDGET%3F.TXT"
    assert mapped_local.read_bytes() == b"remote file"
    assert event(f"synchronize both -criteria=time,checksum -preview {arguments}")["changes"] == []

    mapped_local.write_bytes(b"updated locally")
    (mapped_local.parent / "Extra%3F.txt").write_bytes(b"extra file")
    server.transfer_aliases["EXTRA?.TXT"] = "extra-question-file"
    assert event(f"synchronize remote -mirror -criteria=checksum {arguments}")["completed"] == 2
    assert remote_file.read_bytes() == b"updated locally"
    assert (nested / "extra-question-file").read_bytes() == b"extra file"
    assert not (nested / aliases["BUDGET?.TXT"]).exists()

    os.utime(remote_file, (1_700_000_010, 1_700_000_010))
    assert event(f"synchronize local -timestamps {arguments}")["completed"] == 1
    assert int(mapped_local.stat().st_mtime) == 1_700_000_010
    assert mapped_local.read_bytes() == b"updated locally"
    (nested / "obsolete.txt").write_bytes(b"remote only")
    assert event(f"synchronize remote -delete {arguments}")["completed"] == 1
    assert not (nested / "obsolete.txt").exists()
    assert remote_file.is_file()
    orphan = pair / "local only.txt"
    orphan.write_bytes(b"local only")
    assert event(f"synchronize local -delete {arguments}")["completed"] == 1
    assert not orphan.exists()
    assert mapped_local.is_file()

    for name in ("Straße.txt", "Strasse.txt"):
        (pair / name).write_bytes(b"ambiguous")
    ambiguous = run(f"synchronize remote {arguments}\n", 1)
    assert "Kollidierende Dateinamen" in ambiguous.stderr
    assert not (remote_pair / "STRASSE.TXT").exists()
    print("SFTP synchronization paired transformed names, preserved original server spellings, and checked mirror and timestamp actions.")

    for name in ("Straße.txt", "Strasse.txt"):
        (pair / name).unlink()
    (mapped_local.parent / "Tail.txt%20").write_bytes(b"trailing space")
    server.transfer_aliases["TAIL.TXT "] = "trailing-space-file"
    assert event(f"synchronize remote -mirror -criteria=checksum {arguments}")["completed"] == 1
    assert (nested / "trailing-space-file").read_bytes() == b"trailing space"

    (remote_pair / "raw names").mkdir()
    run('put -rawtransfersettings FileNameCase=2 ReplaceInvalidChars=0 Mixed.TXT "/mapped-sync-script/raw names/"\n')
    assert [entry.name for entry in (remote_pair / "raw names").iterdir()] == ["mixed.txt"]


def reference_local_proxy_script(executable, work, server, command):
    profile = work / "local-proxy-profile.json"
    link = properties(server, "")
    profile.write_text(json.dumps({"ftp_profiles": {"Local proxy": {
        "address": link["URL"].replace("127.0.0.1", "localhost"),
        "user": "arco",
        "security": "sftp",
        "ssh_fingerprint": link["FINGERPRINT"],
        "address_family": "ipv4",
        "proxy": "local://command",
        "local_proxy_command": command + ' pipe "%host" %port 127.0.0.1',
        "ssh_proxy_dns_locally": True,
    }}}), encoding="utf-8")
    result = subprocess.run([
        str(executable.resolve()), "--profile", str(profile), "--script", "-",
        "--password-env", "ARCO_TEST_LOCAL_PROXY_PASSWORD",
    ], input=b'open "Local proxy"\nls\n', capture_output=True, timeout=30,
        env=dict(os.environ, ARCO_TEST_LOCAL_PROXY_PASSWORD="fixture-only"))
    assert result.returncode == 0, (result.stdout, result.stderr)
    print("SFTP local proxy: saved profile, local DNS and script listing passed.")


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
            proxy_helper = work / "Arco Proxy Helper.py"
            proxy_helper.write_bytes(Path(__file__).with_name("sftp-proxy-stdio-fixture.py").read_bytes())
            interpreter = Path(sys.executable).resolve()
            local_proxy = f'"{interpreter}" "{proxy_helper.as_posix()}"'
            relay_arguments = r' relay "%host" %port "%user" "%pass" "line\nvalue %%" ""'
            local_proxy_marker = work / "local-proxy-process.pid"
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
            server.range_lock = threading.Lock()
            server.range_trace = work / "segment-reads.txt"
            server.range_opens = {}
            server.range_disconnected = False
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
            http_basic = peers.enter_context(loopback(
                Proxy, scheme="http", forward=server.server_address, credentials=login,
                challenge=b'Proxy-Authenticate: Basic realm="fixture"\r\n',
            ))
            digest_proxies = {}
            for algorithm, qop, close, stale in (
                ("MD5", "auth", True, False),
                ("SHA-256", "auth", False, True),
                ("SHA-512-256-sess", "auth-int", False, False),
            ):
                digest = peers.enter_context(loopback(
                    DigestProxy, algorithm=algorithm, qop=qop, close_challenge=close,
                    stale=stale, forward=server.server_address, credentials=login,
                ))
                digest_proxies[f"ARCO_SFTP_TEST_HTTP_{algorithm.upper().replace('-', '_')}_AUTH"] = (
                    f"http://127.0.0.1:{digest.server_address[1]}"
                )
            socks_auth = peers.enter_context(loopback(Proxy, scheme="socks5h", forward=server.server_address, credentials=login))
            socks_chap = peers.enter_context(loopback(
                Proxy, scheme="socks-chap", forward=server.server_address, credentials=login,
            ))
            socks4 = peers.enter_context(loopback(Proxy, scheme="socks4", forward=server.server_address))
            socks4a = peers.enter_context(loopback(Proxy, scheme="socks4a", forward=server.server_address))
            telnet = peers.enter_context(loopback(
                Proxy, scheme="telnet", forward=server.server_address, credentials=login,
            ))
            telnet_default = peers.enter_context(loopback(
                Proxy, scheme="telnet", forward=server.server_address, credentials=(),
            ))
            scp_root = work / "scp"
            scp_root.mkdir()
            link_root, reversed_root, scp_link_root = (work / name for name in ("sftp-links", "sftp-other-order", "scp-links"))
            for link_fixture in (link_root, reversed_root, scp_link_root):
                links = link_fixture / "arco-link-fixture"
                links.mkdir(parents=True)
                (links / "source file.txt").write_bytes(b"preserve original")
                (links / "target folder").mkdir()
                (links / "target folder/preserved.txt").write_bytes(b"outside the link")
            link_server = peers.enter_context(peer(host_key, (ed_key,), root=link_root))
            link_server.transfer_aliases["alias?.txt"] = "arco-link-win-question-storage"
            reversed_server = peers.enter_context(peer(host_key, (ed_key,), root=reversed_root, link_path_first=True))
            scp_links = peers.enter_context(peer(host_key, (ed_key,), root=scp_link_root, allow_scp=True))
            (scp_root / "proxy-reference.txt").write_bytes(b"SCP through an authenticated proxy\n")
            scp = peers.enter_context(peer(host_key, (ed_key,), root=scp_root, allow_scp=True))
            scp_digest = peers.enter_context(loopback(
                DigestProxy, algorithm="SHA-256", qop="auth", close_challenge=False,
                stale=False, forward=scp.server_address, credentials=login,
            ))
            scp_chap = peers.enter_context(loopback(
                Proxy, scheme="socks-chap", forward=scp.server_address, credentials=login,
            ))
            gss_only = peers.enter_context(loopback(GssKexOnly))
            gss_null = peers.enter_context(loopback(GssKexOnly, gss_host_key=b"null"))
            environment = dict(
                os.environ,
                **properties(server, "ARCO_SFTP_TEST_"),
                **properties(ed_jump, "ARCO_SFTP_TEST_ED_"),
                **properties(ec_jump, "ARCO_SFTP_TEST_EC_"),
                **properties(scp, "ARCO_SCP_TEST_"),
                **properties(scp_links, "ARCO_SCP_LINK_"),
                **properties(link_server, "ARCO_SFTP_LINK_"),
                **properties(reversed_server, "ARCO_SFTP_REVERSED_"),
                **digest_proxies,
                ARCO_SCP_TEST_HTTP_DIGEST=f"http://127.0.0.1:{scp_digest.server_address[1]}",
                ARCO_SCP_TEST_SOCKS_CHAP=f"socks5h://127.0.0.1:{scp_chap.server_address[1]}",
                ARCO_SFTP_TEST_ROOT=str(root),
                ARCO_SFTP_RANGE_TRACE=str(server.range_trace),
                ARCO_SFTP_TEST_KEY=str(key),
                ARCO_SFTP_TEST_ED_KEY=str(ed_path),
                ARCO_SFTP_TEST_EC_KEY=str(ec_path),
                ARCO_SFTP_TEST_HTTP=f"http://127.0.0.1:{http.server_address[1]}",
                ARCO_SFTP_TEST_SOCKS=f"socks5h://127.0.0.1:{socks.server_address[1]}",
                ARCO_SFTP_TEST_HTTP_AUTH=f"http://127.0.0.1:{http_auth.server_address[1]}",
                ARCO_SFTP_TEST_HTTP_BASIC_AUTH=f"http://127.0.0.1:{http_basic.server_address[1]}",
                ARCO_SFTP_TEST_SOCKS_AUTH=f"socks5h://127.0.0.1:{socks_auth.server_address[1]}",
                ARCO_SFTP_TEST_SOCKS_CHAP_AUTH=f"socks5h://127.0.0.1:{socks_chap.server_address[1]}",
                ARCO_SFTP_TEST_SOCKS4=f"socks4://127.0.0.1:{socks4.server_address[1]}",
                ARCO_SFTP_TEST_SOCKS4A=f"socks4a://127.0.0.1:{socks4a.server_address[1]}",
                ARCO_SFTP_TEST_TELNET=f"telnet://127.0.0.1:{telnet.server_address[1]}",
                ARCO_SFTP_TEST_TELNET_DEFAULT=f"telnet://127.0.0.1:{telnet_default.server_address[1]}",
                ARCO_SFTP_TEST_LOCAL_COMMAND=local_proxy + relay_arguments + " ssh.invalid",
                ARCO_SFTP_TEST_LOCAL_RESOLVED=local_proxy + relay_arguments + " 127.0.0.1",
                ARCO_SFTP_TEST_LOCAL_STALL=local_proxy + f' stall "{local_proxy_marker.as_posix()}"',
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
            if os.name == "nt":
                import _winapi

                pid = int(local_proxy_marker.read_text(encoding="ascii"))
                try:
                    handle = _winapi.OpenProcess(0x00100000, False, pid)
                except OSError as error:
                    if error.winerror != 87:
                        raise
                else:
                    try:
                        result = _winapi.WaitForSingleObject(handle, 3000)
                        if result != 0:
                            raise AssertionError("The timed-out local proxy process is still running")
                    finally:
                        _winapi.CloseHandle(handle)
            if args.net_exe:
                reference_local_proxy_script(args.net_exe, work, server, local_proxy)
                reference_link_script(args.net_exe, work, link_server)
            scp_command = [str(args.harness.resolve()), "reference_scp_"] if args.harness else [
                "cargo", "test", "--locked", "-p", "arco-windows", "--lib", "reference_scp_", "--",
            ]
            subprocess.run([*scp_command, "--ignored", "--nocapture", "--test-threads=1"], env=environment, check=True, timeout=60)
            if not args.backend_only:
                server.transfer_aliases.update({
                    "Relay:": "mapped-relay-input",
                    "relay:": "mapped-relay-output",
                    "Q?.txt": "mapped-relay-question-input",
                    "q?.txt": "mapped-relay-question-output",
                })
                for test in (APP_TEST, MAPPED_SYNC_TEST, WATCH_TEST, WATCH_DELETE_TEST,
                             "app::ftp_sync::tests::sftp_job_transfer_options_apply_speed_masks_and_names",
                             "app::ftp_sync::tests::sftp_background_copies_transfer_independent_files_in_parallel",
                             "app::ftp_panel::tests::mounted_sftp_skips_link_targets_in_mirror_and_recursive_delete",
                             "app::ftp_edit::tests::ssh_drafts_publish_and_keep_conflicting_edits"):
                    app_command = [str(args.app_harness.resolve()), test] if args.app_harness else [
                        "cargo", "test", "--locked", "-p", "arco-app", "--bin", "arco-commander",
                        test, "--",
                    ]
                    result = subprocess.run(
                        [*app_command, "--exact", "--ignored", "--nocapture"],
                        env=environment, timeout=240, capture_output=True,
                        encoding="utf-8", errors="replace",
                        creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
                    )
                    print(result.stdout)
                    if result.returncode:
                        print(result.stderr, file=sys.stderr)
                        result.check_returncode()
                    assert "1 passed" in result.stdout
                for fixture in (root, scp_root):
                    uploaded = fixture / "round trip" / "outgoing" / "ä # export.txt"
                    assert uploaded.read_bytes() == b"from local"
                print("Application reference passed: recursive search and bidirectional SFTP and SCP sync")
            if args.net_exe:
                reference_script(args.net_exe, work, root, environment)
                reference_unix_metadata(args.net_exe, work, server, "sftp")
                reference_unix_metadata(args.net_exe, work, scp, "scp")
                reference_transfer_names(args.net_exe, work, server, environment)
                reference_sync(args.net_exe, work, root, environment)
                reference_script_watch(args.net_exe, work, root, environment)
                reference_script_watch(args.net_exe, work, root, environment, mapped=True)
            assert set(server.auth_attempts) == {"arco", "interactive", "mfa"}, server.auth_attempts
            for jump in (ed_jump, ec_jump):
                assert set(jump.auth_attempts) == {"jump"}, jump.auth_attempts


if __name__ == "__main__":
    main()
