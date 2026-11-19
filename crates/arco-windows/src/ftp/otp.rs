//! RFC 2289 login. The challenge and response share the same control connection.
use super::data::{decode_nvt, encode_nvt};
use super::fxp::{Control, parse_passive_port};
use super::{LIST_LIMIT, Session, Url, check_cancel, check_segment, invalid_input};
use md5::{Digest, Md5};
use percent_encoding::percent_decode_str;
use sha1::Sha1;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::TryLockError;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

const TIMEOUT: Duration = Duration::from_secs(30);

fn response(
    challenge: &str,
    passphrase: &str,
    cancel: &AtomicBool,
) -> io::Result<Zeroizing<String>> {
    let mut tokens = challenge.split_ascii_whitespace();
    let hash = tokens
        .find(|word| word.starts_with("otp-"))
        .ok_or_else(|| invalid_input("Die FTP-Anmeldung bietet keine RFC-2289-Challenge an."))?;
    let sequence: u32 = tokens
        .next()
        .and_then(|word| word.parse().ok())
        .filter(|count| *count <= 9999)
        .ok_or_else(|| invalid_input("Ungültige OTP-Sequenz."))?;
    let seed = tokens
        .next()
        .filter(|word| {
            (1..=16).contains(&word.len()) && word.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
        .ok_or_else(|| invalid_input("Ungültiger OTP-Seed."))?;
    if !(10..=63).contains(&passphrase.len()) || !passphrase.is_ascii() {
        return Err(invalid_input(
            "Die OTP-Passphrase benötigt 10–63 ASCII-Zeichen.",
        ));
    }
    let fold: fn(&[u8]) -> io::Result<[u8; 8]> = match hash {
        "otp-md4" => fold_md4,
        "otp-md5" => |input| {
            let bytes = Md5::digest(input);
            Ok(std::array::from_fn(|index| bytes[index] ^ bytes[index + 8]))
        },
        "otp-sha1" => |input| {
            let bytes = Sha1::digest(input);
            let mut folded = [0; 8];
            for (index, byte) in bytes.into_iter().enumerate() {
                folded[index / 4 % 2 * 4 + (3 - index % 4)] ^= byte;
            }
            Ok(folded)
        },
        _ => {
            return Err(invalid_input(
                "Diese FTP-OTP-Variante wird nicht unterstützt. Wähle RFC 2289 mit MD4, MD5 oder SHA-1.",
            ));
        }
    };
    let initial = Zeroizing::new(format!("{}{passphrase}", seed.to_ascii_lowercase()));
    let mut value = Zeroizing::new(fold(initial.as_bytes())?);
    for _ in 0..sequence {
        check_cancel(cancel)?;
        *value = fold(&*value)?;
    }
    let mut password = Zeroizing::new(String::from("PASS "));
    use std::fmt::Write as _;
    for byte in value.iter() {
        write!(&mut *password, "{byte:02x}").expect("format OTP");
    }
    Ok(password)
}

#[cfg(windows)]
fn fold_md4(input: &[u8]) -> io::Result<[u8; 8]> {
    use windows_sys::Win32::Security::Cryptography::{BCRYPT_MD4_ALG_HANDLE, BCryptHash};

    let mut digest = Zeroizing::new([0; 16]);
    // The Windows 10/11 CNG MD4 pseudo-handle needs no open or close operation.
    let status = unsafe {
        BCryptHash(
            BCRYPT_MD4_ALG_HANDLE,
            std::ptr::null(),
            0,
            input.as_ptr(),
            input.len() as u32,
            digest.as_mut_ptr(),
            digest.len() as u32,
        )
    };
    if status < 0 {
        return Err(io::Error::other(format!(
            "MD4 für FTP-OTP ist nicht verfügbar (0x{status:08x})."
        )));
    }
    Ok(std::array::from_fn(|index| {
        digest[index] ^ digest[index + 8]
    }))
}

#[cfg(not(windows))]
fn fold_md4(_: &[u8]) -> io::Result<[u8; 8]> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "MD4 für FTP-OTP benötigt Windows.",
    ))
}

#[derive(Default)]
struct Exchange<'a> {
    quotes: Vec<&'a str>,
    head: bool,
    replies: bool,
    ascii: bool,
    operation: Option<&'a str>,
    upload: Option<&'a str>,
    offset: u64,
    range: Option<u64>,
    limit: Option<u64>,
    max_time: Option<Duration>,
}

impl<'a> Exchange<'a> {
    // Translate only the requests emitted by this FTP module, never user CLI input.
    fn parse(arguments: &'a [&'a str]) -> io::Result<Self> {
        let mut exchange = Self::default();
        let mut arguments = arguments.iter().copied();
        let missing = || invalid_input("Unvollständiger FTP-Transferparameter.");
        while let Some(argument) = arguments.next() {
            match argument {
                "--head" => exchange.head = true,
                "--ignore-content-length" => (),
                "--quote" => exchange.quotes.push(arguments.next().ok_or_else(missing)?),
                "--dump-header" | "--output" => {
                    let value = arguments.next().ok_or_else(missing)?;
                    if argument == "--dump-header" && value == "-" {
                        exchange.replies = true;
                    }
                }
                "--request" => exchange.operation = Some(arguments.next().ok_or_else(missing)?),
                "--list-only" => exchange.operation = Some("NLST"),
                "--use-ascii" => exchange.ascii = true,
                "--upload-file" => exchange.upload = Some(arguments.next().ok_or_else(missing)?),
                "--max-time" => {
                    exchange.max_time = Some(Duration::from_secs(parse_number(
                        arguments.next().ok_or_else(missing)?,
                    )?))
                }
                "--max-filesize" => {
                    exchange.limit = Some(parse_number(arguments.next().ok_or_else(missing)?)?)
                }
                "--continue-at" => {
                    exchange.offset = parse_number(arguments.next().ok_or_else(missing)?)?
                }
                "--range" => {
                    let (from, to) = arguments
                        .next()
                        .and_then(|value| value.split_once('-'))
                        .ok_or_else(missing)?;
                    exchange.offset = parse_number(from)?;
                    exchange.range = Some(
                        parse_number(to)?
                            .checked_sub(exchange.offset)
                            .and_then(|count| count.checked_add(1))
                            .ok_or_else(missing)?,
                    );
                }
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        format!("FTP-OTP unterstützt diese Transferoption nicht: {argument}"),
                    ));
                }
            }
        }
        Ok(exchange)
    }
}

pub(super) fn execute(
    session: &Session,
    url: &Url,
    arguments: &[&str],
    output: Option<File>,
    cancel: &AtomicBool,
    mut observe: impl FnMut() -> io::Result<()>,
) -> io::Result<Vec<u8>> {
    let exchange = Exchange::parse(arguments)?;
    let deadline = Instant::now() + exchange.max_time.unwrap_or(Duration::from_secs(1800));
    let _guard = loop {
        check_time(cancel, deadline)?;
        match session.otp_lock.try_lock() {
            Ok(guard) => break guard,
            Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(30)),
            Err(TryLockError::Poisoned(_)) => {
                return Err(io::Error::other(
                    "Die FTP-OTP-Verbindung kann nicht weiterverwendet werden.",
                ));
            }
        }
    };
    let mut control =
        Control::connect_peer(session, url, session.connection.options.family, cancel)?;
    control.send(&format!("USER {}", session.connection.user), cancel)?;
    let (status, challenge) = control.reply(TIMEOUT, cancel)?;
    if status != 331 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Der FTP-Server hat keine OTP-Anmeldung angeboten.",
        ));
    }
    let password = response(&challenge, session.password.as_str(), cancel)?;
    control.command(&password, &[230], "OTP", cancel)?;
    let mut replies = Vec::new();
    run_quotes(&mut control, &exchange, false, &mut replies, cancel)?;

    let (parent, name) = if url.path().ends_with('/') {
        (url.path(), None)
    } else {
        let (parent, name) = url
            .path()
            .rsplit_once('/')
            .ok_or_else(|| invalid_input("FTP-Dateiname fehlt."))?;
        let name = percent_decode_str(name)
            .decode_utf8()
            .map_err(io::Error::other)?;
        check_segment(&name)?;
        // A leading double slash addresses the remote filesystem root.
        let parent = if parent == "/" && url.path().starts_with("//") {
            "//"
        } else {
            parent
        };
        (parent, Some(name))
    };
    control.change_directory(parent, cancel)?;
    if exchange.head {
        run_quotes(&mut control, &exchange, true, &mut replies, cancel)?;
        return Ok(replies);
    }

    let listing = name.is_none();
    control.command(
        if exchange.ascii || listing {
            "TYPE A"
        } else {
            "TYPE I"
        },
        &[200],
        "TYPE",
        cancel,
    )?;
    run_quotes(&mut control, &exchange, true, &mut replies, cancel)?;
    let mut data = passive(&mut control, cancel)?;
    if exchange.offset > 0 && exchange.upload.is_none() {
        control.command(&format!("REST {}", exchange.offset), &[350], "REST", cancel)?;
    }
    let operation = match name {
        Some(name) if exchange.upload.is_some() && exchange.offset > 0 => format!("APPE {name}"),
        Some(name) if exchange.upload.is_some() => format!("STOR {name}"),
        Some(name) => format!("RETR {name}"),
        None => exchange.operation.unwrap_or("LIST").to_owned(),
    };
    control.command(&operation, &[125, 150], "Datentransfer", cancel)?;
    let mut content = Vec::new();
    if let Some(source) = exchange.upload {
        let mut file = File::open(source)?;
        file.seek(SeekFrom::Start(exchange.offset))?;
        send(&mut file, &mut data, exchange.ascii, deadline, cancel)?;
        data.shutdown(std::net::Shutdown::Write)?;
    } else {
        let destination: &mut dyn Write = match &output {
            Some(file) => &mut &*file,
            None => &mut content,
        };
        let limit = if output.is_none() {
            Some(LIST_LIMIT as u64)
        } else {
            exchange.limit
        };
        receive(
            &mut data,
            destination,
            exchange.range,
            limit,
            exchange.ascii && !listing,
            deadline,
            cancel,
            &mut observe,
        )?;
    }
    drop(data);
    let accepted: &[u16] = if exchange.range.is_some() {
        &[226, 250, 426]
    } else {
        &[226, 250]
    };
    control.expect(accepted, "Transferabschluss", TIMEOUT, cancel)?;
    observe()?;
    check_time(cancel, deadline)?;
    if exchange.replies {
        replies.extend(content);
        return Ok(replies);
    }
    Ok(content)
}

fn run_quotes(
    control: &mut Control,
    exchange: &Exchange<'_>,
    after_cwd: bool,
    replies: &mut Vec<u8>,
    cancel: &AtomicBool,
) -> io::Result<()> {
    for command in &exchange.quotes {
        let after = command.starts_with('+');
        if after != after_cwd {
            continue;
        }
        let command = command.strip_prefix('+').unwrap_or(command);
        let (command, optional) = command
            .strip_prefix('*')
            .map_or((command, false), |command| (command, true));
        control.send(command, cancel)?;
        let (code, reply) = control.reply(TIMEOUT, cancel)?;
        if code >= 400 && !optional {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("FTP-Server lehnt den Befehl ab (Status {code})."),
            ));
        }
        if exchange.replies {
            let (bytes, _, _) = control.encoding().encode(&reply);
            replies.extend_from_slice(&bytes);
            replies.extend_from_slice(b"\r\n");
        }
    }
    Ok(())
}

fn passive(control: &mut Control, cancel: &AtomicBool) -> io::Result<TcpStream> {
    control.send("EPSV", cancel)?;
    let (status, response) = control.reply(TIMEOUT, cancel)?;
    let port = if status == 229 {
        let field = response
            .rsplit_once('(')
            .and_then(|(_, tail)| tail.split_once(')'))
            .map(|(field, _)| field);
        field
            .and_then(|field| {
                let delimiter = field.chars().next()?;
                let mut components = field.split(delimiter);
                if components.next() != Some("")
                    || components.next() != Some("")
                    || components.next() != Some("")
                {
                    return None;
                }
                let port = components.next()?.parse::<u16>().ok()?;
                (port >= 1024 && components.next() == Some("") && components.next().is_none())
                    .then_some(port)
            })
            .ok_or_else(|| io::Error::other("Ungültige FTP-EPSV-Antwort."))?
    } else if control.peer.is_ipv4() {
        let (_, reply) = control.command("PASV", &[227], "PASV", cancel)?;
        parse_passive_port(&reply)?
    } else {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Der IPv6-FTP-Server unterstützt EPSV nicht.",
        ));
    };
    check_cancel(cancel)?;
    let data = TcpStream::connect_timeout(
        &SocketAddr::new(control.peer, port),
        Duration::from_secs(10),
    )?;
    data.set_read_timeout(Some(Duration::from_millis(250)))?;
    data.set_write_timeout(Some(Duration::from_millis(250)))?;
    Ok(data)
}

fn send(
    file: &mut File,
    data: &mut TcpStream,
    ascii: bool,
    deadline: Instant,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let mut buffer = [0; 64 * 1024];
    let mut previous_cr = false;
    let mut converted = Vec::with_capacity(buffer.len() * 2 + 2);
    loop {
        check_time(cancel, deadline)?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        if ascii {
            encode_nvt(&buffer[..count], &mut previous_cr, &mut converted);
            write_data(data, &converted, deadline, cancel)?;
        } else {
            write_data(data, &buffer[..count], deadline, cancel)?;
        }
    }
    if previous_cr {
        write_data(data, b"\r\0", deadline, cancel)?;
    }
    Ok(())
}

fn write_data(
    data: &mut TcpStream,
    mut bytes: &[u8],
    deadline: Instant,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let mut last_progress = Instant::now();
    while !bytes.is_empty() {
        check_time(cancel, deadline.min(last_progress + TIMEOUT))?;
        match data.write(bytes) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(count) => {
                bytes = &bytes[count..];
                last_progress = Instant::now();
            }
            Err(error) if timed_out(&error) => (),
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn receive(
    data: &mut TcpStream,
    output: &mut dyn Write,
    range: Option<u64>,
    limit: Option<u64>,
    ascii: bool,
    deadline: Instant,
    cancel: &AtomicBool,
    observe: &mut impl FnMut() -> io::Result<()>,
) -> io::Result<()> {
    let mut buffer = [0; 64 * 1024];
    let mut converted = Vec::with_capacity(buffer.len() + 1);
    let mut received = 0u64;
    let mut previous_cr = false;
    let mut last_progress = Instant::now();
    loop {
        check_time(cancel, deadline.min(last_progress + TIMEOUT))?;
        let capacity = range.map_or(buffer.len(), |range| {
            (range - received).min(buffer.len() as u64) as usize
        });
        if capacity == 0 {
            break;
        }
        let count = match data.read(&mut buffer[..capacity]) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if timed_out(&error) => continue,
            Err(error) => return Err(error),
        };
        last_progress = Instant::now();
        received += count as u64;
        if limit.is_some_and(|limit| received > limit) {
            return Err(io::Error::other(
                "Der FTP-Transfer überschreitet das Größenlimit.",
            ));
        }
        if ascii {
            decode_nvt(&buffer[..count], &mut previous_cr, &mut converted);
            output.write_all(&converted)?;
        } else {
            output.write_all(&buffer[..count])?;
        }
        observe()?;
    }
    if previous_cr {
        output.write_all(b"\r")?;
    }
    if range.is_some_and(|expected| received != expected) {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    Ok(())
}

fn parse_number(value: &str) -> io::Result<u64> {
    value.parse().map_err(io::Error::other)
}

fn check_time(cancel: &AtomicBool, deadline: Instant) -> io::Result<()> {
    check_cancel(cancel)?;
    if Instant::now() >= deadline {
        return Err(io::ErrorKind::TimedOut.into());
    }
    Ok(())
}

fn timed_out(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc2289_matches_known_responses_without_plaintext_fallback() {
        let cancel = AtomicBool::new(false);
        for (algorithm, count, expected) in [
            #[cfg(windows)]
            ("md4", 0, "d1854218ebbb0b51"),
            #[cfg(windows)]
            ("md4", 1, "63473ef01cd0b444"),
            ("md5", 0, "9e876134d90499dd"),
            ("md5", 99, "50fe1962c4965880"),
            ("sha1", 0, "bb9e6ae1979d8ff4"),
            ("sha1", 99, "87fec7768b73ccf9"),
        ] {
            let challenge = format!("331 otp-{algorithm} {count} TeSt ext");
            assert_eq!(
                *response(&challenge, "This is a test.", &cancel).unwrap(),
                format!("PASS {expected}")
            );
        }
        assert!(response("331 Password", "This is a test.", &cancel).is_err());
        assert!(response("331 otp-md5 10000 test", "This is a test.", &cancel).is_err());
    }
}
