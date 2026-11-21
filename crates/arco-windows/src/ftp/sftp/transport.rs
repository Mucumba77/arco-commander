use super::{ChallengeHandler, Options, SshAgent, SshChallenge, Wire, runtime};
use crate::ftp::{AddressFamily, Connection, Session, check_cancel, invalid_input, local_proxy};
use arco_core::ssh_host_keys::HostKeyPins;
use russh::client::{AuthResult, KeyboardInteractiveAuthResponse};
use russh::keys::agent::{AgentIdentity, client::AgentClient};
use russh::keys::{
    Algorithm, HashAlg, PublicKeyOrCertificate, decode_secret_key, key::PrivateKeyWithHashAlg,
};
use russh::{ChannelMsg, client};
use std::future::Future;
use std::io::{self, Read};
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use zeroize::Zeroizing;

mod http_proxy;
mod socks_chap;

pub(in crate::ftp) struct SftpConnection {
    pub(super) wire: Wire,
    ssh: SshConnection,
}

impl SftpConnection {
    pub(super) fn is_closed(&self) -> bool {
        self.ssh.target.is_closed() || self.ssh.jumps.iter().any(client::Handle::is_closed)
    }

    pub(super) fn peer_fingerprint(&self) -> Option<&str> {
        self.ssh.peer_fingerprint()
    }
}

pub(in crate::ftp) struct SshConnection {
    target: client::Handle<PinnedHost>,
    jumps: Vec<client::Handle<PinnedHost>>,
    fingerprint: Arc<OnceLock<String>>,
    _local_proxies: Vec<local_proxy::Process>,
}

impl SshConnection {
    pub(super) fn is_closed(&self) -> bool {
        self.target.is_closed() || self.jumps.iter().any(client::Handle::is_closed)
    }

    pub(super) fn peer_fingerprint(&self) -> Option<&str> {
        self.fingerprint.get().map(String::as_str)
    }
}

impl SshConnection {
    #[cfg(windows)]
    pub(super) async fn terminal(
        &self,
        command: Option<&str>,
        width: u32,
        height: u32,
        timeout: u64,
        cancel: &AtomicBool,
    ) -> io::Result<russh::Channel<client::Msg>> {
        bounded(cancel, timeout, async {
            let mut channel = self
                .target
                .channel_open_session()
                .await
                .map_err(io::Error::other)?;
            // Advertise UTF-8 terminal input and an ordinary VT-compatible screen.
            channel
                .request_pty(true, "xterm", width, height, 0, 0, &[])
                .await
                .map_err(io::Error::other)?;
            match channel.wait().await {
                Some(ChannelMsg::Success) => (),
                _ => {
                    return Err(io::Error::other(
                        "Der SSH-Server stellt kein Terminal bereit.",
                    ));
                }
            }
            if let Some(command) = command {
                channel
                    .exec(true, command)
                    .await
                    .map_err(io::Error::other)?;
            } else {
                channel
                    .request_shell(true)
                    .await
                    .map_err(io::Error::other)?;
            }
            // The relay handles the command reply and any output in order.
            Ok(channel)
        })
        .await
    }

    pub(super) async fn exec(
        &self,
        command: String,
        timeout: u64,
        cancel: &AtomicBool,
    ) -> io::Result<russh::Channel<client::Msg>> {
        bounded(cancel, timeout, async {
            let mut channel = self
                .target
                .channel_open_session()
                .await
                .map_err(io::Error::other)?;
            channel
                .exec(true, command)
                .await
                .map_err(io::Error::other)?;
            match channel.wait().await {
                Some(ChannelMsg::Success) => Ok(channel),
                _ => Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "Der SSH-Server hat den SCP-Kanal abgelehnt.",
                )),
            }
        })
        .await
    }
}

#[derive(Debug)]
enum HandshakeError {
    Transport(russh::Error),
    PinMismatch(Algorithm, io::Error),
    Certificate(io::Error),
}

impl From<russh::Error> for HandshakeError {
    fn from(error: russh::Error) -> Self {
        Self::Transport(error)
    }
}

struct PinnedHost {
    name: String,
    fingerprints: HostKeyPins,
    verified: Arc<OnceLock<String>>,
}

impl client::Handler for PinnedHost {
    type Error = HandshakeError;

    async fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        if self.fingerprints.is_empty() {
            // This path exists only in GSS-only mode. The exchange already
            // authenticated the explicitly configured Kerberos principal.
            return Ok(true);
        }
        let PublicKeyOrCertificate::PublicKey { key, .. } = key else {
            return Err(HandshakeError::Certificate(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Das Profil erfordert einen SSH-Hostschlüssel mit SHA256-Fingerabdruck.",
            )));
        };
        let actual = key.fingerprint(HashAlg::Sha256);
        if self.fingerprints.contains(actual.as_bytes()) {
            let _ = self.verified.set(actual.to_string());
            Ok(true)
        } else {
            Err(HandshakeError::PinMismatch(
                key.algorithm(),
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "Der SSH-Hostschlüssel für {} weicht vom Profil ab. Server: {actual}",
                        self.name
                    ),
                ),
            ))
        }
    }
}

fn endpoint(connection: &Connection) -> io::Result<(String, u16)> {
    Ok((
        hostname(&connection.url)?,
        connection.url.port().unwrap_or(22),
    ))
}

fn hostname(address: &url::Url) -> io::Result<String> {
    let host = match address.host() {
        Some(url::Host::Ipv6(address)) => address.to_string(),
        Some(url::Host::Ipv4(address)) => address.to_string(),
        Some(url::Host::Domain(host)) => {
            let host = percent_encoding::percent_decode_str(host)
                .decode_utf8()
                .map_err(io::Error::other)?;
            url::Host::parse(&host)
                .map_err(io::Error::other)?
                .to_string()
        }
        None => return Err(invalid_input("Die Adresse enthält keinen Server.")),
    };
    Ok(host)
}

async fn addresses(host: &str, port: u16, family: AddressFamily) -> io::Result<Vec<SocketAddr>> {
    Ok(tokio::net::lookup_host((host, port))
        .await?
        .filter(move |address| match family {
            AddressFamily::Automatic => true,
            AddressFamily::Ipv4 => address.is_ipv4(),
            AddressFamily::Ipv6 => address.is_ipv6(),
        })
        .take(4)
        .collect())
}

async fn dial(host: &str, port: u16, family: AddressFamily) -> io::Result<TcpStream> {
    let mut last = io::Error::new(
        io::ErrorKind::AddrNotAvailable,
        "Keine passende SSH-Adresse.",
    );
    for address in addresses(host, port, family).await? {
        match tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(address)).await {
            Ok(Ok(stream)) => {
                stream.set_nodelay(true)?;
                return Ok(stream);
            }
            Ok(Err(error)) => last = error,
            Err(_) => last = io::ErrorKind::TimedOut.into(),
        }
    }
    Err(last)
}

async fn connect_tcp(connection: &Connection) -> io::Result<TcpStream> {
    let (host, port) = proxy_endpoint(connection).await?;
    let family = connection.options.family;
    let Some(proxy) = &connection.proxy else {
        return dial(&host, port, family).await;
    };
    let proxy_port = proxy
        .port_or_known_default()
        .unwrap_or(if proxy.scheme() == "telnet" { 23 } else { 1080 });
    let proxy_host = hostname(proxy)?;
    if proxy.scheme() == "http" {
        return http_proxy::connect(
            (&proxy_host, proxy_port, family),
            (&host, port),
            connection.proxy_credentials(),
        )
        .await;
    }
    let mut stream = dial(&proxy_host, proxy_port, family).await?;
    if let Some(command) = &connection.telnet_proxy {
        let bytes = command.render(
            &host,
            port,
            &connection.proxy_user,
            &connection.proxy_password,
        )?;
        tokio::time::timeout(Duration::from_secs(10), stream.write_all(&bytes))
            .await
            .map_err(|_| {
                io::Error::new(io::ErrorKind::TimedOut, "Telnet-Proxy antwortet nicht.")
            })??;
    } else if connection.proxy_uses_user_id_only() {
        let target = match host.parse::<IpAddr>() {
            Ok(IpAddr::V4(ip)) => Some(ip),
            Ok(IpAddr::V6(_)) => return Err(invalid_input("SOCKS4 benötigt ein IPv4-Ziel.")),
            Err(_) if proxy.scheme() == "socks4a" => None,
            Err(_) if family == AddressFamily::Ipv6 => {
                return Err(invalid_input(
                    "SOCKS4 mit lokaler DNS-Auflösung benötigt IPv4.",
                ));
            }
            Err(_) => Some(
                addresses(&host, port, AddressFamily::Ipv4)
                    .await?
                    .into_iter()
                    .find_map(|address| {
                        if let SocketAddr::V4(address) = address {
                            Some(*address.ip())
                        } else {
                            None
                        }
                    })
                    .ok_or(io::ErrorKind::AddrNotAvailable)?,
            ),
        };
        connect_socks4(&mut stream, &host, port, target, &connection.proxy_user).await?;
    } else {
        let ip = match (proxy.scheme(), host.parse::<IpAddr>()) {
            (_, Ok(address)) => Some(address),
            ("socks5", Err(_)) => Some(
                addresses(&host, port, family)
                    .await?
                    .into_iter()
                    .next()
                    .ok_or(io::ErrorKind::AddrNotAvailable)?
                    .ip(),
            ),
            _ => None,
        };
        connect_socks5(&mut stream, &host, port, ip, connection.proxy_credentials()).await?;
    }
    Ok(stream)
}

async fn proxy_endpoint(connection: &Connection) -> io::Result<(String, u16)> {
    let (host, port) = endpoint(connection)?;
    if !connection.ssh_proxy_dns_locally || host.parse::<IpAddr>().is_ok() {
        return Ok((host, port));
    }
    let mut family = connection.options.family;
    if family == AddressFamily::Automatic
        && connection
            .proxy
            .as_ref()
            .is_some_and(|url| matches!(url.scheme(), "socks4" | "socks4a"))
    {
        family = AddressFamily::Ipv4;
    }
    let ip = addresses(&host, port, family)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::AddrNotAvailable,
                "Der SSH-Zielname lässt sich lokal nicht auflösen.",
            )
        })?
        .ip();
    Ok((ip.to_string(), port))
}

async fn connect_socks4(
    stream: &mut TcpStream,
    host: &str,
    port: u16,
    ip: Option<std::net::Ipv4Addr>,
    user: &str,
) -> io::Result<()> {
    let mut request = vec![4, 1];
    request.extend_from_slice(&port.to_be_bytes());
    request.extend_from_slice(&ip.map_or([0, 0, 0, 1], |address| address.octets()));
    request.extend_from_slice(user.as_bytes());
    request.push(0);
    if ip.is_none() {
        request.extend_from_slice(host.as_bytes());
        request.push(0);
    }
    stream.write_all(&request).await?;
    let mut reply = [0; 8];
    stream.read_exact(&mut reply).await?;
    if reply[0] != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Ungültige SOCKS4-Antwort.",
        ));
    }
    if reply[1] != 90 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "Der SOCKS4-Proxy hat den SSH-Tunnel abgelehnt. Status: {}",
                reply[1]
            ),
        ));
    }
    Ok(())
}

async fn connect_socks5(
    stream: &mut TcpStream,
    host: &str,
    port: u16,
    ip: Option<IpAddr>,
    credentials: Option<(&str, &str)>,
) -> io::Result<()> {
    let offered: &[u8] = if credentials.is_some() { &[3, 2] } else { &[0] };
    let mut negotiation = vec![5, offered.len() as u8];
    negotiation.extend_from_slice(offered);
    stream.write_all(&negotiation).await?;
    let mut greeting = [0; 2];
    stream.read_exact(&mut greeting).await?;
    if greeting[0] != 5 || !offered.contains(&greeting[1]) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Der SOCKS5-Proxy akzeptiert die konfigurierte Anmeldeart nicht.",
        ));
    }
    if let Some((user, password)) = credentials {
        if greeting[1] == 3 {
            socks_chap::authenticate(stream, user, password).await?;
        } else {
            socks_password(stream, user, password).await?;
        }
    }
    let mut request = vec![5, 1, 0];
    match ip {
        Some(IpAddr::V4(address)) => {
            request.push(1);
            request.extend_from_slice(&address.octets());
        }
        Some(IpAddr::V6(address)) => {
            request.push(4);
            request.extend_from_slice(&address.octets());
        }
        None => {
            let length = u8::try_from(host.len())
                .map_err(|_| invalid_input("Der SSH-Servername ist zu lang."))?;
            request.extend_from_slice(&[3, length]);
            request.extend_from_slice(host.as_bytes());
        }
    }
    request.extend_from_slice(&port.to_be_bytes());
    stream.write_all(&request).await?;
    let mut reply = [0; 4];
    stream.read_exact(&mut reply).await?;
    if reply[..3] != [5, 0, 0] {
        return Err(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            format!(
                "Der SOCKS5-Proxy hat den SSH-Tunnel abgelehnt. Status: {}",
                reply[1]
            ),
        ));
    }
    let bound_size = match reply[3] {
        1 => 4,
        4 => 16,
        3 => usize::from(stream.read_u8().await?),
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Ungültige SOCKS5-Zieladresse.",
            ));
        }
    };
    stream.read_exact(&mut [0; 257][..bound_size + 2]).await?;
    Ok(())
}

async fn socks_password(stream: &mut TcpStream, user: &str, password: &str) -> io::Result<()> {
    let user_len = u8::try_from(user.len())
        .map_err(|_| invalid_input("Der Proxy-Benutzername ist zu lang."))?;
    let password_len = u8::try_from(password.len())
        .map_err(|_| invalid_input("Das Proxy-Passwort ist zu lang."))?;
    let mut login = Zeroizing::new(Vec::with_capacity(user.len() + password.len() + 3));
    login.extend_from_slice(&[1, user_len]);
    login.extend_from_slice(user.as_bytes());
    login.push(password_len);
    login.extend_from_slice(password.as_bytes());
    stream.write_all(&login).await?;
    let mut reply = [0; 2];
    stream.read_exact(&mut reply).await?;
    if reply != [1, 0] {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Die SOCKS5-Proxy-Anmeldung wurde abgelehnt.",
        ));
    }
    Ok(())
}

struct EstablishedSsh {
    session: client::Handle<PinnedHost>,
    proxy: Option<local_proxy::Process>,
    fingerprint: Arc<OnceLock<String>>,
}

async fn establish(
    connection: &Connection,
    password: &str,
    previous: Option<&client::Handle<PinnedHost>>,
    challenge: Option<&ChallengeHandler>,
    cancel: &AtomicBool,
) -> io::Result<EstablishedSsh> {
    let options = connection
        .ssh
        .as_ref()
        .ok_or_else(|| invalid_input("SSH-Profil fehlt."))?;
    let (host, port) = endpoint(connection)?;
    let mut preferred = russh::Preferred::DEFAULT;
    let name = format!("{host}:{port}");
    #[cfg(windows)]
    let gss_kex = if options.kerberos_kex {
        let principal = options
            .kerberos_spn
            .as_deref()
            .ok_or_else(|| invalid_input("Kerberos-Dienstprinzipal fehlt."))?;
        Some(super::kerberos::key_exchange(
            principal,
            options.fingerprints.is_empty(),
        ))
    } else {
        None
    };
    let mut first_mismatch = None;
    loop {
        let verified = Arc::new(OnceLock::new());
        let handler = PinnedHost {
            name: name.clone(),
            fingerprints: options.fingerprints.clone(),
            verified: verified.clone(),
        };
        let offered = Arc::new(client::Config {
            preferred: preferred.clone(),
            #[cfg(windows)]
            gss_kex: gss_kex.clone(),
            keepalive_interval: options.keepalive.map(Duration::from_secs),
            keepalive_max: 3,
            ..Default::default()
        });
        let mut proxy_process = None;
        let result = bounded(cancel, options.timeout, async {
            Ok(match previous {
                Some(jump) => {
                    client::connect_stream(offered, tunnel(jump, connection).await?, handler).await
                }
                None => {
                    if let Some(command) = &connection.local_proxy {
                        let (host, port) = proxy_endpoint(connection).await?;
                        let (stream, process) = command.spawn(
                            &host,
                            port,
                            &connection.proxy_user,
                            &connection.proxy_password,
                        )?;
                        proxy_process = Some(process);
                        client::connect_stream(offered, stream, handler).await
                    } else {
                        client::connect_stream(offered, connect_tcp(connection).await?, handler)
                            .await
                    }
                }
            })
        })
        .await?;
        match result {
            Ok(mut session) => {
                authenticate(
                    &mut session,
                    &connection.user,
                    password,
                    options,
                    &name,
                    challenge,
                    cancel,
                )
                .await?;
                return Ok(EstablishedSsh {
                    session,
                    proxy: proxy_process,
                    fingerprint: verified,
                });
            }
            Err(HandshakeError::PinMismatch(observed, error)) => {
                // Only try the remaining algorithms already allowed by the client.
                // A new handshake must still match the configured trust set.
                let keys = preferred.key.to_mut();
                let old_count = keys.len();
                let rsa = matches!(observed, Algorithm::Rsa { .. });
                keys.retain(|key| {
                    key != &observed && !(rsa && matches!(key, Algorithm::Rsa { .. }))
                });
                if keys.is_empty() || keys.len() == old_count {
                    return Err(first_mismatch.unwrap_or(error));
                }
                first_mismatch.get_or_insert(error);
            }
            Err(HandshakeError::Transport(
                error @ russh::Error::NoCommonAlgo {
                    kind: russh::AlgorithmKind::Key,
                    ..
                },
            )) => {
                if let Some(error) = first_mismatch {
                    return Err(error);
                }
                if options.kerberos_kex
                    && !options.fingerprints.is_empty()
                    && matches!(&error, russh::Error::NoCommonAlgo { theirs, .. } if theirs == &["null"])
                {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "Der Kerberos-Server bietet keinen SSH-Hostschlüssel an. Das Profil verlangt den hinterlegten Fingerabdruck.",
                    ));
                }
                return Err(io::Error::other(error));
            }
            Err(HandshakeError::Certificate(error)) => return Err(error),
            Err(HandshakeError::Transport(error)) => return Err(transport_error(error)),
        }
    }
}

fn transport_error(error: russh::Error) -> io::Error {
    if let russh::Error::NoCommonAlgo {
        kind: russh::AlgorithmKind::Kex,
        theirs,
        ..
    } = &error
        && theirs.iter().any(|algorithm| algorithm.starts_with("gss-"))
    {
        return io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "Der SSH-Server bietet GSSAPI-Schlüsselaustausch an. Aktiviere ihn im Kerberos-Profil oder prüfe die gemeinsam unterstützten SHA-2-Verfahren. {error}"
            ),
        );
    }
    match error {
        russh::Error::IO(error) => error,
        error => io::Error::other(error),
    }
}

#[cfg(test)]
#[test]
fn negotiation_reports_unsupported_gss_key_exchange_separately() {
    for (kind, algorithm, expected) in [
        (
            russh::AlgorithmKind::Kex,
            "gss-group14-sha256-test",
            io::ErrorKind::Unsupported,
        ),
        (
            russh::AlgorithmKind::Kex,
            "diffie-hellman-group1-sha1",
            io::ErrorKind::Other,
        ),
        (
            russh::AlgorithmKind::Key,
            "gss-not-a-hostkey",
            io::ErrorKind::Other,
        ),
    ] {
        let error = transport_error(russh::Error::NoCommonAlgo {
            kind,
            ours: vec!["curve25519-sha256".into()],
            theirs: vec![algorithm.into()],
        });
        assert_eq!(error.kind(), expected);
    }
}

fn read_key(file: &Path, password: &str) -> io::Result<russh::keys::PrivateKey> {
    const MAX_KEY: u64 = 1024 * 1024;
    let mut key = Zeroizing::new(String::new());
    std::fs::File::open(file)?
        .take(MAX_KEY + 1)
        .read_to_string(&mut key)?;
    if key.len() as u64 > MAX_KEY {
        return Err(invalid_input("Die private SSH-Schlüsseldatei ist zu groß."));
    }
    decode_secret_key(&key, (!password.is_empty()).then_some(password)).map_err(io::Error::other)
}

async fn authenticate(
    session: &mut client::Handle<PinnedHost>,
    user: &str,
    password: &str,
    options: &Options,
    endpoint: &str,
    challenge: Option<&ChallengeHandler>,
    cancel: &AtomicBool,
) -> io::Result<()> {
    if options.interactive
        && options.agent.is_none()
        && options.private_key.is_none()
        && options.kerberos_spn.is_none()
        && password.is_empty()
    {
        return authenticate_interactive(
            session,
            user,
            endpoint,
            challenge,
            options.timeout,
            cancel,
        )
        .await;
    }
    let result = bounded(cancel, options.timeout, async {
        Ok(if let Some(principal) = &options.kerberos_spn {
            #[cfg(windows)]
            {
                let mut result = None;
                if options.kerberos_kex && !options.kerberos_delegate {
                    let auth = session.authenticate_gssapi_keyex(user).await.map_err(io::Error::other)?;
                    if !matches!(&auth, AuthResult::Failure { partial_success: false, remaining_methods }
                        if remaining_methods.contains(&russh::MethodKind::GssapiWithMic))
                    {
                        result = Some(auth);
                    }
                }
                match result {
                    Some(auth) => Some(auth),
                    None => Some(super::kerberos::authenticate(
                        session,
                        user,
                        principal,
                        options.kerberos_delegate,
                    )
                    .await?),
                }
            }
            #[cfg(not(windows))]
            {
                let _ = principal;
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "Kerberos benötigt Windows SSPI.",
                ));
            }
        } else if let Some(mode) = options.agent {
            authenticate_agent(session, user, mode).await?
        } else if let Some(file) = &options.private_key {
            let key = read_key(file, password)?;
            let hash = if key.algorithm().is_rsa() {
                session
                    .best_supported_rsa_hash()
                    .await
                    .map_err(io::Error::other)?
                    .flatten()
                    .or(Some(HashAlg::Sha256))
            } else {
                None
            };
            Some(
                session
                    .authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
                    .await
                    .map_err(io::Error::other)?,
            )
        } else {
            Some(
                session
                    .authenticate_password(user, password)
                    .await
                    .map_err(io::Error::other)?,
            )
        })
    })
    .await?;
    match result {
        Some(AuthResult::Success) => Ok(()),
        Some(AuthResult::Failure {
            partial_success: true,
            ..
        }) if options.interactive => {
            authenticate_interactive(session, user, endpoint, challenge, options.timeout, cancel)
                .await
        }
        _ => Err(authentication_rejected()),
    }
}

fn authentication_rejected() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "Der SSH-Server hat die Anmeldung abgelehnt.",
    )
}

async fn authenticate_interactive(
    session: &mut client::Handle<PinnedHost>,
    user: &str,
    endpoint: &str,
    challenge: Option<&ChallengeHandler>,
    timeout: u64,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let challenge = challenge
        .ok_or_else(|| invalid_input("Für Keyboard-Interactive fehlt ein Eingabedialog."))?;
    let mut reply = bounded(cancel, timeout, async {
        session
            .authenticate_keyboard_interactive_start(user, None)
            .await
            .map_err(io::Error::other)
    })
    .await?;
    for _ in 0..16 {
        match reply {
            KeyboardInteractiveAuthResponse::Success => return Ok(()),
            KeyboardInteractiveAuthResponse::Failure { .. } => {
                return Err(authentication_rejected());
            }
            KeyboardInteractiveAuthResponse::InfoRequest {
                name,
                instructions,
                prompts,
            } => {
                if prompts.len() > 16 {
                    return Err(invalid_input(
                        "Der SSH-Server hat zu viele Eingabefelder angefordert.",
                    ));
                }
                let clean = |text: &str| -> String {
                    text.chars()
                        .filter(|ch| {
                            (!ch.is_control() || *ch == '\n')
                                && !matches!(*ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
                        })
                        .take(1024)
                        .collect()
                };
                let instructions = format!("{}\n{}", clean(&name), clean(&instructions));
                let mut answers = Vec::with_capacity(prompts.len());
                for item in prompts {
                    let (response, value) = tokio::sync::oneshot::channel();
                    challenge(SshChallenge {
                        endpoint: format!("{user}@{endpoint}"),
                        instructions: instructions.clone(),
                        prompt: clean(&item.prompt),
                        echo: item.echo,
                        response,
                    })?;
                    answers.push(
                        bounded(cancel, 180, async {
                            value
                                .await
                                .map_err(|_| io::Error::from(io::ErrorKind::Interrupted))
                        })
                        .await?,
                    );
                }
                reply = bounded(cancel, timeout, async {
                    session
                        .authenticate_keyboard_interactive_respond(
                            answers
                                .iter_mut()
                                .map(|answer| std::mem::take(&mut **answer))
                                .collect(),
                        )
                        .await
                        .map_err(io::Error::other)
                })
                .await?;
            }
        }
    }
    if matches!(reply, KeyboardInteractiveAuthResponse::Success) {
        Ok(())
    } else {
        Err(authentication_rejected())
    }
}

#[cfg(any(windows, unix))]
async fn authenticate_agent(
    session: &mut client::Handle<PinnedHost>,
    user: &str,
    mode: SshAgent,
) -> io::Result<Option<AuthResult>> {
    #[cfg(windows)]
    let mut agent = match mode {
        SshAgent::OpenSsh => {
            let socket = std::env::var_os("SSH_AUTH_SOCK")
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| r"\\.\pipe\openssh-ssh-agent".into());
            // The agent may use any local named pipe, but never a network pipe.
            if !socket
                .to_string_lossy()
                .to_ascii_lowercase()
                .starts_with(r"\\.\pipe\")
            {
                return Err(invalid_input(
                    "Der OpenSSH-Agent benötigt eine lokale Windows-Named-Pipe.",
                ));
            }
            AgentClient::connect_named_pipe(socket)
                .await
                .map(|agent| agent.dynamic())
        }
        SshAgent::Pageant => AgentClient::connect_pageant()
            .await
            .map(|agent| agent.dynamic()),
    }
    .map_err(|error| io::Error::other(format!("SSH-Agent nicht erreichbar: {error}")))?;
    #[cfg(unix)]
    let mut agent = match mode {
        SshAgent::OpenSsh => AgentClient::connect_env().await.map_err(io::Error::other)?,
        SshAgent::Pageant => {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Pageant benötigt Windows.",
            ));
        }
    };

    if mode == SshAgent::OpenSsh
        && let Some(binding) = session.agent_session_binding()
    {
        match agent.bind_session(binding, false).await {
            Ok(()) | Err(russh::keys::Error::AgentFailure) => {}
            Err(error) => return Err(io::Error::other(error)),
        }
    }
    let identities = agent.request_identities().await.map_err(io::Error::other)?;
    if identities.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "Der SSH-Agent hält keine Schlüssel bereit.",
        ));
    }
    let hash = session
        .best_supported_rsa_hash()
        .await
        .map_err(io::Error::other)?
        .flatten()
        .or(Some(HashAlg::Sha256));
    for identity in identities {
        let AgentIdentity::PublicKey { key, .. } = identity else {
            continue;
        };
        let hash = hash.filter(|_| key.algorithm().is_rsa());
        let result = session
            .authenticate_publickey_with(user, key, hash, &mut agent)
            .await
            .map_err(io::Error::other)?;
        if matches!(
            result,
            AuthResult::Success
                | AuthResult::Failure {
                    partial_success: true,
                    ..
                }
        ) {
            return Ok(Some(result));
        }
    }
    Ok(None)
}

#[cfg(not(any(windows, unix)))]
async fn authenticate_agent(
    _: &mut client::Handle<PinnedHost>,
    _: &str,
    _: SshAgent,
) -> io::Result<Option<AuthResult>> {
    Err(io::ErrorKind::Unsupported.into())
}

async fn tunnel(
    jump: &client::Handle<PinnedHost>,
    connection: &Connection,
) -> io::Result<impl AsyncRead + AsyncWrite + Unpin + Send + 'static + use<>> {
    let (host, port) = endpoint(connection)?;
    let channel = jump
        .channel_open_direct_tcpip(host, port.into(), "127.0.0.1", 0)
        .await
        .map_err(io::Error::other)?;
    Ok(channel.into_stream())
}

pub(super) async fn bounded<T>(
    cancel: &AtomicBool,
    timeout: u64,
    work: impl Future<Output = io::Result<T>>,
) -> io::Result<T> {
    tokio::select! {
        result = tokio::time::timeout(Duration::from_secs(timeout), work) => {
            result.map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Zeitlimit für SSH-Verbindungsaufbau erreicht."))?
        }
        interrupted = async {
            loop {
                check_cancel(cancel)?;
                tokio::time::sleep(Duration::from_millis(75)).await;
            }
        } => interrupted,
    }
}

impl Session {
    pub(super) async fn open_ssh(&self, cancel: &AtomicBool) -> io::Result<SshConnection> {
        let settings = self
            .connection
            .ssh
            .as_ref()
            .ok_or_else(|| invalid_input("SSH-Profil fehlt."))?;
        let mut jumps = Vec::new();
        let mut local_proxies = Vec::new();
        for (connection, secret) in &settings.jumps {
            let jump = establish(
                connection,
                secret,
                jumps.last(),
                settings.challenge.as_deref(),
                cancel,
            )
            .await
            .map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("SSH-Sprungserver {}: {error}", connection.url),
                )
            })?;
            jumps.push(jump.session);
            local_proxies.extend(jump.proxy);
        }
        let target = establish(
            &self.connection,
            &self.password,
            jumps.last(),
            settings.challenge.as_deref(),
            cancel,
        )
        .await?;
        local_proxies.extend(target.proxy);
        Ok(SshConnection {
            target: target.session,
            jumps,
            fingerprint: target.fingerprint,
            _local_proxies: local_proxies,
        })
    }

    pub(super) fn open_sftp(&self, cancel: &AtomicBool) -> io::Result<SftpConnection> {
        let timeout = self
            .connection
            .ssh
            .as_ref()
            .ok_or_else(|| invalid_input("SSH-Profil fehlt."))?
            .timeout;
        runtime()?.block_on(async {
            let ssh = self.open_ssh(cancel).await?;
            bounded(cancel, timeout, async {
                let mut channel = ssh
                    .target
                    .channel_open_session()
                    .await
                    .map_err(io::Error::other)?;
                channel
                    .request_subsystem(true, "sftp")
                    .await
                    .map_err(io::Error::other)?;
                loop {
                    match channel.wait().await {
                        Some(ChannelMsg::Success) => break,
                        Some(ChannelMsg::Failure | ChannelMsg::Eof | ChannelMsg::Close) | None => {
                            return Err(io::Error::new(
                                io::ErrorKind::Unsupported,
                                "Der SSH-Server stellt kein SFTP-Subsystem bereit.",
                            ));
                        }
                        _ => (),
                    }
                }
                let wire = Wire::start(channel.into_stream(), Duration::from_secs(timeout)).await?;
                Ok(SftpConnection { wire, ssh })
            })
            .await
        })
    }
}
