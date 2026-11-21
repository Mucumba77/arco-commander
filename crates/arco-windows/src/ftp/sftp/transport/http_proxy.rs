use super::{AddressFamily, dial};
use http_auth::{PasswordClient, PasswordParams, digest::Algorithm};
use sha2::Digest as _;
use std::fmt::Write as _;
use std::io;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use zeroize::Zeroizing;

const HEADER_LIMIT: usize = 8 * 1024;
const CHALLENGE_BODY_LIMIT: usize = 64 * 1024;

pub(super) async fn connect(
    proxy: (&str, u16, AddressFamily),
    (host, port): (&str, u16),
    credentials: Option<(&str, &str)>,
) -> io::Result<TcpStream> {
    let target = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let mut stream = dial(proxy.0, proxy.1, proxy.2).await?;
    let mut response = request(&mut stream, &target, None).await?;
    if response.connected() {
        return Ok(stream);
    }
    let (user, password) = credentials
        .filter(|_| response.status == 407)
        .ok_or_else(rejected)?;
    let mut auth = response.authentication()?;
    for attempt in 0..2 {
        // http-auth 0.1.10 uses the supplied bytes directly in A2 for auth-int.
        // CONNECT has an empty entity body. Supply its hex digest per RFC 7616.
        let body_hash = match &auth {
            PasswordClient::Digest(digest) => match digest.algorithm() {
                Algorithm::Md5 => format!("{:x}", md5::Md5::digest([])),
                Algorithm::Sha256 => format!("{:x}", sha2::Sha256::digest([])),
                Algorithm::Sha512Trunc256 => format!("{:x}", sha2::Sha512_256::digest([])),
                _ => return Err(unsupported_auth()),
            },
            _ => String::new(),
        };
        let parameters = PasswordParams {
            username: user,
            password,
            uri: &target,
            method: "CONNECT",
            body: Some(body_hash.as_bytes()),
        };
        let value = Zeroizing::new(auth.respond(&parameters).map_err(|_| unsupported_auth())?);
        let reused = if let Some(length) = response.remaining {
            discard_body(&mut stream, length).await.is_ok()
        } else {
            false
        };
        if !reused {
            stream = dial(proxy.0, proxy.1, proxy.2).await?;
        }
        response = match request(&mut stream, &target, Some(&value)).await {
            Err(error) if reused && closed(&error) => {
                stream = dial(proxy.0, proxy.1, proxy.2).await?;
                request(&mut stream, &target, Some(&value)).await?
            }
            result => result?,
        };
        if response.connected() {
            return Ok(stream);
        }
        if response.status != 407 || attempt > 0 {
            break;
        }
        match (&auth, response.authentication()?) {
            (PasswordClient::Digest(previous), PasswordClient::Digest(next))
                if next.stale()
                    && previous.realm() == next.realm()
                    && previous.nonce() != next.nonce() =>
            {
                auth = PasswordClient::Digest(next);
            }
            _ => break,
        }
    }
    Err(rejected())
}

#[derive(Debug)]
struct Response {
    status: u16,
    challenges: Vec<String>,
    remaining: Option<usize>,
}

impl Response {
    fn connected(&self) -> bool {
        (200..300).contains(&self.status)
    }

    fn authentication(&self) -> io::Result<PasswordClient> {
        // Some older proxies challenge with a bare 407 for Basic.
        let mut builder = PasswordClient::builder();
        if self.challenges.is_empty() {
            builder = builder.challenges(r#"Basic realm="""#);
        }
        for challenge in &self.challenges {
            builder = builder.challenges(challenge);
        }
        builder.build().map_err(|_| unsupported_auth())
    }
}

async fn request(
    stream: &mut TcpStream,
    target: &str,
    authorization: Option<&str>,
) -> io::Result<Response> {
    let mut request = Zeroizing::new(format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n"));
    if let Some(authorization) = authorization {
        write!(request, "Proxy-Authorization: {authorization}\r\n").expect("write to string");
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).await?;
    for _ in 0..4 {
        let response = read_head(stream).await?;
        if response.status >= 200 {
            return Ok(response);
        }
    }
    Err(invalid_response())
}

async fn read_head(stream: &mut TcpStream) -> io::Result<Response> {
    let mut bytes = Vec::with_capacity(512);
    // Leave the SSH greeting in the stream after a successful CONNECT.
    while bytes.len() < HEADER_LIMIT {
        bytes.push(stream.read_u8().await?);
        if bytes.ends_with(b"\r\n\r\n") {
            return parse_response(&bytes);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "Die Antwort des HTTP-Proxys ist zu groß.",
    ))
}

fn parse_response(bytes: &[u8]) -> io::Result<Response> {
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut response = httparse::Response::new(&mut headers);
    let parsed = response.parse(bytes).map_err(|_| invalid_response())?;
    let httparse::Status::Complete(header_end) = parsed else {
        return Err(invalid_response());
    };
    let status = response.code.ok_or_else(invalid_response)?;
    if header_end != bytes.len() || !(100..=599).contains(&status) {
        return Err(invalid_response());
    }
    let mut challenges = Vec::new();
    let mut length = None;
    let mut can_reuse = response.version == Some(1);
    if status == 407 {
        for header in response.headers {
            if header.name.eq_ignore_ascii_case("Proxy-Authenticate") {
                let challenge =
                    std::str::from_utf8(header.value).map_err(|_| unsupported_auth())?;
                challenges.push(challenge.to_owned());
            } else if header.name.eq_ignore_ascii_case("Connection")
                || header.name.eq_ignore_ascii_case("Proxy-Connection")
            {
                can_reuse &= !header
                    .value
                    .split(|byte| *byte == b',')
                    .any(|value| value.trim_ascii().eq_ignore_ascii_case(b"close"));
            } else if header.name.eq_ignore_ascii_case("Transfer-Encoding") {
                can_reuse = false;
            } else if header.name.eq_ignore_ascii_case("Content-Length") {
                let next = std::str::from_utf8(header.value.trim_ascii())
                    .ok()
                    .filter(|text| text.bytes().all(|byte| byte.is_ascii_digit()))
                    .and_then(|text| text.parse::<usize>().ok());
                can_reuse &= next.is_some() && length.is_none_or(|previous| Some(previous) == next);
                length = next;
            }
        }
    }
    Ok(Response {
        status,
        challenges,
        remaining: length.filter(|&n| can_reuse && n <= CHALLENGE_BODY_LIMIT),
    })
}

async fn discard_body(stream: &mut TcpStream, mut length: usize) -> io::Result<()> {
    let mut buffer = [0; 4096];
    while length != 0 {
        let chunk = length.min(buffer.len());
        stream.read_exact(&mut buffer[..chunk]).await?;
        length -= chunk;
    }
    Ok(())
}

fn closed(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::UnexpectedEof
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::BrokenPipe
    )
}

fn rejected() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "Der HTTP-Proxy hat den SSH-Tunnel abgelehnt. Prüfe auch die Proxy-Anmeldedaten.",
    )
}

fn unsupported_auth() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "Der HTTP-Proxy hat keine verwendbare Basic- oder Digest-Anmeldung angeboten.",
    )
}

fn invalid_response() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Die Antwort des HTTP-Proxys ist ungültig.",
    )
}
