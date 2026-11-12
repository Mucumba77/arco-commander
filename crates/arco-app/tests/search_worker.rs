use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

fn search_worker(request: serde_json::Value) -> Vec<serde_json::Value> {
    let mut worker = Command::new(env!("CARGO_BIN_EXE_arco-commander"))
        .arg("--search-worker")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = worker.stdin.take().unwrap();
    writeln!(input, "{request}").unwrap();
    let messages: Vec<serde_json::Value> = BufReader::new(worker.stdout.take().unwrap())
        .lines()
        .map(|line| serde_json::from_str(&line.unwrap()).unwrap())
        .collect();
    drop(input);
    let status = worker.wait().unwrap();
    assert!(
        status.success(),
        "search worker exited with {status}: {messages:?}"
    );
    messages
}

#[test]
fn opted_in_progress_is_streamed_before_results_with_final_counts() {
    let fixture = tempfile::tempdir().unwrap();
    std::fs::write(fixture.path().join("Größe.txt"), b"contents").unwrap();
    std::fs::write(fixture.path().join("other.md"), b"skip").unwrap();
    let messages = search_worker(serde_json::json!({
        "progress": true, "options": {"names": "*.txt"},
        "roots": [fixture.path().as_os_str()], "show_hidden": false, "plugins": []
    }));
    let progress = messages
        .iter()
        .filter_map(|message| message.get("Progress"))
        .collect::<Vec<_>>();
    let last = progress.last().expect("a final progress snapshot");
    assert_eq!(last["examined"], 2);
    assert_eq!(last["matched"], 1);
    assert_eq!(last["errors"], 0);
    let last_progress = messages
        .iter()
        .rposition(|message| message.get("Progress").is_some())
        .unwrap();
    let first_result = messages
        .iter()
        .position(|message| message.get("Entry").is_some())
        .unwrap();
    assert!(last_progress < first_result);
    assert_eq!(
        messages.last().unwrap(),
        &serde_json::json!({"Done": {"cancelled": false}})
    );
}

#[test]
fn separate_search_process_returns_matching_file_metadata_and_exits() {
    let fixture = tempfile::tempdir().unwrap();
    let found = fixture.path().join("Größe.txt");
    std::fs::write(&found, b"find this text").unwrap();
    std::fs::write(fixture.path().join("other.txt"), b"nothing here").unwrap();
    let request = serde_json::json!({
        "options": {"names": "*.txt", "content_mode": 1, "content": "find this"},
        "roots": [fixture.path().as_os_str()],
        "show_hidden": false,
        "plugins": []
    });
    let messages = search_worker(request);
    assert_eq!(messages.len(), 2, "{messages:?}");
    let entry = &messages[0]["Entry"];
    let path = serde_json::from_value::<OsString>(entry["path"].clone()).unwrap();
    assert_eq!(std::path::PathBuf::from(path), found);
    assert_eq!(entry["kind"], "File");
    assert_eq!(entry["size"], 14);
    assert_eq!(
        messages[1],
        serde_json::json!({"Done": {"cancelled": false}})
    );
}

#[test]
fn separate_search_process_reads_encrypted_and_split_seven_zip() {
    use arco_core::archive_view;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicBool;

    let fixture = tempfile::tempdir().unwrap();
    let base = fixture.path();
    let docs = base.join("docs");
    std::fs::create_dir(&docs).unwrap();
    let source = docs.join("Größe.txt");
    let body = "Paketgröße nach Inhalt";
    let encoded = [0xff, 0xfe]
        .into_iter()
        .chain(body.encode_utf16().flat_map(u16::to_le_bytes))
        .collect::<Vec<_>>();
    std::fs::write(&source, &encoded).unwrap();
    std::fs::write(docs.join("FalscherInhalt.txt"), b"no match").unwrap();
    std::fs::write(docs.join("FalscherTyp.md"), &encoded).unwrap();
    let paths = [docs];
    let cancel = AtomicBool::new(false);
    let password = "search-fixture";
    let encrypted = base.join("Geschützt.7z");
    archive_view::pack_7z(&paths, base, &encrypted, Some(password), &cancel).unwrap();
    let split = base.join("Teile.7z");
    assert!(archive_view::pack_split(&paths, base, &split, 83, None, &cancel).unwrap() > 1);
    for (archive, password) in [
        (encrypted, Some(password)),
        (base.join("Teile.7z.001"), None),
    ] {
        let messages = search_worker(serde_json::json!({
            "options": {"names": "*.txt", "content_mode": 1, "content": "Paketgröße"},
            "roots": [OsString::from("docs")],
            "show_hidden": false,
            "plugins": [],
            "archive": {"file": archive, "folder": "docs", "catalog_password": password},
            "password": password
        }));
        assert_eq!(messages.len(), 2, "{messages:?}");
        let entry = &messages[0]["Entry"];
        let path = serde_json::from_value::<OsString>(entry["path"].clone()).unwrap();
        assert_eq!(PathBuf::from(path), Path::new("docs").join("Größe.txt"));
        assert_eq!(entry["size"], encoded.len());
        assert_eq!(
            messages[1],
            serde_json::json!({"Done": {"cancelled": false}})
        );
    }
}

#[cfg(windows)]
#[test]
#[ignore = "requires 7-Zip in ARCO_7ZIP_PATH"]
fn separate_search_process_reads_gzip_content() {
    use std::os::windows::process::CommandExt;
    use std::path::PathBuf;

    let fixture = tempfile::tempdir().unwrap();
    let name = "Nachweis.txt";
    std::fs::write(fixture.path().join(name), b"Gzip archive search fixture").unwrap();
    let gzip = fixture.path().join("Nachweis.txt.gz");
    let seven_zip = std::env::var_os("ARCO_7ZIP_PATH").expect("7-Zip test executable");
    let output = Command::new(seven_zip)
        .current_dir(fixture.path())
        .args(["a", "-tgzip", "-y"])
        .arg(&gzip)
        .arg(name)
        .creation_flags(0x0800_0000)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let messages = search_worker(serde_json::json!({
        "options": {"names": "*.txt", "content_mode": 1, "content": "archive search"},
        "roots": [OsString::new()],
        "show_hidden": false,
        "plugins": [],
        "archive": {"file": gzip, "folder": ""}
    }));
    assert_eq!(messages.len(), 2, "{messages:?}");
    let path = serde_json::from_value::<OsString>(messages[0]["Entry"]["path"].clone()).unwrap();
    assert_eq!(PathBuf::from(path), PathBuf::from(name));
    assert_eq!(
        messages[1],
        serde_json::json!({"Done": {"cancelled": false}})
    );
}
