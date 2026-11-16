use super::tc_field;
use crate::rename::FieldReference;
use std::fs;

#[test]
fn complete_internal_catalog_includes_name_decoding_path_length_and_case_metadata() {
    let fixture = tempfile::tempdir().unwrap();
    let path = fixture.path().join("😀-caf%C3%A9.txt");
    fs::write(&path, b"name").unwrap();
    let metadata = fs::metadata(&path).unwrap();
    assert_eq!(
        tc_field("tc.decode html characters (utf-8)", &path, &metadata)
            .unwrap()
            .unwrap()
            .display,
        "😀-café"
    );
    let full = tc_field("tc.fullpath", &path, &metadata)
        .unwrap()
        .unwrap()
        .display;
    assert_eq!(
        tc_field("tc.path length", &path, &metadata)
            .unwrap()
            .unwrap()
            .display,
        full.encode_utf16().count().to_string()
    );
    assert_eq!(
        tc_field("tc.case sensitive in folder", &path, &metadata)
            .unwrap()
            .unwrap()
            .display,
        "false"
    );
    assert!(matches!(
        tc_field("tc.case sensitive name", &path, &metadata)
            .unwrap()
            .unwrap()
            .key,
        super::WdxKey::Boolean(_)
    ));
    let random = tc_field("tc.random number", &path, &metadata)
        .unwrap()
        .unwrap();
    assert!(matches!(random.key, super::WdxKey::Integer(0..=99999)));
    for field in [
        "random number",
        "decode html characters (text)",
        "decode html characters (utf-8)",
        "decode html characters (utf-16)",
        "path length",
        "case sensitive name",
        "case sensitive in folder",
    ] {
        assert!(super::TC_METADATA_FIELDS.contains(&field));
    }
}

#[test]
fn builtin_metadata_values_and_date_formats_are_available_without_a_dll() {
    let fixture = tempfile::tempdir().unwrap();
    let path = fixture.path().join("Grüße.txt");
    fs::write(&path, b"abc").unwrap();
    let metadata = fs::metadata(&path).unwrap();
    let size = tc_field("tc.size", &path, &metadata).unwrap().unwrap();
    assert_eq!(
        FieldReference::parse("tc.size._X2")
            .unwrap()
            .render(Some(&size))
            .unwrap()
            .as_deref(),
        Some("03")
    );
    assert_eq!(
        tc_field("tc.ext", &path, &metadata)
            .unwrap()
            .unwrap()
            .display,
        "txt"
    );
    assert_eq!(
        tc_field("tc.name", &path, &metadata)
            .unwrap()
            .unwrap()
            .display,
        "Grüße"
    );
    assert_eq!(
        tc_field("tc.fullname", &path, &metadata)
            .unwrap()
            .unwrap()
            .display,
        "Grüße.txt"
    );
    assert!(
        tc_field("tc.path", &path, &metadata)
            .unwrap()
            .unwrap()
            .display
            .ends_with(std::path::MAIN_SEPARATOR)
    );
    assert_eq!(
        tc_field("tc.attributestr", &path, &metadata)
            .unwrap()
            .unwrap()
            .display
            .len(),
        4
    );
    let date = tc_field("tc.writedate", &path, &metadata).unwrap().unwrap();
    let expression = FieldReference::parse("tc.writedate.Y-M-D h:m:s.v").unwrap();
    let formatted = expression.render(Some(&date)).unwrap().unwrap();
    assert_eq!(formatted.len(), 23);
    assert!(formatted.starts_with(&date.display));
    assert!(tc_field("tc.unknown", &path, &metadata).is_err());
    assert_eq!(
        tc_field("tc.size.kbytes", &path, &metadata)
            .unwrap()
            .unwrap()
            .display,
        (3_f64 / 1024.0).to_string()
    );
    assert!(tc_field("Other.size", &path, &metadata).unwrap().is_none());
}

#[cfg(windows)]
#[test]
fn native_dos_names_and_version_resources_do_not_load_the_file_as_a_program() {
    let fixture = tempfile::tempdir().unwrap();
    let path = fixture.path().join("Eine lange Textdatei.txt");
    fs::write(&path, b"not a PE image").unwrap();
    let metadata = fs::metadata(&path).unwrap();
    assert!(
        !tc_field("tc.dosname", &path, &metadata)
            .unwrap()
            .unwrap()
            .display
            .is_empty()
    );
    assert!(
        tc_field("tc.versionstring", &path, &metadata)
            .unwrap()
            .is_none()
    );
    let system =
        std::path::PathBuf::from(std::env::var_os("WINDIR").unwrap()).join("System32/kernel32.dll");
    let metadata = fs::metadata(&system).unwrap();
    let version = tc_field("tc.versionnr", &system, &metadata)
        .unwrap()
        .unwrap()
        .display;
    assert_eq!(
        version
            .split('.')
            .filter(|part| part.parse::<u16>().is_ok())
            .count(),
        4
    );
    assert!(
        !tc_field("tc.versionstring", &system, &metadata)
            .unwrap()
            .unwrap()
            .display
            .is_empty()
    );
}

#[cfg(windows)]
#[test]
#[ignore = "requires FFmpeg in ARCO_FFMPEG_PATH; creates local reference files without playing them"]
fn audio_fields_match_ffmpeg_tags_in_seven_independent_container_formats() {
    use super::tc_field_with_cancel;
    use std::os::windows::process::CommandExt;
    use std::process::Command;
    use std::sync::atomic::AtomicBool;
    let fixture = tempfile::tempdir().unwrap();
    let tool = std::env::var_os("ARCO_FFMPEG_PATH").unwrap();
    for format in ["mp3", "flac", "ogg", "opus", "m4a", "wav", "wv"] {
        let path = fixture.path().join(format!("Grüße.{format}"));
        let mut command = Command::new(&tool);
        command.args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "anullsrc=r=8000:cl=mono",
            "-t",
            "0.05",
        ]);
        for (key, value) in [
            ("title", "Reference Title"),
            ("artist", "Reference Artist"),
            ("album", "Reference Album"),
            ("date", "2024"),
            ("year", "2024"),
            ("track", "7"),
            ("genre", "Rock"),
            ("comment", "Reference Description"),
            ("description", "Reference Description"),
        ] {
            command.arg("-metadata").arg(format!("{key}={value}"));
        }
        let output = command
            .arg(&path)
            .creation_flags(0x0800_0000)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{format}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let metadata = fs::metadata(&path).unwrap();
        for (field, expected) in [
            ("Tag Present", "true"),
            ("Title", "Reference Title"),
            ("Artist", "Reference Artist"),
            ("Album", "Reference Album"),
            ("Release Year", "2024"),
            ("Track Nr", "7"),
            ("Genre", "Rock"),
            ("Description", "Reference Description"),
        ] {
            let value = tc_field(&format!("tc.{field}"), &path, &metadata)
                .unwrap()
                .unwrap_or_else(|| panic!("{format}: missing {field}"));
            assert_eq!(value.display, expected, "{format}: {field}");
        }
        assert!(
            tc_field_with_cancel("tc.Title", &path, &metadata, &AtomicBool::new(true)).is_err()
        );
    }
}

#[cfg(windows)]
#[test]
fn zone_fields_read_only_the_zone_transfer_section_and_default_to_zero() {
    use crate::plugin::WdxKey;
    let fixture = tempfile::tempdir().unwrap();
    let path = fixture.path().join("probe.txt");
    fs::write(&path, b"data").unwrap();
    let metadata = fs::metadata(&path).unwrap();
    assert_eq!(
        tc_field("tc.zoneid", &path, &metadata)
            .unwrap()
            .unwrap()
            .key,
        WdxKey::Integer(0)
    );
    let mut stream = path.as_os_str().to_os_string();
    stream.push(":Zone.Identifier");
    fs::write(&stream, b"[Other]\nZoneId=4\n[ZoneTransfer]\nZoneId=3\nHostUrl=https://example.invalid/a\nReferrerUrl=https://example.invalid/\n").unwrap();
    assert_eq!(
        tc_field("tc.zoneid", &path, &metadata)
            .unwrap()
            .unwrap()
            .key,
        WdxKey::Integer(3)
    );
    assert_eq!(
        tc_field("tc.hosturl", &path, &metadata)
            .unwrap()
            .unwrap()
            .display,
        "https://example.invalid/a"
    );
    assert_eq!(fs::read(&path).unwrap(), b"data");
}
