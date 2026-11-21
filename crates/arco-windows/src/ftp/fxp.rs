use super::sync::{Publication, expect_entry};
use super::{
    AddressFamily, Connection, Entry, Security, Session, Url, check_cancel, check_segment,
};
use percent_encoding::percent_decode_str;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{IpAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

const REPLY_TIMEOUT: Duration = Duration::from_secs(30);
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(1800);

pub struct FxpCopy<'a> {
    pub source: &'a Entry,
    pub target_name: &'a str,
    pub expected_target: Option<&'a Entry>,
    pub modified_utc: Option<&'a str>,
}

impl Session {
    pub fn can_fxp_to(&self, other: &Self) -> bool {
        self.supports_fxp()
            && other.supports_fxp()
            && !same_endpoint(&self.connection.url, &other.connection.url)
            && !matches!(
                (
                    self.connection.options.family,
                    other.connection.options.family
                ),
                (AddressFamily::Ipv4, AddressFamily::Ipv6)
                    | (AddressFamily::Ipv6, AddressFamily::Ipv4)
            )
    }

    fn supports_fxp(&self) -> bool {
        self.connection.security == Security::Plain
            && self.connection.proxy.is_none()
            && !self.uses_native_ftp()
    }

    pub fn fxp_to(
        &self,
        (source_dir, source_file): (&Url, &str),
        other: &Self,
        (target_dir, target_file): (&Url, &str),
        cancel: &AtomicBool,
    ) -> io::Result<u64> {
        check_segment(source_file)?;
        check_segment(target_file)?;
        let mut pair = self.fxp_controls(source_dir, other, target_dir, cancel)?;
        let (listing, directory) = session_at_peer(other, target_dir, pair.target.peer)?;
        let reservation = staging_name()?;
        let staging = staging_leaf(&reservation)?;
        check_target(&listing, &directory, &[target_file, staging], cancel)?;
        let size = pair.source.size(source_file, cancel)?;
        pair.transfer(source_file, staging, size, cancel)?;
        let mut publish = || {
            check_target(&listing, &directory, &[target_file], cancel)?;
            pair.target
                .command(&format!("RNFR {staging}"), &[350], "RNFR", cancel)?;
            pair.target
                .command(&format!("RNTO {target_file}"), &[250], "RNTO", cancel)?;
            Ok(size)
        };
        publish().map_err(|error| stage_error(error, staging))
    }

    pub fn fxp_copy_sync(
        &self,
        source_dir: &Url,
        other: &Self,
        target_dir: &Url,
        copy: FxpCopy<'_>,
        cancel: &AtomicBool,
    ) -> io::Result<Entry> {
        let size = copy
            .source
            .size
            .filter(|_| copy.source.directory == Some(false) && !copy.source.symlink)
            .ok_or_else(|| {
                super::invalid_input("FXP benötigt eine reguläre Quelldatei mit bekannter Größe.")
            })?;
        check_segment(&copy.source.name)?;
        if copy
            .expected_target
            .is_some_and(|entry| entry.directory != Some(false) || entry.symlink)
        {
            return Err(super::invalid_input(
                "FXP kann nur reguläre Zieldateien ersetzen.",
            ));
        }
        let publication = Publication {
            name: copy.target_name,
            expected: copy.expected_target,
            size,
            modified_utc: copy.modified_utc,
        };
        other.validate_publication(publication)?;
        let mut pair = self.fxp_controls(source_dir, other, target_dir, cancel)?;
        let (source, source_dir) = session_at_peer(self, source_dir, pair.source.peer)?;
        let (target, target_dir) = session_at_peer(other, target_dir, pair.target.peer)?;
        source.verify_sync_entry(&source_dir, copy.source, cancel)?;
        let reservation = staging_name()?;
        let staging = staging_leaf(&reservation)?;
        let listing = target.list_sync(&target_dir, cancel)?;
        expect_entry(&listing, publication.name, publication.expected)?;
        expect_entry(&listing, staging, None)?;
        if pair.source.size(&copy.source.name, cancel)? != size {
            return Err(io::Error::other(
                "Die FTP-Quelldatei hat ihre Größe geändert.",
            ));
        }
        pair.transfer(&copy.source.name, staging, size, cancel)?;
        drop(pair);
        source
            .verify_sync_entry(&source_dir, copy.source, cancel)
            .and_then(|()| target.publish_sync_stage(&target_dir, staging, publication, cancel))
            .map_err(|error| stage_error(error, staging))
    }

    fn fxp_controls(
        &self,
        source_dir: &Url,
        other: &Self,
        target_dir: &Url,
        cancel: &AtomicBool,
    ) -> io::Result<FxpControls> {
        if !self.supports_fxp() || !other.supports_fxp() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "FXP erfordert zwei direkt verbundene Klartext-FTP-Server",
            ));
        }
        self.validate(source_dir)?;
        other.validate(target_dir)?;
        if same_endpoint(source_dir, target_dir) {
            return Err(io::Error::other(
                "FXP benötigt zwei verschiedene FTP-Endpunkte",
            ));
        }
        let (source_socket, target_socket) =
            open_pair(self, source_dir, other, target_dir, cancel)?;
        if source_socket.peer_addr()? == target_socket.peer_addr()? {
            return Err(io::Error::other(
                "FXP benötigt zwei verschiedene FTP-Endpunkte",
            ));
        }
        Ok(FxpControls {
            source: Control::login(self, source_dir, source_socket, cancel)?,
            target: Control::login(other, target_dir, target_socket, cancel)?,
        })
    }
}

fn same_endpoint(left: &Url, right: &Url) -> bool {
    left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

fn staging_name() -> io::Result<tempfile::TempDir> {
    tempfile::Builder::new().prefix(".arco-fxp-").tempdir()
}

fn staging_leaf(reservation: &tempfile::TempDir) -> io::Result<&str> {
    reservation
        .path()
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or_else(|| io::Error::other("Ungültiger temporärer FXP-Dateiname."))
}

fn stage_error(error: io::Error, staging: &str) -> io::Error {
    io::Error::new(
        error.kind(),
        format!("{error}. Mögliche Teildatei am Ziel: {staging}"),
    )
}

struct FxpControls {
    source: Control,
    target: Control,
}

impl FxpControls {
    fn transfer(
        &mut self,
        name: &str,
        staging: &str,
        expected: u64,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let Self { source, target } = self;
        match target.peer {
            IpAddr::V4(peer) => {
                let (_, passive) = target.command("PASV", &[227], "PASV", cancel)?;
                let [hi, lo] = parse_passive_port(&passive)?.to_be_bytes();
                let [a, b, c, d] = peer.octets();
                source.command(
                    &format!("PORT {a},{b},{c},{d},{hi},{lo}"),
                    &[200],
                    "PORT",
                    cancel,
                )?;
            }
            IpAddr::V6(peer) => {
                let (_, passive) = target.command("EPSV", &[229], "EPSV", cancel)?;
                let port = parse_extended_passive_port(&passive)?;
                source.command(&format!("EPRT |2|{peer}|{port}|"), &[200], "EPRT", cancel)?;
            }
        }
        target.send(&format!("STOR {staging}"), cancel)?;
        let result = (|| {
            source.send(&format!("RETR {name}"), cancel)?;
            source.expect(&[125, 150], "RETR", REPLY_TIMEOUT, cancel)?;
            target.expect(&[125, 150], "STOR", REPLY_TIMEOUT, cancel)?;
            source.expect(&[226, 250], "Quelltransfer", TRANSFER_TIMEOUT, cancel)?;
            target.expect(&[226, 250], "Zieltransfer", TRANSFER_TIMEOUT, cancel)?;
            if source.size(name, cancel)? != expected || target.size(staging, cancel)? != expected {
                return Err(io::Error::other("FTP-Größenprüfung fehlgeschlagen"));
            }
            Ok(())
        })();
        result.map_err(|error| stage_error(error, staging))
    }
}

fn open_pair(
    source: &Session,
    source_directory: &Url,
    target: &Session,
    target_directory: &Url,
    cancel: &AtomicBool,
) -> io::Result<(TcpStream, TcpStream)> {
    use AddressFamily::{Automatic, Ipv4, Ipv6};

    let open = |family| -> io::Result<_> {
        let left = Control::open_peer(source_directory, family, cancel)?;
        let right = Control::open_peer(target_directory, family, cancel)?;
        Ok((left, right))
    };
    match (
        source.connection.options.family,
        target.connection.options.family,
    ) {
        (Ipv4, Ipv6) | (Ipv6, Ipv4) => Err(super::invalid_input(
            "Beide FTP-Profile benötigen eine gemeinsame IP-Version für FXP.",
        )),
        (Ipv4, _) | (_, Ipv4) => open(Ipv4),
        (Ipv6, _) | (_, Ipv6) => open(Ipv6),
        (Automatic, Automatic) => open(Ipv4).or_else(|error| {
            if error.kind() == io::ErrorKind::Interrupted {
                return Err(error);
            }
            open(Ipv6)
        }),
    }
}

fn session_at_peer(session: &Session, directory: &Url, peer: IpAddr) -> io::Result<(Session, Url)> {
    let pin = |mut address: Url| {
        address
            .set_ip_host(peer)
            .map_err(|()| super::invalid_input("Ungültige FXP-Serveradresse."))?;
        Ok::<_, io::Error>(address)
    };
    let address = pin(session.initial_directory())?;
    let mut connection =
        Connection::new(address.as_str(), &session.connection.user, Security::Plain)?;
    // Snapshot checks and publication must use the same peer as the data transfer.
    connection.options = session.connection.options.clone();
    connection.active = session.connection.active;
    connection.listing_mode = session.connection.listing_mode;
    connection.recycle_remote = session.connection.recycle_remote;
    connection.recycle_overwrites = session.connection.recycle_overwrites;
    Ok((
        connection.authenticate(session.password.as_str())?,
        pin(directory.clone())?,
    ))
}

fn check_target(
    session: &Session,
    directory: &Url,
    names: &[&str],
    cancel: &AtomicBool,
) -> io::Result<()> {
    let names: Vec<_> = names.iter().map(|name| name.to_lowercase()).collect();
    if session
        .list(directory, cancel)?
        .entries
        .iter()
        .any(|entry| names.contains(&entry.name.to_lowercase()))
    {
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "FXP-Zielname existiert bereits",
        ))
    } else {
        Ok(())
    }
}

pub(super) struct Control {
    stream: BufReader<TcpStream>,
    pub peer: IpAddr,
    encoding: &'static encoding_rs::Encoding,
}

impl Control {
    pub fn encoding(&self) -> &'static encoding_rs::Encoding {
        self.encoding
    }
    fn login(
        session: &Session,
        directory: &Url,
        stream: TcpStream,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        let mut control = Self::read_welcome(session, stream, cancel)?;
        let (status, _) = control.command(
            &format!("USER {}", session.connection.user),
            &[230, 331],
            "USER",
            cancel,
        )?;
        if status == 331 {
            let pass = zeroize::Zeroizing::new(format!("PASS {}", session.password.as_str()));
            control.command(pass.as_str(), &[230], "PASS", cancel)?;
        }
        control.command("TYPE I", &[200], "TYPE I", cancel)?;
        control.change_directory(directory.path(), cancel)?;
        Ok(control)
    }

    pub fn connect_peer(
        session: &Session,
        directory: &Url,
        family: AddressFamily,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        Self::read_welcome(session, Self::open_peer(directory, family, cancel)?, cancel)
    }

    fn open_peer(
        directory: &Url,
        family: AddressFamily,
        cancel: &AtomicBool,
    ) -> io::Result<TcpStream> {
        let host = directory
            .host_str()
            .map(|host| host.trim_start_matches('[').trim_end_matches(']'))
            .ok_or_else(|| io::Error::other("fehlender FTP-Host"))?;
        let port = directory.port_or_known_default().unwrap_or(21);
        let mut stream = None;
        for address in (host, port)
            .to_socket_addrs()?
            .filter(|address| match family {
                AddressFamily::Automatic => true,
                AddressFamily::Ipv4 => address.is_ipv4(),
                AddressFamily::Ipv6 => address.is_ipv6(),
            })
            .take(4)
        {
            check_cancel(cancel)?;
            if let Ok(connection) = TcpStream::connect_timeout(&address, Duration::from_secs(10)) {
                stream = Some(connection);
                break;
            }
        }
        stream.ok_or_else(|| io::Error::other("FTP konnte keine Steuerverbindung herstellen"))
    }

    fn read_welcome(session: &Session, stream: TcpStream, cancel: &AtomicBool) -> io::Result<Self> {
        let peer = stream.peer_addr()?.ip();
        stream.set_read_timeout(Some(Duration::from_millis(400)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        let mut control = Self {
            stream: BufReader::new(stream),
            peer,
            encoding: session.connection.options.encoding,
        };
        control.expect(&[220], "Anmeldung", REPLY_TIMEOUT, cancel)?;
        Ok(control)
    }

    pub fn change_directory(&mut self, path: &str, cancel: &AtomicBool) -> io::Result<()> {
        if path.starts_with("//") {
            self.command("CWD /", &[250], "CWD", cancel)?;
        }
        for (index, segment) in path
            .split('/')
            .filter(|segment| !segment.is_empty())
            .enumerate()
        {
            let segment = percent_decode_str(segment)
                .decode_utf8()
                .map_err(io::Error::other)?;
            if !(index == 0 && segment == "/" && !path.starts_with("//")) {
                check_segment(&segment)?;
            }
            self.command(&format!("CWD {segment}"), &[250], "CWD", cancel)?;
        }
        Ok(())
    }

    pub fn send(&mut self, command: &str, cancel: &AtomicBool) -> io::Result<()> {
        check_cancel(cancel)?;
        if command.chars().any(char::is_control) {
            return Err(io::Error::other("ungültiger FTP-Befehl"));
        }
        let (bytes, _, errors) = self.encoding.encode(command);
        if errors {
            return Err(io::Error::other(
                "Der FTP-Befehl ist im Serverzeichensatz nicht darstellbar.",
            ));
        }
        self.stream.get_mut().write_all(&bytes)?;
        self.stream.get_mut().write_all(b"\r\n")
    }

    pub fn command(
        &mut self,
        command: &str,
        codes: &[u16],
        operation: &str,
        cancel: &AtomicBool,
    ) -> io::Result<(u16, String)> {
        self.send(command, cancel)?;
        self.expect(codes, operation, REPLY_TIMEOUT, cancel)
    }

    pub fn expect(
        &mut self,
        codes: &[u16],
        operation: &str,
        timeout: Duration,
        cancel: &AtomicBool,
    ) -> io::Result<(u16, String)> {
        let (status, response) = self.reply(timeout, cancel)?;
        if !codes.contains(&status) {
            let kind = match status {
                421 | 425 | 426 => io::ErrorKind::ConnectionAborted,
                _ => io::ErrorKind::Other,
            };
            return Err(io::Error::new(
                kind,
                format!("FTP-Server meldet Status {status} bei {operation}"),
            ));
        }
        Ok((
            status,
            response
                .lines()
                .last()
                .unwrap_or_default()
                .get(4..)
                .unwrap_or_default()
                .to_owned(),
        ))
    }

    pub fn reply(&mut self, timeout: Duration, cancel: &AtomicBool) -> io::Result<(u16, String)> {
        let deadline = Instant::now() + timeout;
        let line = self.read_line(deadline, cancel)?;
        let status = line
            .get(..3)
            .and_then(|status| status.parse::<u16>().ok())
            .ok_or_else(|| io::Error::other("ungültiger FTP-Status"))?;
        let separator = line.as_bytes().get(3);
        let mut response = line.clone();
        if separator == Some(&b'-') {
            let terminator = format!("{status:03} ");
            for _ in 0..128 {
                let line = self.read_line(deadline, cancel)?;
                response.push('\n');
                response.push_str(&line);
                if line.starts_with(&terminator) {
                    return Ok((status, response));
                }
            }
            return Err(io::Error::other("mehrzeilige FTP-Antwort ist zu lang"));
        }
        if separator != Some(&b' ') {
            return Err(io::Error::other("ungültige FTP-Antwort"));
        }
        Ok((status, response))
    }

    fn read_line(&mut self, deadline: Instant, cancel: &AtomicBool) -> io::Result<String> {
        let mut bytes = Vec::new();
        while bytes.len() < 8192 {
            check_cancel(cancel)?;
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "FTP-Antwortzeit überschritten",
                ));
            }
            match (&mut self.stream)
                .take((8192 - bytes.len()) as u64)
                .read_until(b'\n', &mut bytes)
            {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "FTP-Verbindung beendet",
                    ));
                }
                Ok(_) if bytes.last() == Some(&b'\n') => {
                    return Ok(self
                        .encoding
                        .decode(&bytes)
                        .0
                        .trim_end_matches(['\r', '\n'])
                        .to_owned());
                }
                Ok(_) => (),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) => {}
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::other("FTP-Antwortzeile ist zu lang"))
    }

    fn size(&mut self, name: &str, cancel: &AtomicBool) -> io::Result<u64> {
        let (_, response) = self.command(&format!("SIZE {name}"), &[213], "SIZE", cancel)?;
        response.trim().parse().map_err(io::Error::other)
    }
}

pub(super) fn parse_passive_port(response: &str) -> io::Result<u16> {
    let tuple = response
        .split_once('(')
        .and_then(|(_, after)| after.split_once(')').map(|(inside, _)| inside))
        .ok_or_else(|| io::Error::other("ungültige PASV-Antwort"))?;
    let fields = tuple
        .split(',')
        .map(|field| field.trim().parse::<u8>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(io::Error::other)?;
    let [_, _, _, _, high, low] = fields.as_slice() else {
        return Err(io::Error::other("ungültige PASV-Adresse"));
    };
    // Use the connected peer's IP rather than the server's advertised address.
    let port = u16::from_be_bytes([*high, *low]);
    if port < 1024 {
        return Err(io::Error::other("FXP-Datenport muss mindestens 1024 sein"));
    }
    Ok(port)
}

pub(super) fn parse_extended_passive_port(response: &str) -> io::Result<u16> {
    if let Some((prefix, _)) = response.rsplit_once(')') {
        for (opening, _) in prefix.rmatch_indices('(') {
            let [delimiter, network, address, port @ .., end] = &prefix.as_bytes()[opening + 1..]
            else {
                continue;
            };
            if delimiter.is_ascii_graphic()
                && network == delimiter
                && address == delimiter
                && end == delimiter
                && !port.is_empty()
                && port.iter().all(u8::is_ascii_digit)
                && let Ok(port) = std::str::from_utf8(port).unwrap_or_default().parse::<u16>()
                && port >= 1024
            {
                return Ok(port);
            }
        }
    }
    Err(io::Error::other("Ungültige FTP-EPSV-Antwort."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ftp::Connection;

    #[test]
    fn passive_port_rejects_privileged_or_malformed_ports() {
        assert_eq!(
            parse_passive_port("Entering Passive Mode (127,0,0,1,20,1)").unwrap(),
            5121
        );
        assert!(parse_passive_port("(127,0,0,1,0,22)").is_err());
        assert!(parse_passive_port("(127,0,0,1,256,0)").is_err());
    }

    #[test]
    fn extended_passive_port_accepts_ascii_delimiters_and_requires_blanks() {
        for response in [
            "229 (|||5121|)",
            "(|||5121|)",
            "(!!!5121!)",
            "((((5121()",
            "(77751217)",
            "()))5121))",
        ] {
            assert_eq!(
                parse_extended_passive_port(response).unwrap(),
                5121,
                "{response}"
            );
        }
        for response in [
            "(|||0|)",
            "(|||1023|)",
            "(|||65536|)",
            "(||2|5121|)",
            "(|||5121)",
            "(|||5121|extra)",
            "(|||+5121|)",
        ] {
            assert!(parse_extended_passive_port(response).is_err(), "{response}");
        }
    }

    #[test]
    #[cfg(windows)]
    #[ignore = "requires two disposable writable local FTP servers"]
    fn direct_server_transfer() {
        let connect = |name| {
            let address = std::env::var(name).unwrap();
            Connection::new(&address, "arco", Security::Plain)
                .unwrap()
                .authenticate("test-only")
                .unwrap()
        };
        let source = connect("ARCO_TEST_FTP");
        let target = connect("ARCO_TEST_FXP_TARGET");
        let left = source.initial_directory();
        let right = target.initial_directory();
        let cancel = AtomicBool::new(false);
        let fixture = tempfile::tempdir().unwrap();
        let file = fixture.path().join("source.txt");
        let received = fixture.path().join("received.txt");
        std::fs::write(&file, b"direct FTP transfer").unwrap();
        source
            .upload(&left, &file, "fxp-fixture.txt", false, &cancel)
            .unwrap();
        let size = source
            .fxp_to(
                (&left, "fxp-fixture.txt"),
                &target,
                (&right, "fxp-target.txt"),
                &cancel,
            )
            .unwrap();
        assert_eq!(size, 19);
        target
            .download(&right, "fxp-target.txt", &received, &cancel)
            .unwrap();
        assert_eq!(std::fs::read(received).unwrap(), b"direct FTP transfer");
    }
}
