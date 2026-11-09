#![cfg(windows)]

use arco_core::{descriptions, files};
use std::fs;
use std::io;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, UNIX_EPOCH};

#[test]
#[ignore = "requires ARCO_TEST_SECOND_VOLUME on a writable second volume, e.g. exFAT"]
fn reference_cross_volume_move_preserves_comments_attributes_and_size() {
    let second = PathBuf::from(
        std::env::var_os("ARCO_TEST_SECOND_VOLUME")
            .expect("set ARCO_TEST_SECOND_VOLUME to a writable folder on a second volume"),
    );
    let from = tempfile::tempdir().unwrap();
    let to = tempfile::tempdir_in(&second).unwrap();
    assert_ne!(
        arco_windows::volume(from.path()).unwrap().identifier,
        arco_windows::volume(to.path()).unwrap().identifier,
        "the test requires a real volume boundary"
    );
    verify_copy_remove_move(from.path(), to.path(), arco_windows::rename_noreplace);
}

#[test]
fn reference_move_fallback_preserves_comments_attributes_and_size_on_ntfs() {
    let from = tempfile::tempdir().unwrap();
    let to = tempfile::tempdir().unwrap();
    verify_copy_remove_move(from.path(), to.path(), |_, _| {
        Err(io::ErrorKind::CrossesDevices.into())
    });
}

fn verify_copy_remove_move(from: &Path, to: &Path, rename: files::NativeRename) {
    let source = from.join("Größe mit Leerzeichen.txt");
    fs::write(&source, b"file data").unwrap();
    let prepare: descriptions::ApplyAttributes = |_, _| Ok(());
    descriptions::set(&source, None, "Kommentar\nZweite Zeile", prepare).unwrap();
    let created = UNIX_EPOCH + Duration::from_secs(1_699_000_000);
    arco_windows::edit_metadata(
        &source,
        arco_windows::MetadataEdit {
            set: 2,
            created: Some(created),
            ..Default::default()
        },
    )
    .unwrap();
    let report = files::transfer(
        std::slice::from_ref(&source),
        to,
        files::Transfer::Move,
        files::TransferPolicy {
            descriptions: Some(prepare),
            ..files::Conflict::Fail.into()
        },
        &AtomicBool::new(false),
        Some(rename),
        |_| {},
    )
    .unwrap();
    assert_eq!(report.completed, 1, "{report:?}");
    assert!(report.errors.is_empty(), "{report:?}");
    assert!(!source.exists());
    let destination = to.join(source.file_name().unwrap());
    let entry =
        files::Entry::read(destination.clone(), source.file_name().unwrap().to_owned()).unwrap();
    assert_eq!(entry.size, Some(9));
    assert!(entry.hidden);
    assert_ne!(
        fs::metadata(&destination).unwrap().created().unwrap(),
        created
    );
    assert!(arco_windows::file_allocation(&destination).unwrap().bytes >= 9);
    assert_eq!(
        descriptions::read(to).unwrap()[&descriptions::key(&entry.name)],
        "Kommentar\nZweite Zeile"
    );
    assert!(
        !descriptions::read(from)
            .unwrap()
            .contains_key(&descriptions::key(&entry.name))
    );
}

#[test]
fn reference_ntfs_directory_copy_preserves_dos_attributes_times_streams_and_hardlinks() {
    const READONLY: u32 = 1;
    const HIDDEN: u32 = 2;
    const SYSTEM: u32 = 4;
    const ARCHIVE: u32 = 32;
    const DOS: u32 = READONLY | HIDDEN | SYSTEM | ARCHIVE;

    let fixture = tempfile::tempdir().unwrap();
    let from = fixture.path().join("Quelle ü");
    let to = fixture.path().join("Ziel");
    fs::create_dir(&from).unwrap();
    fs::create_dir(&to).unwrap();
    let file = from.join("Daten.txt");
    let alias = from.join("Daten verknüpft.txt");
    fs::write(&file, b"content").unwrap();
    fs::write(named_stream(&from), b"directory metadata").unwrap();
    fs::write(named_stream(&file), b"file metadata").unwrap();
    fs::hard_link(&file, &alias).unwrap();
    let modified = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let accessed = UNIX_EPOCH + Duration::from_secs(1_700_000_100);
    let created = UNIX_EPOCH + Duration::from_secs(1_699_000_000);
    for (path, flags) in [
        (&from, HIDDEN | SYSTEM),
        (&file, HIDDEN | SYSTEM | READONLY),
    ] {
        arco_windows::edit_metadata(
            path,
            arco_windows::MetadataEdit {
                set: flags,
                clear: ARCHIVE,
                created: Some(created),
                modified: Some(modified),
                accessed: Some(accessed),
                ..Default::default()
            },
        )
        .unwrap();
    }

    let report = files::transfer(
        std::slice::from_ref(&from),
        &to,
        files::Transfer::Copy,
        files::TransferPolicy {
            ntfs: files::NtfsCopyOptions {
                hardlinks: true,
                streams: true,
                creation_time: true,
                ..Default::default()
            },
            ..files::Conflict::Fail.into()
        },
        &AtomicBool::new(false),
        None,
        |_| {},
    )
    .unwrap();
    assert_eq!(report.completed, 1, "{report:?}");
    assert!(report.errors.is_empty(), "{report:?}");

    let copied = to.join(from.file_name().unwrap());
    let copied_file = copied.join(file.file_name().unwrap());
    let copied_alias = copied.join(alias.file_name().unwrap());
    for (path, flags) in [
        (&copied, HIDDEN | SYSTEM),
        (&copied_file, HIDDEN | SYSTEM | READONLY),
        (&copied_alias, HIDDEN | SYSTEM | READONLY),
    ] {
        let metadata = fs::metadata(path).unwrap();
        assert_eq!(metadata.file_attributes() & DOS, flags);
        assert_eq!(metadata.modified().unwrap(), modified);
        assert_eq!(metadata.created().unwrap(), created);
    }
    assert_eq!(
        fs::read(named_stream(&copied)).unwrap(),
        b"directory metadata"
    );
    assert_eq!(
        fs::read(named_stream(&copied_file)).unwrap(),
        b"file metadata"
    );
    let original = arco_windows::file_allocation(&copied_file)
        .unwrap()
        .hardlink_id;
    assert!(original.is_some());
    assert_eq!(
        arco_windows::file_allocation(&copied_alias)
            .unwrap()
            .hardlink_id,
        original
    );
    for path in [&file, &copied_file] {
        arco_windows::edit_metadata(
            path,
            arco_windows::MetadataEdit {
                clear: READONLY,
                ..Default::default()
            },
        )
        .unwrap();
    }
}

fn named_stream(path: &Path) -> PathBuf {
    let mut path = path.as_os_str().to_owned();
    path.push(":arco-metadata");
    PathBuf::from(path)
}

#[test]
fn reference_f5_copies_directory_junctions_without_traversing_their_targets() {
    let fixture = tempfile::tempdir().unwrap();
    let referent = fixture.path().join("Ordner mit Daten");
    let missing = fixture.path().join("Fehlender Ordner");
    let source = fixture.path().join("Junction Größe");
    let dangling = fixture.path().join("Junction ins Leere");
    let target = fixture.path().join("Ziel");
    for directory in [&referent, &missing, &target] {
        fs::create_dir(directory).unwrap();
    }
    fs::write(referent.join("sentinel.txt"), b"original").unwrap();
    create_junction(&referent.join("Schleife"), &referent);
    create_junction(&source, &referent);
    create_junction(&dangling, &missing);
    fs::remove_dir(&missing).unwrap();

    let report = files::transfer(
        &[source.clone(), dangling.clone()],
        &target,
        files::Transfer::Copy,
        junction_policy(files::Conflict::Fail),
        &AtomicBool::new(false),
        None,
        |_| {},
    )
    .unwrap();
    assert_eq!(report.completed, 2, "{report:?}");
    assert!(report.errors.is_empty(), "{report:?}");
    let copied = target.join(source.file_name().unwrap());
    let copied_dangling = target.join(dangling.file_name().unwrap());
    assert!(files::is_link(&fs::symlink_metadata(&copied).unwrap()));
    assert!(files::is_link(
        &fs::symlink_metadata(&copied_dangling).unwrap()
    ));
    assert_eq!(
        fs::canonicalize(&copied).unwrap(),
        fs::canonicalize(&referent).unwrap()
    );
    assert_eq!(
        fs::read_link(&copied_dangling).unwrap(),
        fs::read_link(&dangling).unwrap()
    );
    assert!(!missing.exists());
    assert_eq!(
        fs::read(referent.join("sentinel.txt")).unwrap(),
        b"original"
    );

    let disabled = fixture.path().join("Option aus");
    fs::create_dir(&disabled).unwrap();
    let report = files::transfer(
        std::slice::from_ref(&source),
        &disabled,
        files::Transfer::Copy,
        files::Conflict::Fail,
        &AtomicBool::new(false),
        None,
        |_| {},
    )
    .unwrap();
    assert_eq!(report.errors.len(), 1, "{report:?}");
    assert_eq!(fs::read_dir(disabled).unwrap().count(), 0);
}

#[test]
fn reference_f5_junction_conflicts_leave_existing_targets_in_place() {
    let fixture = tempfile::tempdir().unwrap();
    let referent = fixture.path().join("source target");
    let source = fixture.path().join("link");
    let target = fixture.path().join("destination");
    fs::create_dir(&referent).unwrap();
    fs::create_dir(&target).unwrap();
    create_junction(&source, &referent);
    let existing = target.join("link");
    fs::create_dir(&existing).unwrap();
    fs::write(existing.join("sentinel.txt"), b"untouched").unwrap();
    let transfer = |conflict| {
        files::transfer(
            std::slice::from_ref(&source),
            &target,
            files::Transfer::Copy,
            junction_policy(conflict),
            &AtomicBool::new(false),
            None,
            |_| {},
        )
        .unwrap()
    };
    for conflict in [
        files::Conflict::Fail,
        files::Conflict::Replace,
        files::Conflict::Newer,
        files::Conflict::Larger,
        files::Conflict::Append,
    ] {
        let report = transfer(conflict);
        assert_eq!(report.completed, 0, "{report:?}");
        assert_eq!(report.errors.len(), 1, "{report:?}");
        assert_eq!(
            fs::read(existing.join("sentinel.txt")).unwrap(),
            b"untouched"
        );
    }
    let report = transfer(files::Conflict::Skip);
    assert_eq!(report.skipped, 1, "{report:?}");
    assert!(report.errors.is_empty(), "{report:?}");
    let report = transfer(files::Conflict::Rename);
    assert_eq!(report.completed, 1, "{report:?}");
    let renamed = target.join("link (2)");
    assert_eq!(
        fs::canonicalize(renamed).unwrap(),
        fs::canonicalize(referent).unwrap()
    );
    assert_eq!(
        fs::read(existing.join("sentinel.txt")).unwrap(),
        b"untouched"
    );
}

#[test]
fn reference_f5_junction_publication_rejects_a_late_target() {
    let fixture = tempfile::tempdir().unwrap();
    let referent = fixture.path().join("source target");
    let source = fixture.path().join("link");
    let target = fixture.path().join("destination");
    fs::create_dir(&referent).unwrap();
    fs::create_dir(&target).unwrap();
    create_junction(&source, &referent);
    let existing = target.join("link");
    let report = files::transfer(
        std::slice::from_ref(&source),
        &target,
        files::Transfer::Copy,
        junction_policy(files::Conflict::Fail),
        &AtomicBool::new(false),
        None,
        |_| fs::write(&existing, b"created while copying").unwrap(),
    )
    .unwrap();
    assert_eq!(report.completed, 0, "{report:?}");
    assert_eq!(report.errors.len(), 1, "{report:?}");
    assert_eq!(fs::read(&existing).unwrap(), b"created while copying");
    assert_eq!(fs::read_dir(&target).unwrap().count(), 1);
    assert_eq!(
        fs::canonicalize(source).unwrap(),
        fs::canonicalize(referent).unwrap()
    );
}

fn junction_policy(conflict: files::Conflict) -> files::TransferPolicy<'static> {
    files::TransferPolicy {
        ntfs: files::NtfsCopyOptions {
            copy_links: true,
            ..Default::default()
        },
        ..conflict.into()
    }
}

fn create_junction(link: &Path, target: &Path) {
    use std::os::windows::process::CommandExt;
    let result = std::process::Command::new("cmd.exe")
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .args(["/d", "/v:off", "/c"])
        .env("ARCO_REFERENCE_JUNCTION", link)
        .env("ARCO_REFERENCE_JUNCTION_TARGET", target)
        .raw_arg(r#"mklink /J "%ARCO_REFERENCE_JUNCTION%" "%ARCO_REFERENCE_JUNCTION_TARGET%""#)
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
}
