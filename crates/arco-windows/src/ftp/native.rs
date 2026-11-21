use super::data::{decode_nvt, encode_nvt};
use super::fxp::{Control, parse_extended_passive_port, parse_passive_port};
use super::rate::Rate;
use super::{
    LIST_LIMIT, NativeRequest, Session, Url, check_cancel, check_segment, invalid_input, otp,
};
use percent_encoding::percent_decode_str;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::TryLockError;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(30);

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
                        format!(
                            "Der interne FTP-Transport unterstützt diese Option nicht: {argument}"
                        ),
                    ));
                }
            }
        }
        Ok(exchange)
    }
}

pub(super) fn execute(
    request: NativeRequest<'_>,
    arguments: &[&str],
    output: Option<File>,
    cancel: &AtomicBool,
    mut observe: impl FnMut() -> io::Result<()>,
) -> io::Result<Vec<u8>> {
    let NativeRequest {
        session,
        ref url,
        transfer,
    } = request;
    let exchange = Exchange::parse(arguments)?;
    let deadline = Instant::now() + exchange.max_time.unwrap_or(Duration::from_secs(1800));
    let _otp_guard = if session.connection.options.otp {
        Some(lock_otp(session, deadline, cancel)?)
    } else {
        None
    };
    let mut control = match &session.connection.proxy {
        Some(gateway) => session.gateway_login(gateway, url, cancel)?,
        None => otp_login(session, url, cancel)?,
    };
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
    let mut rate = transfer.then(|| session.transfer_rate());
    let mut content = Vec::new();
    if let Some(source) = exchange.upload {
        let mut file = File::open(source)?;
        file.seek(SeekFrom::Start(exchange.offset))?;
        send(
            &mut file,
            &mut data,
            exchange.ascii,
            rate.as_mut(),
            deadline,
            cancel,
        )?;
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
            rate.as_mut(),
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

fn lock_otp<'a>(
    session: &'a Session,
    deadline: Instant,
    cancel: &AtomicBool,
) -> io::Result<std::sync::MutexGuard<'a, ()>> {
    loop {
        check_time(cancel, deadline)?;
        match session.otp_lock.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(30)),
            Err(TryLockError::Poisoned(_)) => {
                return Err(io::Error::other(
                    "Die FTP-OTP-Verbindung kann nicht weiterverwendet werden.",
                ));
            }
        }
    }
}

fn otp_login(session: &Session, url: &Url, cancel: &AtomicBool) -> io::Result<Control> {
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
    let password = otp::response(&challenge, session.password.as_str(), cancel)?;
    control.command(&password, &[230], "OTP", cancel)?;
    Ok(control)
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
        parse_extended_passive_port(&response)?
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
    mut rate: Option<&mut Rate>,
    deadline: Instant,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let mut buffer = [0; 64 * 1024];
    let mut previous_cr = false;
    let mut converted = Vec::with_capacity(buffer.len() * 2 + 2);
    let mut transferred = 0;
    loop {
        check_time(cancel, deadline)?;
        let capacity = rate
            .as_deref()
            .map_or(buffer.len(), |rate| rate.chunk(buffer.len()));
        let count = file.read(&mut buffer[..capacity])?;
        if count == 0 {
            break;
        }
        if ascii {
            encode_nvt(&buffer[..count], &mut previous_cr, &mut converted);
            write_data(data, &converted, deadline, cancel)?;
        } else {
            write_data(data, &buffer[..count], deadline, cancel)?;
        }
        transferred += if ascii { converted.len() } else { count } as u64;
        if let Some(rate) = rate.as_deref_mut() {
            rate.wait(transferred, cancel)?;
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
    mut rate: Option<&mut Rate>,
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
        let capacity = rate
            .as_deref()
            .map_or(capacity, |rate| rate.chunk(capacity));
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
        if let Some(rate) = rate.as_deref_mut() {
            rate.wait(received, cancel)?;
        }
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
