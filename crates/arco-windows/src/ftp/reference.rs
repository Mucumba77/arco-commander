//! Opt-in reference tests against disposable loopback FTP servers.
use super::*;
use std::fs;

#[test]
#[ignore = "requires scripts/test-ftp-area5-reference.py"]
fn area5_plain_profiles_and_transfer_modes() {
    let address = std::env::var("ARCO_TEST_FTP_AREA5").unwrap();
    let work = tempfile::tempdir().unwrap();
    let source = work.path().join("source.txt");
    let output = work.path().join("out.txt");
    let cancel = AtomicBool::new(false);
    let payload = b"First line\nSecond line\n";
    fs::write(&source, payload).unwrap();
    let connect = |options| {
        Connection::new(&address, "arco", Security::Plain)
            .unwrap()
            .with_options(options)
            .unwrap()
            .authenticate("test-only")
            .unwrap()
    };
    let plain = connect(ConnectionOptions {
        charset: "windows-1252".into(),
        family: AddressFamily::Ipv4,
        ..Default::default()
    });
    let root = plain.initial_directory();
    let (folder, name) = plain
        .download_location(&root, &format!("{root}caf%C3%A9.txt"))
        .unwrap();
    assert_eq!(name, "café.txt");
    plain
        .upload_with_mode(&folder, &source, &name, false, TransferMode::Text, &cancel)
        .unwrap();
    assert!(
        plain
            .list(&root, &cancel)
            .unwrap()
            .entries
            .iter()
            .any(|entry| entry.name == name)
    );
    assert!(
        plain
            .server_command(&root, "FEAT", &cancel)
            .unwrap()
            .contains("MLSD")
    );
    assert!(
        plain
            .server_command(&root, "PWD", &cancel)
            .unwrap()
            .contains("257")
    );
    assert!(
        plain
            .server_command(&root, "NOOP\r\nNOOP", &cancel)
            .is_err()
    );
    assert!(
        plain
            .download_location(&root, "ftp://elsewhere.invalid/file.txt")
            .is_err()
    );
    assert!(
        plain
            .download_location(&root, "folder/../file.txt")
            .is_err()
    );
    plain
        .download_text(&root, &name, &output, &cancel, |_| {})
        .unwrap();
    assert_eq!(
        fs::read_to_string(output).unwrap().replace("\r\n", "\n"),
        String::from_utf8_lossy(payload)
    );

    let compressed = connect(ConnectionOptions {
        mode_z: true,
        charset: "windows-1252".into(),
        ..Default::default()
    });
    compressed
        .upload(&root, &source, "compressed.txt", false, &cancel)
        .unwrap();
    let expanded = work.path().join("expanded");
    compressed
        .download_bounded(
            &root,
            "compressed.txt",
            &expanded,
            payload.len() as u64,
            &cancel,
        )
        .unwrap();
    assert_eq!(fs::read(&expanded).unwrap(), payload);
    let rejected = work.path().join("limit");
    assert!(
        compressed
            .download_bounded(&root, "compressed.txt", &rejected, 3, &cancel)
            .is_err()
    );
    assert!(!rejected.exists());

    let text_source = work.path().join("compressed-text-input");
    let text_output = work.path().join("compressed-text-output");
    let mut text_payload = vec![b'x'; 64 * 1024 - 1];
    text_payload.extend_from_slice(b"\r\nSecond line\n");
    fs::write(&text_source, &text_payload).unwrap();
    compressed
        .upload_with_mode(
            &root,
            &text_source,
            "compressed-text.txt",
            false,
            TransferMode::Text,
            &cancel,
        )
        .unwrap();
    compressed
        .download_text(&root, "compressed-text.txt", &text_output, &cancel, |_| {})
        .unwrap();
    text_payload.insert(text_payload.len() - 1, b'\r');
    assert_eq!(fs::read(text_output).unwrap(), text_payload);

    let seed = work.path().join("partial");
    fs::write(&seed, &payload[..4]).unwrap();
    plain
        .upload(&root, &seed, "resume.txt", false, &cancel)
        .unwrap();
    plain
        .upload_resuming(&root, &source, "resume.txt", &cancel)
        .unwrap();
    let resumed = work.path().join("resumed");
    plain
        .download(&root, "resume.txt", &resumed, &cancel)
        .unwrap();
    assert_eq!(fs::read(resumed).unwrap(), payload);
    fs::write(&seed, b"not the prefix").unwrap();
    plain
        .upload(&root, &seed, "unrelated.txt", false, &cancel)
        .unwrap();
    assert!(
        plain
            .upload_resuming(&root, &source, "unrelated.txt", &cancel)
            .is_err()
    );
    assert_eq!(
        plain
            .list_sync(&root, &cancel)
            .unwrap()
            .entries
            .into_iter()
            .find(|entry| entry.name == "unrelated.txt")
            .unwrap()
            .size,
        Some(14)
    );

    let custom = connect(ConnectionOptions {
        list_pattern: r"(?P<kind>[fd])\|(?P<size>[0-9]*)\|(?P<name>.+)".into(),
        charset: "windows-1252".into(),
        show_hidden: true,
        ..Default::default()
    });
    let listing = custom.list(&root, &cancel).unwrap();
    assert!(listing.entries.iter().any(|entry| entry.name == ".hidden"));
    assert!(
        custom
            .raw_listing(&root, &cancel)
            .unwrap()
            .contains("café.txt")
    );
}

#[test]
#[ignore = "requires scripts/test-ftp-area5-reference.py"]
fn area5_tls_explicit_implicit_and_untrusted_certificate() {
    let cert = PathBuf::from(std::env::var_os("ARCO_TEST_FTPS_CA").unwrap());
    for (variable, security) in [
        ("ARCO_TEST_FTPS_EXPLICIT", Security::ExplicitTls),
        ("ARCO_TEST_FTPS_IMPLICIT", Security::ImplicitTls),
    ] {
        let address = std::env::var(variable).unwrap();
        let cancel = AtomicBool::new(false);
        let untrusted = Connection::new(&address, "arco", security)
            .unwrap()
            .authenticate("test-only")
            .unwrap();
        assert_eq!(
            untrusted
                .list(&untrusted.initial_directory(), &cancel)
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        let wrong_host =
            Connection::new(&address.replace("localhost", "127.0.0.1"), "arco", security)
                .unwrap()
                .with_options(ConnectionOptions {
                    ca_bundle: Some(cert.clone()),
                    ..Default::default()
                })
                .unwrap()
                .authenticate("test-only")
                .unwrap();
        assert_eq!(
            wrong_host
                .list(&wrong_host.initial_directory(), &cancel)
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        let trusted = Connection::new(&address, "arco", security)
            .unwrap()
            .with_options(ConnectionOptions {
                ca_bundle: Some(cert.clone()),
                ..Default::default()
            })
            .unwrap()
            .authenticate("test-only")
            .unwrap();
        let location = trusted.initial_directory();
        assert!(
            trusted
                .list(&location, &cancel)
                .unwrap()
                .entries
                .iter()
                .any(|entry| entry.name == "fixture.txt")
        );
        let work = tempfile::tempdir().unwrap();
        let output = work.path().join("from-tls");
        trusted
            .download(&location, "fixture.txt", &output, &cancel)
            .unwrap();
        assert_eq!(fs::read(output).unwrap(), b"reference\n");
    }
}

#[test]
#[ignore = "requires scripts/test-ftp-area5-reference.py"]
fn area5_otp_gateway_and_ipv6() {
    let cancel = AtomicBool::new(false);
    let address = std::env::var("ARCO_TEST_FTP_OTP").unwrap();
    let otp = Connection::new(&address, "arco", Security::Plain)
        .unwrap()
        .with_options(ConnectionOptions {
            otp: true,
            ..Default::default()
        })
        .unwrap()
        .authenticate("This is a test.")
        .unwrap();
    let root = otp.initial_directory();
    assert!(
        otp.list_sync(&root, &cancel)
            .unwrap()
            .entries
            .iter()
            .any(|entry| entry.name == "fixture.txt")
    );
    let work = tempfile::tempdir().unwrap();
    let target = work.path().join("otp.txt");
    otp.download(&root, "fixture.txt", &target, &cancel)
        .unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"reference\n");
    assert!(
        otp.server_command(&root, "FEAT", &cancel)
            .unwrap()
            .contains("MLSD")
    );
    otp.upload(&root, &target, "copy.txt", false, &cancel)
        .unwrap();
    assert!(
        otp.list(&root, &cancel)
            .unwrap()
            .entries
            .iter()
            .any(|entry| entry.name == "copy.txt")
    );

    let gateway = std::env::var("ARCO_TEST_FTP_GATEWAY").unwrap();
    let through_gateway =
        Connection::new("ftp://files.example.invalid:2121/", "arco", Security::Plain)
            .unwrap()
            .with_proxy(&gateway)
            .unwrap()
            .authenticate("test-only")
            .unwrap();
    assert_eq!(
        through_gateway
            .list(&through_gateway.initial_directory(), &cancel)
            .unwrap()
            .entries
            .len(),
        1
    );

    if let Ok(address) = std::env::var("ARCO_TEST_FTP_IPV6") {
        let ipv6 = Connection::new(&address, "arco", Security::Plain)
            .unwrap()
            .with_options(ConnectionOptions {
                family: AddressFamily::Ipv6,
                ..Default::default()
            })
            .unwrap()
            .authenticate("test-only")
            .unwrap();
        assert_eq!(
            ipv6.list(&ipv6.initial_directory(), &cancel)
                .unwrap()
                .entries
                .len(),
            1
        );
    }
}

#[test]
#[ignore = "requires scripts/test-ftp-area5-reference.py"]
fn area5_legacy_server_checksums() {
    let cancel = AtomicBool::new(false);
    for (command, algorithm, digest) in [
        ("XSHA1", "SHA-1", "bcb3d6650e281ef57458d83722d07be115132a02"),
        ("XMD5", "MD5", "b4b8061b9d0ac9812995a11cc2941a51"),
        ("MD5", "MD5", "b4b8061b9d0ac9812995a11cc2941a51"),
        ("XCRC", "CRC32", "b6c7e404"),
    ] {
        let address = std::env::var(format!("ARCO_TEST_FTP_{command}")).unwrap();
        let session = Connection::new(&address, "arco", Security::Plain)
            .unwrap()
            .authenticate("test-only")
            .unwrap();
        let root = session.initial_directory();
        let entry = session
            .list_sync(&root, &cancel)
            .unwrap()
            .entries
            .into_iter()
            .find(|entry| entry.name == "fixture.txt")
            .unwrap();
        let method = session
            .sync_checksum_method(&root, &cancel)
            .unwrap()
            .unwrap();
        assert_eq!(
            session
                .sync_checksum(&root, &entry, method, &cancel)
                .unwrap(),
            Some((algorithm, digest.into())),
            "{command}"
        );
    }
}
