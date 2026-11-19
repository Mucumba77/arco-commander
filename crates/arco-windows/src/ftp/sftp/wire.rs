//! The SFTP v3 requests used by the file panes. Handles are opaque byte strings.

use super::runtime;
use std::io;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const MAX_PACKET: usize = 256 * 1024;
const STATUS: u8 = 101;
const HANDLE: u8 = 102;
const DATA: u8 = 103;
const NAME: u8 = 104;
const ATTRS: u8 = 105;

pub(super) trait Channel: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Channel for T {}

pub(super) struct Wire {
    stream: Option<Box<dyn Channel>>,
    runtime: tokio::runtime::Handle,
    timeout: Duration,
    id: u32,
    pub(super) usable: bool,
}

impl Drop for Wire {
    fn drop(&mut self) {
        // Russh closes its channel by spawning a task in the active runtime.
        let _entered = self.runtime.enter();
        drop(self.stream.take());
    }
}

#[derive(Default)]
pub(super) struct Attrs {
    pub size: Option<u64>,
    pub permissions: Option<u32>,
    pub access: Option<u32>,
    pub modified: Option<u32>,
}

impl Attrs {
    pub(super) fn directory(&self) -> Option<bool> {
        match self.permissions? & 0o170000 {
            0o040000 => Some(true),
            0o100000 => Some(false),
            _ => None,
        }
    }
}

fn malformed() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Ungültige Antwort des SFTP-Servers.",
    )
}

fn push_string(packet: &mut Vec<u8>, value: &[u8]) {
    packet.extend_from_slice(&(value.len() as u32).to_be_bytes());
    packet.extend_from_slice(value);
}

struct Packet<'a>(&'a [u8]);

impl<'a> Packet<'a> {
    fn bytes(&mut self, count: usize) -> io::Result<&'a [u8]> {
        let (value, remainder) = self.0.split_at_checked(count).ok_or_else(malformed)?;
        self.0 = remainder;
        Ok(value)
    }

    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_be_bytes(self.bytes(4)?.try_into().unwrap()))
    }

    fn string(&mut self) -> io::Result<&'a [u8]> {
        let len = self.u32()? as usize;
        self.bytes(len)
    }

    fn attrs(&mut self) -> io::Result<Attrs> {
        let flags = self.u32()?;
        if flags & !0x8000_000f != 0 {
            return Err(malformed());
        }
        let size = if flags & 1 != 0 {
            Some(u64::from_be_bytes(self.bytes(8)?.try_into().unwrap()))
        } else {
            None
        };
        if flags & 2 != 0 {
            self.bytes(8)?;
        }
        let permissions = (flags & 4 != 0).then(|| self.u32()).transpose()?;
        let (access, modified) = if flags & 8 != 0 {
            (Some(self.u32()?), Some(self.u32()?))
        } else {
            (None, None)
        };
        if flags & 0x8000_0000 != 0 {
            let count = self.u32()? as usize;
            if count > self.0.len() / 8 {
                return Err(malformed());
            }
            for _ in 0..count {
                self.string()?;
                self.string()?;
            }
        }
        Ok(Attrs {
            size,
            permissions,
            access,
            modified,
        })
    }
}

fn put_attrs(packet: &mut Vec<u8>, attr: Attrs) {
    let timestamps = attr.access.zip(attr.modified);
    let flags = u32::from(attr.permissions.is_some()) * 4 + u32::from(timestamps.is_some()) * 8;
    packet.extend_from_slice(&flags.to_be_bytes());
    if let Some(permissions) = attr.permissions {
        packet.extend_from_slice(&permissions.to_be_bytes());
    }
    if let Some((access, modified)) = timestamps {
        packet.extend_from_slice(&access.to_be_bytes());
        packet.extend_from_slice(&modified.to_be_bytes());
    }
}

async fn exchange(stream: &mut dyn Channel, packet: &[u8]) -> io::Result<Vec<u8>> {
    if packet.len() > MAX_PACKET {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Die SFTP-Anfrage ist zu groß.",
        ));
    }
    stream.write_u32(packet.len() as u32).await?;
    stream.write_all(packet).await?;
    let length = stream.read_u32().await? as usize;
    if !(5..=MAX_PACKET).contains(&length) {
        return Err(malformed());
    }
    let mut reply = vec![0; length];
    stream.read_exact(&mut reply).await?;
    Ok(reply)
}

impl Wire {
    pub(super) async fn start(
        stream: impl Channel + 'static,
        timeout: Duration,
    ) -> io::Result<Self> {
        let mut stream: Box<dyn Channel> = Box::new(stream);
        let reply = exchange(stream.as_mut(), &[1, 0, 0, 0, 3]).await?;
        if reply[..5] != [2, 0, 0, 0, 3] {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Der Server unterstützt SFTP Version 3 nicht.",
            ));
        }
        Ok(Self {
            stream: Some(stream),
            runtime: tokio::runtime::Handle::current(),
            timeout,
            id: 0,
            usable: true,
        })
    }

    fn request<T>(
        &mut self,
        kind: u8,
        expected: u8,
        encode: impl FnOnce(&mut Vec<u8>),
        decode: impl FnOnce(&mut Packet<'_>) -> io::Result<T>,
    ) -> io::Result<Option<T>> {
        if !self.usable {
            return Err(io::ErrorKind::ConnectionAborted.into());
        }
        self.id = self.id.wrapping_add(1);
        let id = self.id.to_be_bytes();
        let mut packet = vec![kind];
        packet.extend_from_slice(&id);
        encode(&mut packet);
        let result = (|| {
            let stream = self.stream.as_deref_mut().expect("active SFTP channel");
            let reply = runtime()?.block_on(async {
                tokio::time::timeout(self.timeout, exchange(stream, &packet))
                    .await
                    .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))?
            })?;
            if reply[1..5] != id {
                return Err(malformed());
            }
            let mut response = Packet(&reply[5..]);
            if reply[0] == STATUS {
                let status = response.u32()?;
                if status == 0 && expected == STATUS {
                    return decode(&mut response).map(Some);
                }
                if status == 1 && matches!(expected, DATA | NAME) {
                    return Ok(None);
                }
                let kind = match status {
                    0 | 1 | 5 => io::ErrorKind::InvalidData,
                    2 | 10 => io::ErrorKind::NotFound,
                    3 => io::ErrorKind::PermissionDenied,
                    6 | 7 => io::ErrorKind::ConnectionAborted,
                    8 => io::ErrorKind::Unsupported,
                    11 => io::ErrorKind::AlreadyExists,
                    _ => io::ErrorKind::Other,
                };
                let message = response.string().map(String::from_utf8_lossy)?;
                return Err(io::Error::new(
                    kind,
                    format!("SFTP-Status {status}: {message}"),
                ));
            }
            if reply[0] != expected || expected == STATUS {
                return Err(malformed());
            }
            decode(&mut response).map(Some)
        })();
        if result.as_ref().is_err_and(|error| {
            matches!(
                error.kind(),
                io::ErrorKind::InvalidData
                    | io::ErrorKind::ConnectionAborted
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::BrokenPipe
                    | io::ErrorKind::TimedOut
                    | io::ErrorKind::UnexpectedEof
            )
        }) {
            self.usable = false;
        }
        result
    }

    fn status(&mut self, kind: u8, encode: impl FnOnce(&mut Vec<u8>)) -> io::Result<()> {
        self.request(kind, STATUS, encode, |_| Ok(()))?
            .ok_or_else(malformed)
    }

    pub(super) fn open(&mut self, path: &str, exclusive: bool) -> io::Result<Vec<u8>> {
        self.open_flags(path, if exclusive { 0x2 | 0x8 | 0x20 } else { 0x1 })
    }

    pub(super) fn reopen_for_write(&mut self, path: &str) -> io::Result<Vec<u8>> {
        self.open_flags(path, 0x2)
    }

    fn open_flags(&mut self, path: &str, flags: u32) -> io::Result<Vec<u8>> {
        self.request(
            3,
            HANDLE,
            |packet| {
                push_string(packet, path.as_bytes());
                packet.extend_from_slice(&u32::to_be_bytes(flags));
                put_attrs(
                    packet,
                    Attrs {
                        permissions: (flags & 8 != 0).then_some(0o600),
                        ..Default::default()
                    },
                );
            },
            |packet| Ok(packet.string()?.to_vec()),
        )?
        .ok_or_else(malformed)
    }

    pub(super) fn open_dir(&mut self, path: &str) -> io::Result<Vec<u8>> {
        self.request(
            11,
            HANDLE,
            |packet| push_string(packet, path.as_bytes()),
            |packet| Ok(packet.string()?.to_vec()),
        )?
        .ok_or_else(malformed)
    }

    pub(super) fn close(&mut self, handle: &[u8]) -> io::Result<()> {
        self.status(4, |packet| push_string(packet, handle))
    }

    pub(super) fn read(
        &mut self,
        handle: &[u8],
        offset: u64,
        len: u32,
    ) -> io::Result<Option<Vec<u8>>> {
        self.request(
            5,
            DATA,
            |packet| {
                push_string(packet, handle);
                packet.extend_from_slice(&offset.to_be_bytes());
                packet.extend_from_slice(&len.to_be_bytes());
            },
            |packet| {
                let data = packet.string()?;
                if data.is_empty() || data.len() > len as usize {
                    return Err(malformed());
                }
                Ok(data.to_vec())
            },
        )
    }

    pub(super) fn write(&mut self, handle: &[u8], offset: u64, data: &[u8]) -> io::Result<()> {
        self.status(6, |packet| {
            push_string(packet, handle);
            packet.extend_from_slice(&offset.to_be_bytes());
            push_string(packet, data);
        })
    }

    pub(super) fn read_dir(&mut self, handle: &[u8]) -> io::Result<Option<Vec<(String, Attrs)>>> {
        self.request(
            12,
            NAME,
            |packet| push_string(packet, handle),
            |packet| {
                let count = packet.u32()? as usize;
                if count == 0 || count > packet.0.len() / 12 {
                    return Err(malformed());
                }
                let mut entries = Vec::with_capacity(count);
                for _ in 0..count {
                    let name = str::from_utf8(packet.string()?)
                        .map_err(|_| malformed())?
                        .to_owned();
                    packet.string()?;
                    entries.push((name, packet.attrs()?));
                }
                Ok(entries)
            },
        )
    }

    pub(super) fn lstat(&mut self, path: &str) -> io::Result<Attrs> {
        self.request(
            7,
            ATTRS,
            |packet| push_string(packet, path.as_bytes()),
            |packet| packet.attrs(),
        )?
        .ok_or_else(malformed)
    }

    pub(super) fn mkdir(&mut self, path: &str) -> io::Result<()> {
        self.status(14, |packet| {
            push_string(packet, path.as_bytes());
            put_attrs(
                packet,
                Attrs {
                    permissions: Some(0o755),
                    ..Default::default()
                },
            );
        })
    }

    pub(super) fn remove(&mut self, path: &str, directory: bool) -> io::Result<()> {
        self.status(if directory { 15 } else { 13 }, |packet| {
            push_string(packet, path.as_bytes())
        })
    }

    pub(super) fn rename(&mut self, from: &str, to: &str) -> io::Result<()> {
        self.status(18, |packet| {
            push_string(packet, from.as_bytes());
            push_string(packet, to.as_bytes());
        })
    }

    pub(super) fn set_attrs(&mut self, path: &str, attrs: Attrs) -> io::Result<()> {
        self.status(9, |packet| {
            push_string(packet, path.as_bytes());
            put_attrs(packet, attrs);
        })
    }
}
