use crate::{
    archive_settings::{Settings, ZipCipher, ZipMethod},
    archive_view, archives,
};
use std::{
    fs,
    io::{Cursor, Write},
    path::Path,
    sync::atomic::AtomicBool,
};
use zip::{
    AesMode, CompressionMethod, ZipWriter, unstable::write::FileOptionsExt,
    write::SimpleFileOptions,
};

#[test]
#[ignore = "requires the pinned acefile adapter via ARCO_ARCHIVE_PYTHON"]
fn encrypted_ace_modes_reject_wrong_passwords_and_support_bounded_sfx_prefixes() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("sample.txt");
    let payload = "ACE encryption, CRC and adapter regression.\n".repeat(80);
    fs::write(&file, &payload).unwrap();
    let cancel = AtomicBool::new(false);
    for mode in 0..=2 {
        let archive = root.path().join(format!("mode{mode}.ace"));
        crate::legacy_archives::pack_ace_encrypted(
            std::slice::from_ref(&file),
            root.path(),
            &archive,
            mode,
            "paß word",
            &cancel,
        )
        .unwrap();
        let catalog = archive_view::Catalog::read(&archive, &cancel).unwrap();
        assert!(catalog.contains_encrypted_entries());
        assert!(archive_view::supports_password(&archive));
        assert!(archive_view::test(&archive, None, &cancel).is_err());
        assert!(archive_view::test(&archive, Some("wrong"), &cancel).is_err());
        let output = tempfile::tempdir().unwrap();
        assert!(
            archive_view::extract_all(&archive, output.path(), Some("wrong"), &cancel).is_err()
        );
        assert!(!output.path().join("sample.txt").exists());
        archive_view::extract_all(&archive, output.path(), Some("paß word"), &cancel).unwrap();
        assert_eq!(
            fs::read_to_string(output.path().join("sample.txt")).unwrap(),
            payload
        );
        let sfx = root.path().join(format!("mode{mode}.exe"));
        let mut data = vec![0; 512];
        data[..2].copy_from_slice(b"MZ");
        data.extend(fs::read(&archive).unwrap());
        fs::write(&sfx, data).unwrap();
        assert!(archive_view::is_openable(&sfx));
        archive_view::test(&sfx, Some("paß word"), &cancel).unwrap();
    }
}

#[test]
#[ignore = "requires the pinned acefile adapter"]
fn ace_volumes_split_inside_files_rebuild_and_reject_missing_or_changed_parts() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("data.bin");
    let data: Vec<u8> = (0..2048)
        .map(|index| ((index * 79 + index / 17) % 251) as u8)
        .collect();
    fs::write(&source, &data).unwrap();
    let cancel = AtomicBool::new(false);
    for password in [None, Some("secret")] {
        let directory = root.path().join(if password.is_some() {
            "encrypted"
        } else {
            "plain"
        });
        fs::create_dir(&directory).unwrap();
        let first = directory.join("fixture.ace");
        let count = crate::legacy_archives::pack_ace_volumes(
            std::slice::from_ref(&source),
            root.path(),
            &first,
            200,
            0,
            password,
            &cancel,
        )
        .unwrap();
        assert!(count > 2);
        let catalog = archive_view::Catalog::read(&first, &cancel).unwrap();
        assert_eq!(catalog.storage_paths().count(), count);
        let output = tempfile::tempdir().unwrap();
        archive_view::extract_all(&first, output.path(), password, &cancel).unwrap();
        assert_eq!(fs::read(output.path().join("data.bin")).unwrap(), data);
        let second = directory.join("fixture.c00");
        let stored = fs::read(&second).unwrap();
        fs::remove_file(&second).unwrap();
        assert!(archive_view::test(&first, password, &cancel).is_err());
        assert!(!catalog.unchanged(&first));
        fs::write(&second, stored).unwrap();
        archive_view::edit(
            &first,
            archives::ZipEdit::Rename("data.bin".into(), "renamed.bin".into()),
            password,
            &cancel,
        )
        .unwrap();
        archive_view::test(&first, password, &cancel).unwrap();
        let fresh = archive_view::Catalog::read(&first, &cancel).unwrap();
        assert_eq!(fresh.rows(Path::new("")).unwrap()[0].name, "renamed.bin");
        archive_view::edit(
            &first,
            archives::ZipEdit::Remove(vec!["renamed.bin".into()]),
            password,
            &cancel,
        )
        .unwrap();
        assert_eq!(archive_view::test(&first, password, &cancel).unwrap(), 0);
        assert!(!second.exists());
    }
}

#[test]
#[ignore = "requires the bundled 7-Zip and the ACE/UC2 adapters"]
fn transactional_legacy_and_tar_rewrites_preserve_conflicts_and_handle_empty_archives() {
    use crate::external_archives::{ExternalArchiver, Format};
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("input");
    fs::create_dir(&source).unwrap();
    let folder = source.join("docs");
    fs::create_dir(&folder).unwrap();
    fs::write(folder.join("first.txt"), "original").unwrap();
    let added = source.join("added.txt");
    fs::write(&added, "new contents").unwrap();
    let cancel = AtomicBool::new(false);
    let adapter = ExternalArchiver::discover(&cancel).unwrap();
    for format in ["tar", "tgz", "uc2", "ace"] {
        let archive = root.path().join(format!("fixture.{format}"));
        let password = (format == "ace").then_some("secret");
        match format {
            "tar" => adapter
                .pack(
                    std::slice::from_ref(&folder),
                    &source,
                    &archive,
                    Format::Tar,
                    &cancel,
                )
                .unwrap(),
            "tgz" => adapter
                .pack(
                    std::slice::from_ref(&folder),
                    &source,
                    &archive,
                    Format::TarGzip,
                    &cancel,
                )
                .unwrap(),
            "uc2" => crate::legacy_archives::pack_uc2(
                std::slice::from_ref(&folder),
                &source,
                &archive,
                &cancel,
            )
            .unwrap(),
            _ => crate::legacy_archives::pack_ace_encrypted(
                std::slice::from_ref(&folder),
                &source,
                &archive,
                2,
                "secret",
                &cancel,
            )
            .unwrap(),
        }
        let catalog =
            archive_view::Catalog::read_with_password(&archive, password, &cancel).unwrap();
        assert!(catalog.has_directory(Path::new("docs")));
        archive_view::append(
            &archive,
            Path::new("docs"),
            std::slice::from_ref(&added),
            &source,
            password,
            &cancel,
        )
        .unwrap();
        let original = fs::read(&archive).unwrap();
        assert!(
            archive_view::append(
                &archive,
                Path::new("docs"),
                std::slice::from_ref(&added),
                &source,
                password,
                &cancel
            )
            .is_err()
        );
        assert_eq!(fs::read(&archive).unwrap(), original);
        archive_view::edit(
            &archive,
            archives::ZipEdit::Rename(Path::new("docs/first.txt").into(), "renamed.txt".into()),
            password,
            &cancel,
        )
        .unwrap();
        let output = tempfile::tempdir().unwrap();
        archive_view::extract_all(&archive, output.path(), password, &cancel).unwrap();
        assert_eq!(
            fs::read_to_string(output.path().join("docs/renamed.txt")).unwrap(),
            "original"
        );
        assert_eq!(
            fs::read_to_string(output.path().join("docs/added.txt")).unwrap(),
            "new contents"
        );
        archive_view::edit(
            &archive,
            archives::ZipEdit::Remove(vec!["docs".into()]),
            password,
            &cancel,
        )
        .unwrap();
        assert_eq!(archive_view::test(&archive, password, &cancel).unwrap(), 0);
    }
}

#[test]
fn configurable_zip_methods_ciphers_and_timestamp_preserve_the_source() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("Grüße.txt");
    let payload = "Archive mit Unicode und gleichbleibendem Inhalt.\n".repeat(80);
    fs::write(&source, &payload).unwrap();
    let stamp = filetime::FileTime::from_unix_time(1_700_000_000, 0);
    filetime::set_file_mtime(&source, stamp).unwrap();
    let cancel = AtomicBool::new(false);
    for method in [
        ZipMethod::Store,
        ZipMethod::Deflate,
        ZipMethod::Bzip2,
        ZipMethod::Ppmd,
        ZipMethod::Xz,
        ZipMethod::Zstd,
    ] {
        for cipher in [
            ZipCipher::Aes256,
            ZipCipher::Aes192,
            ZipCipher::Aes128,
            ZipCipher::ZipCrypto,
        ] {
            let settings = Settings {
                zip_method: method,
                zip_cipher: cipher,
                zip_level: 1,
                newest_timestamp: true,
                ..Settings::default()
            };
            let archive = root.path().join(format!("{method:?}-{cipher:?}.zip"));
            archives::create_zip_with_options(
                std::slice::from_ref(&source),
                root.path(),
                &archive,
                Some("paß word"),
                settings,
                &cancel,
                |_| {},
            )
            .unwrap();
            assert_eq!(
                filetime::FileTime::from_last_modification_time(&fs::metadata(&archive).unwrap()),
                stamp
            );
            assert!(archives::test_zip(&archive, Some("wrong"), &cancel).is_err());
            let output = tempfile::tempdir().unwrap();
            assert_eq!(
                archive_view::extract_all(&archive, output.path(), Some("paß word"), &cancel)
                    .unwrap(),
                1
            );
            assert_eq!(
                fs::read_to_string(output.path().join("Grüße.txt")).unwrap(),
                payload
            );
        }
    }
    assert_eq!(fs::read_to_string(source).unwrap(), payload);
}

#[test]
#[ignore = "requires the bundled 7-Zip 26.05+ adapter for LZMA writing"]
fn lzma_zip_uses_private_password_input_and_all_cipher_variants() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("sample.txt");
    fs::write(&source, "source payload").unwrap();
    let cancel = AtomicBool::new(false);
    for cipher in [
        ZipCipher::Aes256,
        ZipCipher::Aes192,
        ZipCipher::Aes128,
        ZipCipher::ZipCrypto,
    ] {
        let archive = root.path().join(format!("{cipher:?}.zip"));
        let options = Settings {
            zip_method: ZipMethod::Lzma,
            zip_cipher: cipher,
            ..Settings::default()
        };
        archives::create_zip_with_options(
            std::slice::from_ref(&source),
            root.path(),
            &archive,
            Some("secret"),
            options,
            &cancel,
            |_| {},
        )
        .unwrap();
        let output = tempfile::tempdir().unwrap();
        archive_view::extract_all(&archive, output.path(), Some("secret"), &cancel).unwrap();
        assert_eq!(
            fs::read_to_string(output.path().join("sample.txt")).unwrap(),
            "source payload"
        );
        assert!(archive_view::test(&archive, Some("wrong"), &cancel).is_err());
    }
}

fn encrypted_brotli(mode: Option<AesMode>) -> Vec<u8> {
    let payload = b"Arco Brotli fixture\n";
    let compressed = [
        139, 9, 128, 65, 114, 99, 111, 32, 66, 114, 111, 116, 108, 105, 32, 102, 105, 120, 116,
        117, 114, 101, 10, 3,
    ];
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let options = match mode {
        Some(mode) => options.with_aes_encryption(mode, "secret"),
        None => options.with_deprecated_encryption(b"secret").unwrap(),
    };
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    writer.start_file("data.txt", options).unwrap();
    writer.write_all(&compressed).unwrap();
    let mut bytes = writer.finish().unwrap().into_inner();
    let central = bytes.windows(4).position(|b| b == b"PK\x01\x02").unwrap();
    let crc = crc32fast::hash(payload);
    let packed = u32::from_le_bytes(bytes[18..22].try_into().unwrap());
    for (base, method, checksum, size, extra_len, name_len, extra_start) in [
        (0, 8, 14, 22, 28, 26, 30),
        (central, 10, 16, 24, 30, 28, 46),
    ] {
        let start = base
            + extra_start
            + usize::from(u16::from_le_bytes(
                bytes[base + name_len..base + name_len + 2]
                    .try_into()
                    .unwrap(),
            ));
        let length = usize::from(u16::from_le_bytes(
            bytes[base + extra_len..base + extra_len + 2]
                .try_into()
                .unwrap(),
        ));
        if mode.is_some() {
            let mut offset = start;
            while offset < start + length {
                let kind = u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap());
                let len = usize::from(u16::from_le_bytes(
                    bytes[offset + 2..offset + 4].try_into().unwrap(),
                ));
                if kind == 0x9901 {
                    bytes[offset + 9..offset + 11].copy_from_slice(&129u16.to_le_bytes());
                }
                offset += 4 + len;
            }
        } else {
            bytes[base + method..base + method + 2].copy_from_slice(&129u16.to_le_bytes());
            // ZipCrypto's encrypted check byte originally used the stored body's
            // CRC. A data-descriptor ZIP instead validates the DOS time byte.
            let flags = if base == 0 { 6 } else { 8 };
            bytes[base + flags] |= 8;
            let time = if base == 0 { 10 } else { 12 };
            bytes[base + time..base + time + 2].copy_from_slice(&0xb500u16.to_le_bytes());
        }
        if mode.is_none() || bytes[base + checksum..base + checksum + 4] != [0; 4] {
            bytes[base + checksum..base + checksum + 4].copy_from_slice(&crc.to_le_bytes());
        }
        bytes[base + size..base + size + 4].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    }
    if mode.is_none() {
        let mut descriptor = b"PK\x07\x08".to_vec();
        descriptor.extend_from_slice(&crc.to_le_bytes());
        descriptor.extend_from_slice(&packed.to_le_bytes());
        descriptor.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        bytes.splice(central..central, descriptor);
        let end = bytes.len() - 22;
        bytes[end + 16..end + 20].copy_from_slice(&((central + 16) as u32).to_le_bytes());
    }
    bytes
}

#[test]
fn encrypted_brotli_authenticates_aes_and_zipcrypto_and_never_publishes_bad_data() {
    let cancel = AtomicBool::new(false);
    for mode in [
        None,
        Some(AesMode::Aes128),
        Some(AesMode::Aes192),
        Some(AesMode::Aes256),
    ] {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("fixture.zip");
        let bytes = encrypted_brotli(mode);
        fs::write(&archive, &bytes).unwrap();
        let out = root.path().join("output");
        fs::create_dir(&out).unwrap();
        assert!(archive_view::test(&archive, None, &cancel).is_err());
        assert!(archive_view::extract_all(&archive, &out, Some("wrong"), &cancel).is_err());
        assert!(!out.join("data.txt").exists());
        archive_view::test(&archive, Some("secret"), &cancel).unwrap();
        archive_view::extract_all(&archive, &out, Some("secret"), &cancel).unwrap();
        assert_eq!(
            fs::read(out.join("data.txt")).unwrap(),
            b"Arco Brotli fixture\n"
        );
        fs::remove_file(out.join("data.txt")).unwrap();
        let mut damaged = bytes;
        let central = damaged.windows(4).position(|b| b == b"PK\x01\x02").unwrap();
        // AES trailer or ZipCrypto payload, immediately before the directory.
        damaged[central - if mode.is_some() { 1 } else { 33 }] ^= 0x40;
        fs::write(&archive, damaged).unwrap();
        assert!(archive_view::extract_all(&archive, &out, Some("secret"), &cancel).is_err());
        assert!(!out.join("data.txt").exists());
    }
}

#[test]
fn seven_zip_options_can_keep_filenames_visible_and_store_without_compression() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("sample.txt");
    fs::write(&file, "payload").unwrap();
    let archive = root.path().join("fixture.7z");
    let settings = Settings {
        encrypt_headers: false,
        seven_zip_level: 0,
        ..Settings::default()
    };
    let cancel = AtomicBool::new(false);
    archive_view::pack_7z_with_options(
        &[file],
        root.path(),
        &archive,
        Some("secret"),
        settings,
        &cancel,
    )
    .unwrap();
    let catalog = archive_view::Catalog::read(&archive, &cancel).unwrap();
    assert!(catalog.contains_encrypted_entries());
    assert!(!catalog.header_encrypted());
    assert_eq!(catalog.rows(Path::new("")).unwrap().len(), 1);
    archive_view::test(&archive, Some("secret"), &cancel).unwrap();
}

#[test]
fn seven_zip_methods_solid_blocks_and_hidden_headers_roundtrip() {
    use crate::archive_settings::SevenZipMethod;
    let root = tempfile::tempdir().unwrap();
    let first = root.path().join("first.txt");
    let second = root.path().join("second.txt");
    fs::write(&first, "first repeated file content\n".repeat(80)).unwrap();
    fs::write(&second, "second repeated file content\n".repeat(80)).unwrap();
    let cancel = AtomicBool::new(false);
    for method in [
        SevenZipMethod::Lzma2,
        SevenZipMethod::Lzma,
        SevenZipMethod::Ppmd,
        SevenZipMethod::Bzip2,
    ] {
        for solid in [false, true] {
            let archive = root.path().join(format!("{method:?}-{solid}.7z"));
            let options = Settings {
                seven_zip_level: 1,
                seven_zip_method: method,
                solid,
                encrypt_headers: true,
                ..Settings::default()
            };
            archive_view::pack_7z_with_options(
                &[first.clone(), second.clone()],
                root.path(),
                &archive,
                Some("secret"),
                options,
                &cancel,
            )
            .unwrap();
            assert!(archive_view::Catalog::read(&archive, &cancel).is_err());
            let catalog =
                archive_view::Catalog::read_with_password(&archive, Some("secret"), &cancel)
                    .unwrap();
            assert!(catalog.header_encrypted());
            let out = tempfile::tempdir().unwrap();
            assert_eq!(
                archive_view::extract_all(&archive, out.path(), Some("secret"), &cancel).unwrap(),
                2
            );
            assert_eq!(
                fs::read(out.path().join("first.txt")).unwrap(),
                fs::read(&first).unwrap()
            );
            assert_eq!(
                fs::read(out.path().join("second.txt")).unwrap(),
                fs::read(&second).unwrap()
            );
            let reader = sevenz_rust2::ArchiveReader::new_with_memory_limit(
                std::fs::File::open(&archive).unwrap(),
                sevenz_rust2::Password::new("secret"),
                128 * 1024,
            )
            .unwrap();
            assert_eq!(reader.archive().blocks.len(), if solid { 1 } else { 2 });
        }
    }
}

#[test]
#[ignore = "requires the bundled 7-Zip adapter"]
fn external_profiles_publish_complete_volume_sets_and_keep_collisions_untouched() {
    use crate::packer_profile::PackerProfile;
    let root = tempfile::tempdir().unwrap();
    let input = root.path().join("input");
    fs::create_dir(&input).unwrap();
    let source = input.join("sample.txt");
    fs::write(&source, "multipart external packer payload\n".repeat(200)).unwrap();
    let exe = std::env::var_os("ARCO_7ZIP_PATH").expect("ARCO_7ZIP_PATH");
    let packer = PackerProfile {
        executable: exe.into(),
        extension: "zip".into(),
        arguments: [
            "a",
            "-tzip",
            "-mx=0",
            "-v256b",
            "-spd",
            "-y",
            "{archive}",
            "{sources}",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
    };
    let target = root.path().join("external.zip");
    let cancel = AtomicBool::new(false);
    let outputs = packer
        .pack_set(
            std::slice::from_ref(&source),
            &input,
            &target,
            Some("secret"),
            &cancel,
        )
        .unwrap();
    assert!(outputs.len() > 2);
    assert_eq!(
        outputs[0],
        fs::canonicalize(root.path().join("external.zip.001")).unwrap()
    );
    assert!(outputs.iter().all(|path| path.is_file()));
    let out = tempfile::tempdir().unwrap();
    archive_view::extract_all(&outputs[0], out.path(), Some("secret"), &cancel).unwrap();
    assert_eq!(
        fs::read(out.path().join("sample.txt")).unwrap(),
        fs::read(&source).unwrap()
    );
    fs::write(root.path().join("collision.zip.002"), b"untouched").unwrap();
    assert!(
        packer
            .pack_set(
                &[source],
                &input,
                &root.path().join("collision.zip"),
                Some("secret"),
                &cancel
            )
            .is_err()
    );
    assert_eq!(
        fs::read(root.path().join("collision.zip.002")).unwrap(),
        b"untouched"
    );
    assert!(!root.path().join("collision.zip.001").exists());
}

#[test]
#[ignore = "requires the LHA32 tool linked by Ghisler via ARCO_LHA_PATH"]
fn official_lha_profile_packs_rewrites_and_removes_the_last_entry() {
    use crate::packer_profile::PackerProfile;
    let root = tempfile::tempdir().unwrap();
    let input = root.path().join("input");
    fs::create_dir(&input).unwrap();
    let source = input.join("folder");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("sample.txt"), b"LHA reference payload").unwrap();
    let exe = std::env::var_os("ARCO_LHA_PATH").expect("ARCO_LHA_PATH");
    let packer = PackerProfile {
        executable: exe.into(),
        extension: "lzh".into(),
        arguments: ["a", "-r", "-x", "{archive}", "{sources}"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
    };
    let archive = root.path().join("fixture.lzh");
    let cancel = AtomicBool::new(false);
    packer
        .pack(std::slice::from_ref(&source), &input, &archive, &cancel)
        .unwrap();
    archive_view::edit_with_packer(
        &archive,
        archives::ZipEdit::Rename("folder/sample.txt".into(), "renamed.txt".into()),
        None,
        Some(&packer),
        &cancel,
    )
    .unwrap();
    let output = tempfile::tempdir().unwrap();
    archive_view::extract_all(&archive, output.path(), None, &cancel).unwrap();
    assert_eq!(
        fs::read(output.path().join("folder/renamed.txt")).unwrap(),
        b"LHA reference payload"
    );
    archive_view::edit_with_packer(
        &archive,
        archives::ZipEdit::Remove(vec!["folder".into()]),
        None,
        Some(&packer),
        &cancel,
    )
    .unwrap();
    assert_eq!(archive_view::test(&archive, None, &cancel).unwrap(), 0);
}

#[test]
#[ignore = "requires the UC2 adapter"]
fn uc2_sas_edits_preserve_entry_point_and_retire_surplus_parts() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("sample.txt");
    fs::write(&source, "UC2 payload\n".repeat(200)).unwrap();
    let cancel = AtomicBool::new(false);
    let stem = root.path().join("fixture.uc2");
    assert!(
        crate::legacy_archives::pack_uc2_sas(
            std::slice::from_ref(&source),
            root.path(),
            &stem,
            80,
            &cancel
        )
        .unwrap()
            > 1
    );
    let first = root.path().join("fixture.uc2.p01");
    assert!(archive_view::is_editable(&first));
    archive_view::edit(
        &first,
        archives::ZipEdit::Rename("sample.txt".into(), "renamed.txt".into()),
        None,
        &cancel,
    )
    .unwrap();
    let output = tempfile::tempdir().unwrap();
    archive_view::extract_all(&first, output.path(), None, &cancel).unwrap();
    assert_eq!(
        fs::read(output.path().join("renamed.txt")).unwrap(),
        fs::read(source).unwrap()
    );
    archive_view::edit(
        &first,
        archives::ZipEdit::Remove(vec!["renamed.txt".into()]),
        None,
        &cancel,
    )
    .unwrap();
    assert_eq!(archive_view::test(&first, None, &cancel).unwrap(), 0);
}
#[test]
#[ignore = "requires the pinned upstream corpus prepared by scripts/prepare-archive-corpus.py"]
fn upstream_read_corpus_matches_independent_readers() {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let root = std::path::PathBuf::from(
        std::env::var_os("ARCO_ARCHIVE_CORPUS").expect("ARCO_ARCHIVE_CORPUS"),
    );
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("manifest.json")).unwrap()).unwrap();
    assert!(
        report["unavailable"].as_array().unwrap().is_empty(),
        "unresolved oracle cases: {}",
        report["unavailable"]
    );
    let cancel = AtomicBool::new(false);
    for source in report["origins"].as_array().unwrap() {
        let digest = Sha256::digest(fs::read(root.join(source["file"].as_str().unwrap())).unwrap());
        assert_eq!(format!("{digest:x}"), source["sha256"].as_str().unwrap());
    }
    for case in report["cases"].as_array().unwrap() {
        let archive = root.join(case["file"].as_str().unwrap());
        let password = case["password"].as_str();
        if case["reject_links"] == true {
            assert!(
                archive_view::Catalog::read_with_password(&archive, password, &cancel).is_err()
            );
            let output = tempfile::tempdir().unwrap();
            assert!(archive_view::extract_all(&archive, output.path(), password, &cancel).is_err());
            assert_eq!(fs::read_dir(output.path()).unwrap().count(), 0);
            continue;
        }
        let catalog = archive_view::Catalog::read_with_password(&archive, password, &cancel)
            .unwrap_or_else(|error| panic!("{}: {error}", archive.display()));
        archive_view::test(&archive, password, &cancel)
            .unwrap_or_else(|error| panic!("{}: {error}", archive.display()));
        let output = tempfile::tempdir().unwrap();
        archive_view::extract_all(&archive, output.path(), password, &cancel)
            .unwrap_or_else(|error| panic!("{}: {error}", archive.display()));
        for member in case["members"].as_array().unwrap() {
            let relative = std::path::PathBuf::from(member["name"].as_str().unwrap());
            let path = output.path().join(&relative);
            if member["directory"] == true {
                assert!(path.is_dir(), "{}: {}", archive.display(), path.display());
                continue;
            }
            assert_eq!(
                fs::metadata(&path).unwrap().len(),
                member["size"].as_u64().unwrap()
            );
            let mut digest = Sha256::new();
            let mut file = fs::File::open(&path).unwrap();
            let mut bytes = [0; 65536];
            loop {
                let size = file.read(&mut bytes).unwrap();
                if size == 0 {
                    break;
                }
                digest.update(&bytes[..size]);
            }
            assert_eq!(
                format!("{:x}", digest.finalize()),
                member["sha256"].as_str().unwrap(),
                "{}: {}",
                archive.display(),
                path.display()
            );
        }
        catalog.check_unchanged().unwrap();
    }
    for name in ["ace/cp437.ace", "ace/blocked.ace"] {
        let archive = root.join(name);
        let catalog = archive_view::Catalog::read(&archive, &cancel).unwrap();
        assert!(!catalog.rows(std::path::Path::new("")).unwrap().is_empty());
        assert!(archive_view::test(&archive, None, &cancel).unwrap() > 0);
        let output = tempfile::tempdir().unwrap();
        assert!(archive_view::extract_all(&archive, output.path(), None, &cancel).unwrap() > 0);
    }
}

#[test]
#[ignore = "requires the bundled 7-Zip console adapter"]
fn advanced_seven_options_preserve_methods_passwords_and_solid_settings() {
    use crate::archive_settings::SevenZipMethod;
    let root = tempfile::tempdir().unwrap();
    let first = root.path().join("first.txt");
    let second = root.path().join("second.txt");
    fs::write(&first, "reference payload one\n".repeat(60)).unwrap();
    fs::write(&second, "reference payload two\n".repeat(60)).unwrap();
    let cancel = AtomicBool::new(false);
    for method in [
        SevenZipMethod::Lzma2,
        SevenZipMethod::Lzma,
        SevenZipMethod::Ppmd,
        SevenZipMethod::Bzip2,
    ] {
        let settings = Settings {
            seven_zip_method: method,
            seven_zip_level: 1,
            threads: 2,
            dictionary_kib: if method == SevenZipMethod::Bzip2 {
                100
            } else {
                1024
            },
            word_size: if method == SevenZipMethod::Bzip2 {
                0
            } else if method == SevenZipMethod::Ppmd {
                8
            } else {
                32
            },
            solid: true,
            solid_block_mib: 1,
            ..Default::default()
        };
        let archive = root.path().join(format!("{method:?}.7z"));
        archive_view::pack_7z_with_options(
            &[first.clone(), second.clone()],
            root.path(),
            &archive,
            Some("secret"),
            settings,
            &cancel,
        )
        .unwrap();
        let catalog =
            archive_view::Catalog::read_with_password(&archive, Some("secret"), &cancel).unwrap();
        assert!(catalog.header_encrypted());
        let output = tempfile::tempdir().unwrap();
        archive_view::extract_all(&archive, output.path(), Some("secret"), &cancel).unwrap();
        assert_eq!(
            fs::read(output.path().join("first.txt")).unwrap(),
            fs::read(&first).unwrap()
        );
        assert_eq!(
            fs::read(output.path().join("second.txt")).unwrap(),
            fs::read(&second).unwrap()
        );
    }
}
