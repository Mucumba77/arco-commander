#![cfg(windows)]

use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
    sync::atomic::AtomicBool,
};

fn extract(archive: &Path, output: &Path, password: Option<&str>) -> bool {
    let mut child = Command::new(archive)
        .arg("--extract-to")
        .arg(output)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(password) = password {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(password.as_bytes())
            .unwrap();
    }
    child.wait().unwrap().success()
}

#[test]
fn generated_zip_sfx_extracts_and_protects_existing_files() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path();
    let source = root.join("Größe");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("Test.txt"), b"Arco ZIP-SFX").unwrap();
    let cancel = AtomicBool::new(false);
    let stub = Path::new(env!("CARGO_BIN_EXE_arco-zip-sfx"));
    for (name, password) in [("plain", None), ("aes", Some("Prüfung!"))] {
        let archive = root.join(format!("{name}.exe"));
        let output = root.join(name);
        fs::create_dir(&output).unwrap();
        arco_core::archives::create_zip_sfx(
            std::slice::from_ref(&source),
            root,
            &archive,
            stub,
            password,
            &cancel,
        )
        .unwrap();
        if password.is_some() {
            assert!(!extract(&archive, &output, Some("wrong")));
            assert_eq!(fs::read_dir(&output).unwrap().count(), 0);
        }
        assert!(extract(&archive, &output, password));
        let result = output.join("Größe").join("Test.txt");
        assert_eq!(fs::read(&result).unwrap(), b"Arco ZIP-SFX");
        fs::write(&result, b"keep").unwrap();
        assert!(!extract(&archive, &output, password));
        assert_eq!(fs::read(result).unwrap(), b"keep");
    }
}
