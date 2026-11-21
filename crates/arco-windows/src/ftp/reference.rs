//! Opt-in reference tests against disposable loopback FTP servers.
use super::*;
use std::fs;

#[test]
#[ignore = "requires scripts/test-ftp-area5-reference.py"]
fn area5_fxp_connects_the_servers_directly_over_ipv4_and_ipv6() {
    let cancel = AtomicBool::new(false);
    for (version, active_command) in [(4, "PORT"), (6, "EPRT")] {
        let Ok(source_address) = std::env::var(format!("ARCO_TEST_FXP_V{version}_SOURCE")) else {
            assert_eq!(version, 6);
            continue;
        };
        let target_address = std::env::var(format!("ARCO_TEST_FXP_V{version}_TARGET")).unwrap();
        let open = |address: &str, family| {
            Connection::new(address, "arco", Security::Plain)
                .unwrap()
                .with_options(ConnectionOptions {
                    family,
                    ..Default::default()
                })
                .unwrap()
                .authenticate("test-only")
                .unwrap()
        };
        let source = open(&source_address, AddressFamily::Automatic);
        let target = open(&target_address, AddressFamily::Automatic);
        let left = source.initial_directory();
        let right = target.initial_directory();
        assert_eq!(
            source
                .fxp_to(
                    (&left, "fxp-input.bin"),
                    &target,
                    (&right, "direct.bin"),
                    &cancel
                )
                .unwrap(),
            131_079
        );
        assert_eq!(
            source
                .fxp_to(
                    (&left, "fxp-input.bin"),
                    &target,
                    (&right, "occupied.bin"),
                    &cancel
                )
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        let error = source
            .fxp_to(
                (&left, "fxp-denied.bin"),
                &target,
                (&right, "denied.bin"),
                &cancel,
            )
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains(&format!("Status 522 bei {active_command}")),
            "{error}"
        );
        for (source_name, target_name, cause) in [
            (
                "fxp-short.bin",
                "incomplete.bin",
                "FTP-Größenprüfung fehlgeschlagen",
            ),
            (
                "fxp-collision.bin",
                "raced.bin",
                "FXP-Zielname existiert bereits",
            ),
        ] {
            let error = source
                .fxp_to(
                    (&left, source_name),
                    &target,
                    (&right, target_name),
                    &cancel,
                )
                .unwrap_err()
                .to_string();
            assert!(
                error.contains(cause) && error.contains(".arco-fxp-"),
                "{error}"
            );
        }
        if version == 6 {
            let explicit = open(&source_address, AddressFamily::Ipv6);
            assert_eq!(
                explicit
                    .fxp_to(
                        (&left, "fxp-input.bin"),
                        &target,
                        (&right, "explicit.bin"),
                        &cancel
                    )
                    .unwrap(),
                131_079
            );
            let incompatible = open(&target_address, AddressFamily::Ipv4);
            let error = explicit
                .fxp_to(
                    (&left, "fxp-input.bin"),
                    &incompatible,
                    (&right, "incompatible.bin"),
                    &cancel,
                )
                .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        }
    }
}

#[test]
#[ignore = "requires scripts/test-ftp-area5-reference.py"]
fn area5_ftp_download_updates_the_limit_while_curl_is_running() {
    let cert = PathBuf::from(std::env::var_os("ARCO_TEST_FTPS_CA").unwrap());
    for (variable, security) in [
        ("ARCO_TEST_FTP_AREA5", Security::Plain),
        ("ARCO_TEST_FTPS_EXPLICIT", Security::ExplicitTls),
        ("ARCO_TEST_FTPS_IMPLICIT", Security::ImplicitTls),
    ] {
        let address = std::env::var(variable).unwrap();
        let session = Connection::new(&address, "arco", security)
            .unwrap()
            .with_options(ConnectionOptions {
                ca_bundle: (security != Security::Plain).then(|| cert.clone()),
                ..Default::default()
            })
            .unwrap()
            .authenticate("test-only")
            .unwrap();
        super::rate::tests::check_live_download(&session, &session.initial_directory());
    }
}

#[test]
#[ignore = "requires scripts/test-ftp-area5-reference.py"]
fn area5_ftp_resume_recovers_after_connection_loss() {
    let cert = PathBuf::from(std::env::var_os("ARCO_TEST_FTPS_CA").unwrap());
    let local = tempfile::tempdir().unwrap();
    let source = local.path().join("source.bin");
    let data: Vec<_> = (0..270_000).map(|index| (index % 251) as u8).collect();
    fs::write(&source, &data).unwrap();
    let cancel = AtomicBool::new(false);
    for (mode, security) in [
        ("PLAIN", Security::Plain),
        ("EXPLICIT", Security::ExplicitTls),
        ("IMPLICIT", Security::ImplicitTls),
    ] {
        let address = std::env::var(format!("ARCO_TEST_FTP_RESUME_{mode}")).unwrap();
        let session = Connection::new(&address, "arco", security)
            .unwrap()
            .with_options(ConnectionOptions {
                ca_bundle: (security != Security::Plain).then(|| cert.clone()),
                ..Default::default()
            })
            .unwrap()
            .authenticate("test-only")
            .unwrap();
        let root = session.initial_directory();
        for (name, expected_reconnects) in [
            ("resume-download.bin", 1),
            ("resume-changed.bin", 1),
            ("resume-exhausted.bin", 2),
        ] {
            let destination = local.path().join(format!("{mode}-{name}"));
            let mut reconnects = 0;
            let mut resumed_at = None;
            let result = session.download_resuming_with_progress(
                &root,
                name,
                &destination,
                &cancel,
                |progress| match progress.phase {
                    DownloadPhase::Reconnecting => reconnects += 1,
                    DownloadPhase::Receiving if reconnects > 0 => {
                        resumed_at.get_or_insert(progress.received);
                    }
                    _ => {}
                },
            );
            assert_eq!(reconnects, expected_reconnects, "{mode}/{name}: {result:?}");
            match name {
                "resume-download.bin" => {
                    result.unwrap();
                    assert_eq!(resumed_at, Some(98_304));
                    assert_eq!(fs::read(destination).unwrap(), data);
                }
                "resume-changed.bin" => {
                    assert!(
                        result
                            .unwrap_err()
                            .to_string()
                            .contains("überlappenden Bereich")
                    );
                    assert!(!destination.exists());
                }
                _ => {
                    assert!(result.is_err());
                    assert!(!destination.exists());
                    // The same checkpoint remains usable in a later job.
                    session
                        .download_resuming(&root, name, &destination, &cancel)
                        .unwrap();
                    assert_eq!(fs::read(destination).unwrap(), data);
                }
            }
        }
        for name in ["resume-upload.bin", "resume-ack.bin"] {
            let mut reconnects = 0;
            session
                .upload_resuming_with_progress(&root, &source, name, &cancel, |progress| {
                    if progress.phase == DownloadPhase::Reconnecting {
                        reconnects += 1;
                    }
                })
                .unwrap_or_else(|error| panic!("{mode}/{name}: {error}"));
            assert_eq!(reconnects, 1, "{mode}/{name}");
        }
        let truncated = session
            .upload_resuming(&root, &source, "resume-truncated.bin", &cancel)
            .unwrap_err();
        assert!(
            truncated.to_string().contains("gekürzt oder ersetzt"),
            "{mode}: {truncated}"
        );
    }
}

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
    for (variable, mutual_variable, security) in [
        (
            "ARCO_TEST_FTPS_EXPLICIT",
            "ARCO_TEST_FTPS_MUTUAL_EXPLICIT",
            Security::ExplicitTls,
        ),
        (
            "ARCO_TEST_FTPS_IMPLICIT",
            "ARCO_TEST_FTPS_MUTUAL_IMPLICIT",
            Security::ImplicitTls,
        ),
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
        if let Ok(thumbprint) = std::env::var("ARCO_TEST_FTPS_CLIENT_CERT") {
            let mutual = std::env::var(mutual_variable).unwrap();
            let connect = |client_certificate| {
                Connection::new(&mutual, "arco", security)
                    .unwrap()
                    .with_options(ConnectionOptions {
                        ca_bundle: Some(cert.clone()),
                        client_certificate,
                        ..Default::default()
                    })
                    .unwrap()
                    .authenticate("test-only")
                    .unwrap()
            };
            let no_certificate = connect(None);
            assert!(
                no_certificate
                    .list(&no_certificate.initial_directory(), &cancel)
                    .is_err()
            );
            let mutual = connect(Some(thumbprint));
            let output = work.path().join("from-mutual-tls");
            mutual
                .download(&mutual.initial_directory(), "fixture.txt", &output, &cancel)
                .unwrap();
            assert_eq!(fs::read(output).unwrap(), b"reference\n");
            println!("{security:?}: client certificate required for FTPS and protected data");
        }
    }
}

#[test]
#[ignore = "requires scripts/test-ftp-area5-reference.py"]
fn area5_skey_words_on_fresh_control_connections() {
    let cancel = AtomicBool::new(false);
    let address = std::env::var("ARCO_TEST_FTP_SKEY").unwrap();
    let session = Connection::new(&address, "arco", Security::Plain)
        .unwrap()
        .with_options(ConnectionOptions {
            otp: true,
            ..Default::default()
        })
        .unwrap()
        .authenticate("This is a test.")
        .unwrap();
    let root = session.initial_directory();
    assert!(
        session
            .list_sync(&root, &cancel)
            .unwrap()
            .entries
            .iter()
            .any(|entry| entry.name == "fixture.txt")
    );
    assert!(
        session
            .server_command(&root, "FEAT", &cancel)
            .unwrap()
            .contains("MLSD")
    );
    assert!(session.list_sync(&root, &cancel).is_err());
}

#[test]
#[ignore = "requires scripts/test-ftp-area5-reference.py"]
fn area5_socks4_user_id_is_sent_on_control_and_data_connections() {
    let address = std::env::var("ARCO_TEST_FTP_SOCKS4_TARGET").unwrap();
    let port = std::env::var("ARCO_TEST_FTP_SOCKS4_PORT").unwrap();
    let cancel = AtomicBool::new(false);
    let work = tempfile::tempdir().unwrap();
    for (scheme, target) in [
        ("socks4", address.clone()),
        ("socks4a", address.replace("127.0.0.1", "ftp.invalid")),
    ] {
        let proxy = format!("{scheme}://127.0.0.1:{port}");
        let connect = |user| {
            Connection::new(&target, "arco", Security::Plain)
                .unwrap()
                .with_proxy(&proxy)
                .unwrap()
                .with_proxy_user(user)
                .unwrap()
                .authenticate("test-only")
                .unwrap()
        };
        let session = connect("proxy-fixture");
        let root = session.initial_directory();
        let path = work.path().join(scheme);
        session
            .download(&root, "fixture.txt", &path, &cancel)
            .unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"reference\n");
        let name = format!("{scheme}.txt");
        session.upload(&root, &path, &name, false, &cancel).unwrap();
        assert!(
            session
                .list_sync(&root, &cancel)
                .unwrap()
                .regular_file(&name)
                .is_ok()
        );
        let rejected = connect("wrong");
        assert!(rejected.list_sync(&root, &cancel).is_err());
    }
}

#[test]
#[ignore = "requires scripts/test-ftp-area5-reference.py"]
fn area5_http_forward_lists_and_downloads_with_proxy_and_ftp_authentication() {
    let cancel = AtomicBool::new(false);
    let work = tempfile::tempdir().unwrap();
    for scheme in ["HTTP", "HTTPS"] {
        let proxy = std::env::var(format!("ARCO_TEST_{scheme}_FORWARD")).unwrap();
        let connect = |ca_bundle| {
            Connection::new(
                "ftp://files.example.invalid:2121/home/",
                "arco",
                Security::Plain,
            )
            .unwrap()
            .with_proxy(&proxy)
            .unwrap()
            .with_proxy_user("proxy-fixture")
            .unwrap()
            .with_proxy_password("proxy-only")
            .unwrap()
            .with_options(ConnectionOptions {
                http_forward: true,
                ca_bundle,
                ..Default::default()
            })
            .unwrap()
            .authenticate("test-only")
            .unwrap()
        };
        if scheme == "HTTPS" {
            let untrusted = connect(None);
            assert_eq!(
                untrusted
                    .list(&untrusted.initial_directory(), &cancel)
                    .err()
                    .unwrap()
                    .kind(),
                io::ErrorKind::PermissionDenied
            );
        }
        let session = connect(
            (scheme == "HTTPS").then(|| std::env::var("ARCO_TEST_FTPS_CA").unwrap().into()),
        );
        let root = session.initial_directory();
        let listing = session.list(&root, &cancel).unwrap();
        assert_eq!(listing.format, ListFormat::HttpForward);
        assert_eq!(
            listing
                .entries
                .iter()
                .map(|entry| (entry.name.as_str(), entry.directory, entry.size))
                .collect::<Vec<_>>(),
            [
                ("nested", Some(true), None),
                ("report & notes.bin", Some(false), Some(8)),
                ("unknown.txt", Some(false), None),
            ]
        );
        assert!(
            session
                .raw_listing(&root, &cancel)
                .unwrap()
                .contains("<pre>")
        );
        assert!(
            session
                .connection_details(&root, &cancel)
                .unwrap()
                .contains("HTTP-Forward")
        );
        let nested = session.child_directory(&root, "nested").unwrap();
        assert_eq!(
            session.list(&nested, &cancel).unwrap().entries[0].name,
            "data.bin"
        );
        let path = work.path().join(scheme);
        session
            .download(&root, "report & notes.bin", &path, &cancel)
            .unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"arco\x00bin");
        let unknown = tempfile::NamedTempFile::new().unwrap();
        session
            .fetch_sync(&root, &listing.entries[2], unknown.as_file(), &cancel)
            .unwrap();
        assert_eq!(fs::read(unknown.path()).unwrap(), b"unknown\r\nsize\n");
        for (name, kind) in [
            ("missing.bin", io::ErrorKind::NotFound),
            ("redirect.bin", io::ErrorKind::Other),
        ] {
            let path = work.path().join(name);
            let err = session.download(&root, name, &path, &cancel).unwrap_err();
            assert_eq!(err.kind(), kind);
            assert!(!path.exists());
        }
        let bounded = work.path().join("bounded.bin");
        assert!(
            session
                .download_bounded(&root, "report & notes.bin", &bounded, 7, &cancel)
                .is_err()
        );
        assert!(!bounded.exists());
        assert_eq!(
            session
                .upload(&root, &path, "write.bin", false, &cancel)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Unsupported
        );
    }
}

#[test]
#[ignore = "requires scripts/test-ftp-area5-reference.py"]
fn area5_gateway_variants_transfer_through_the_selected_proxy() {
    use arco_core::profile::FtpGateway;
    let cancel = AtomicBool::new(false);
    let work = tempfile::tempdir().unwrap();
    for (name, mode, with_login) in [
        ("SITE", FtpGateway::Site, false),
        ("SITE_LOGIN", FtpGateway::Site, true),
        ("AUTH_USER_HOST", FtpGateway::ProxyThenUserAtHost, true),
        ("OPEN", FtpGateway::Open, false),
        ("OPEN_LOGIN", FtpGateway::Open, true),
        ("AUTH_USER", FtpGateway::ProxyThenUser, true),
        ("PROXY_HOST", FtpGateway::ProxyAtHostThenUser, true),
        ("USER_HOST_PROXY", FtpGateway::UserAtHostAndProxy, true),
        ("USER_PROXY_HOST", FtpGateway::UserAtProxyAtHost, true),
    ] {
        let proxy = std::env::var(format!("ARCO_TEST_FTP_GATEWAY_{name}")).unwrap();
        let mut connection =
            Connection::new("ftp://files.example.invalid:2121/", "arco", Security::Plain)
                .unwrap()
                .with_proxy(&proxy)
                .unwrap()
                .with_ftp_gateway(mode)
                .unwrap();
        if with_login {
            connection = connection
                .with_proxy_user("gateway")
                .unwrap()
                .with_proxy_password("proxy-only")
                .unwrap();
        }
        let session = connection.authenticate("test-only").unwrap();
        let root = session.initial_directory();
        let target = work.path().join(name);
        session
            .download(&root, "fixture.txt", &target, &cancel)
            .unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"reference\n");
        session
            .upload(&root, &target, "copy.txt", false, &cancel)
            .unwrap();
        assert_eq!(
            session
                .list_sync(&root, &cancel)
                .unwrap()
                .regular_file("copy.txt")
                .unwrap()
                .size,
            Some(10)
        );
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
