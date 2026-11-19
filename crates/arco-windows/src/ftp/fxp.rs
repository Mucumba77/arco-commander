use super::{AddressFamily, Security, Session, Url, check_cancel, check_segment};
use percent_encoding::percent_decode_str;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{IpAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

const REPLY_TIMEOUT: Duration = Duration::from_secs(30);
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(1800);

impl Session {
    pub fn fxp_to(
        &self,
        from: (&Url, &str),
        other: &Self,
        to: (&Url, &str),
        cancel: &AtomicBool,
    ) -> io::Result<u64> {
        for session in [self, other] {
            if session.connection.security != Security::Plain
                || session.connection.proxy.is_some()
                || session.connection.options.otp
                || session.connection.options.family == AddressFamily::Ipv6
            {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "FXP erfordert zwei direkt verbundene Klartext-FTP-Server",
                ));
            }
        }
        let (source_dir, source_file) = from;
        let (target_dir, target_file) = to;
        self.validate(source_dir)?;
        other.validate(target_dir)?;
        check_segment(source_file)?;
        check_segment(target_file)?;
        if source_dir.host_str() == target_dir.host_str()
            && source_dir.port_or_known_default() == target_dir.port_or_known_default()
        {
            return Err(io::Error::other(
                "FXP benötigt zwei verschiedene FTP-Endpunkte",
            ));
        }
        let unique = tempfile::tempdir()?;
        let token = unique
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| io::Error::other("fehlender temporärer Name"))?;
        let staging = format!(".arco-fxp-{token}");
        check_target(other, target_dir, &[target_file, &staging], cancel)?;
        let mut source = Control::connect(self, source_dir, cancel)?;
        let mut target = Control::connect(other, target_dir, cancel)?;
        let expected = source.size(source_file, cancel)?;
        let (_, passive) = target.command("PASV", &[227], "PASV", cancel)?;
        let port = parse_passive_port(&passive)?;
        let IpAddr::V4(peer) = target.peer else {
            return Err(io::Error::other("FXP benötigt IPv4."));
        };
        let [a, b, c, d] = peer.octets();
        let [hi, lo] = port.to_be_bytes();
        source.command(
            &format!("PORT {a},{b},{c},{d},{hi},{lo}"),
            &[200],
            "PORT",
            cancel,
        )?;
        target.send(&format!("STOR {staging}"), cancel)?;
        let result = (|| {
            source.send(&format!("RETR {source_file}"), cancel)?;
            source.expect(&[125, 150], "RETR", REPLY_TIMEOUT, cancel)?;
            target.expect(&[125, 150], "STOR", REPLY_TIMEOUT, cancel)?;
            source.expect(&[226, 250], "Quelltransfer", TRANSFER_TIMEOUT, cancel)?;
            target.expect(&[226, 250], "Zieltransfer", TRANSFER_TIMEOUT, cancel)?;
            if source.size(source_file, cancel)? != expected
                || target.size(&staging, cancel)? != expected
            {
                return Err(io::Error::other("FTP-Größenprüfung fehlgeschlagen"));
            }
            check_target(other, target_dir, &[target_file], cancel)?;
            target.command(&format!("RNFR {staging}"), &[350], "RNFR", cancel)?;
            target.command(&format!("RNTO {target_file}"), &[250], "RNTO", cancel)?;
            Ok(expected)
        })();
        result.map_err(|error: io::Error| {
            io::Error::new(
                error.kind(),
                format!("{error}. Mögliche Teildatei am Ziel: {staging}"),
            )
        })
    }
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
    fn connect(session: &Session, directory: &Url, cancel: &AtomicBool) -> io::Result<Self> {
        let mut control = Self::connect_peer(session, directory, AddressFamily::Ipv4, cancel)?;
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
        let stream = stream
            .ok_or_else(|| io::Error::other("FTP konnte keine Steuerverbindung herstellen"))?;
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
            return Err(io::Error::other(format!(
                "FTP-Server meldet Status {status} bei {operation}"
            )));
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
