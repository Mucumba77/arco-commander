use hmac::{Hmac, Mac};
use md5::Md5;
use std::io;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use zeroize::Zeroizing;

const VERSION: u8 = 1;
const STATUS: u8 = 0;
const USER: u8 = 2;
const CHALLENGE: u8 = 3;
const RESPONSE: u8 = 4;
const ALGORITHMS: u8 = 0x11;
const HMAC_MD5: u8 = 0x85;

pub(super) async fn authenticate(
    stream: &mut TcpStream,
    user: &str,
    password: &str,
) -> io::Result<()> {
    let name_length = u8::try_from(user.len())
        .map_err(|_| super::invalid_input("Der Proxy-Benutzername ist zu lang."))?;
    // WinSCP and PuTTY send the identity with the algorithm offer.
    let mut hello = vec![VERSION, 2, ALGORITHMS, 1, HMAC_MD5, USER, name_length];
    hello.extend_from_slice(user.as_bytes());
    stream.write_all(&hello).await?;

    let mut proved = false;
    for _ in 0..8 {
        let message = read_message(stream).await?;
        if let Some(status) = message.status {
            if status == 0 && proved && message.challenge.is_none() {
                return Ok(());
            }
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Die SOCKS5-CHAP-Anmeldung wurde abgelehnt.",
            ));
        }
        if let Some(challenge) = message.challenge {
            if proved {
                return Err(invalid_reply());
            }
            let mut mac = <Hmac<Md5> as Mac>::new_from_slice(password.as_bytes())
                .expect("HMAC keys may have any length");
            mac.update(&challenge);
            let mut packet = Zeroizing::new([0u8; 20]);
            packet[..4].copy_from_slice(&[VERSION, 1, RESPONSE, 16]);
            packet[4..].copy_from_slice(&mac.finalize().into_bytes());
            stream.write_all(packet.as_slice()).await?;
            proved = true;
        }
    }
    Err(invalid_reply())
}

#[derive(Default)]
struct Message {
    status: Option<u8>,
    challenge: Option<Vec<u8>>,
}

async fn read_message(stream: &mut TcpStream) -> io::Result<Message> {
    let mut header = [0; 2];
    stream.read_exact(&mut header).await?;
    if header[0] != VERSION || header[1] == 0 {
        return Err(invalid_reply());
    }
    let mut message = Message::default();
    let mut value = [0u8; u8::MAX as usize];
    for _ in 0..header[1] {
        let mut field = [0; 2];
        stream.read_exact(&mut field).await?;
        let bytes = &mut value[..usize::from(field[1])];
        stream.read_exact(bytes).await?;
        match field[0] {
            STATUS if bytes.len() == 1 && message.status.is_none() => {
                message.status = Some(bytes[0]);
            }
            CHALLENGE if !bytes.is_empty() && message.challenge.is_none() => {
                message.challenge = Some(bytes.to_vec());
            }
            ALGORITHMS if bytes != [HMAC_MD5] => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "Der SOCKS5-Proxy hat einen anderen CHAP-Algorithmus gewählt.",
                ));
            }
            STATUS | CHALLENGE => return Err(invalid_reply()),
            _ => (),
        }
    }
    Ok(message)
}

fn invalid_reply() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Die SOCKS5-CHAP-Antwort ist ungültig.",
    )
}
