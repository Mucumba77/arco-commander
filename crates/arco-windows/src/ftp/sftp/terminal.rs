use super::{path, runtime, scp::shell_arg, transport::bounded};
use crate::ftp::{Session, Url, invalid_input};
use russh::ChannelMsg;
use std::fs::{File, OpenOptions};
use std::io::{self, IsTerminal, Read, Write};
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::ptr::null_mut;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use windows_sys::Win32::System::Console::{
    CONSOLE_SCREEN_BUFFER_INFO, ENABLE_EXTENDED_FLAGS, ENABLE_PROCESSED_OUTPUT,
    ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, ENABLE_WRAP_AT_EOL_OUTPUT,
    GetConsoleScreenBufferInfo, ReadConsoleW, SetConsoleMode, SetConsoleTitleW, WriteConsoleW,
};
use windows_sys::Win32::System::Threading::CREATE_NEW_CONSOLE;

const FRAME_LIMIT: usize = 16 * 1024;
const INPUT: u8 = 1;
const SIZE: u8 = 2;

enum Frame {
    Input(Vec<u8>),
    Size(u16, u16),
}

impl Frame {
    fn read(input: &mut impl Read) -> io::Result<Option<Self>> {
        let mut tag = [0];
        if input.read(&mut tag)? == 0 {
            return Ok(None);
        }
        let mut length = [0; 2];
        input.read_exact(&mut length)?;
        let length = usize::from(u16::from_le_bytes(length));
        match (tag[0], length) {
            (INPUT, 1..=FRAME_LIMIT) => {
                let mut data = vec![0; length];
                input.read_exact(&mut data)?;
                Ok(Some(Self::Input(data)))
            }
            (SIZE, 4) => {
                let mut width = [0; 2];
                let mut height = [0; 2];
                input.read_exact(&mut width)?;
                input.read_exact(&mut height)?;
                Ok(Some(Self::Size(
                    u16::from_le_bytes(width).max(1),
                    u16::from_le_bytes(height).max(1),
                )))
            }
            _ => Err(invalid_input("Ungültige SSH-Terminalnachricht.")),
        }
    }
}

fn send_frame(output: &Mutex<io::Stdout>, tag: u8, data: &[u8]) -> io::Result<()> {
    let mut output = output
        .lock()
        .map_err(|_| io::Error::other("SSH-Terminalausgabe wurde unterbrochen."))?;
    output.write_all(&[tag])?;
    output.write_all(&(data.len() as u16).to_le_bytes())?;
    output.write_all(data)?;
    output.flush()
}

fn send_size(output: &Mutex<io::Stdout>, (width, height): (u16, u16)) -> io::Result<()> {
    let mut bytes = [0; 4];
    bytes[..2].copy_from_slice(&width.to_le_bytes());
    bytes[2..].copy_from_slice(&height.to_le_bytes());
    send_frame(output, SIZE, &bytes)
}

struct ConsoleChild(Child);

impl Drop for ConsoleChild {
    fn drop(&mut self) {
        // Closing stdin signals the end of the SSH output. The private
        // console normally exits itself. Terminate a wedged helper promptly.
        self.0.stdin.take();
        for _ in 0..20 {
            match self.0.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(_) => break,
            }
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Session {
    pub fn open_ssh_terminal(
        &self,
        directory: Option<&Url>,
        helper: &Path,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let timeout = self
            .connection
            .ssh
            .as_ref()
            .ok_or_else(|| invalid_input("Ein SSH-Terminal benötigt ein SFTP- oder SCP-Profil."))?
            .timeout;
        let command = directory
            .map(|directory| {
                self.validate(directory)?;
                terminal_command(directory)
            })
            .transpose()?;
        runtime()?.block_on(async {
            // Keep the file panel responsive and maintain a separate connection
            // with the same pin, authentication, jump hosts and proxy.
            let ssh = self.open_ssh(cancel).await?;
            let mut child = ConsoleChild(Command::new(helper)
                .arg("--ssh-console-bridge")
                .creation_flags(CREATE_NEW_CONSOLE)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()?);
            let mut input = child.0.stdout.take()
                .ok_or_else(|| io::Error::other("SSH-Terminal hat keinen Ausgabekanal."))?;
            let (send, mut receive) = tokio::sync::mpsc::channel(16);
            std::thread::Builder::new().name("arco-ssh-input".into()).spawn(move || {
                loop {
                    match Frame::read(&mut input) {
                        Ok(Some(frame)) => if send.blocking_send(Ok(frame)).is_err() { break },
                        Ok(None) => break,
                        Err(error) => {
                            let _ = send.blocking_send(Err(error));
                            break;
                        }
                    }
                }
            })?;
            let first = bounded(cancel, timeout, async {
                receive.recv().await.transpose()?.ok_or_else(|| io::Error::other("Die Windows-Konsole konnte nicht geöffnet werden."))
            }).await?;
            let Frame::Size(width, height) = first else {
                return Err(invalid_input("Die Größe der Windows-Konsole fehlt."));
            };
            let mut channel = ssh.terminal(command.as_deref(), width.into(), height.into(), timeout, cancel).await?;
            let output = child.0.stdin.as_mut()
                .ok_or_else(|| io::Error::other("SSH-Terminal hat keinen Eingabekanal."))?;
            let mut status = 0;
            loop {
                tokio::select! {
                    event = receive.recv() => match event.transpose()? {
                        Some(Frame::Input(data)) => channel.data_bytes(data).await.map_err(io::Error::other)?,
                        Some(Frame::Size(w, h)) => channel.window_change(w.into(), h.into(), 0, 0).await.map_err(io::Error::other)?,
                        None => { let _ = channel.close().await; return Ok(()); }
                    },
                    packet = channel.wait() => match packet {
                        Some(ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. }) => {
                            if let Err(error) = output.write_all(&data) {
                                if error.kind() == io::ErrorKind::BrokenPipe {
                                    return Ok(());
                                }
                                return Err(error);
                            }
                        }
                        Some(ChannelMsg::Failure) => return Err(io::Error::other("Der SSH-Server hat die Terminal-Shell abgelehnt.")),
                        Some(ChannelMsg::ExitStatus { exit_status }) => status = exit_status,
                        Some(ChannelMsg::ExitSignal { .. }) => status = 1,
                        Some(ChannelMsg::Close) | None => break,
                        _ => (),
                    },
                    () = tokio::time::sleep(Duration::from_millis(250)) => {
                        crate::ftp::check_cancel(cancel)?;
                    },
                }
            }
            if status != 0 {
                return Err(io::Error::other(format!("Die SSH-Shell wurde mit Status {status} beendet.")));
            }
            Ok(())
        })
    }
}

fn terminal_command(directory: &Url) -> io::Result<String> {
    let folder = shell_arg(&path(directory)?);
    let start = format!("cd {folder} && exec \"${{SHELL:-/bin/sh}}\" -i");
    Ok(format!("exec /bin/sh -c {}", shell_arg(&start)))
}

pub fn console_bridge() -> io::Result<()> {
    if io::stdin().is_terminal() || io::stdout().is_terminal() {
        return Err(invalid_input("Die SSH-Konsole wird aus Arco geöffnet."));
    }
    let keyboard = OpenOptions::new().read(true).write(true).open("CONIN$")?;
    let screen = OpenOptions::new().read(true).write(true).open("CONOUT$")?;
    // SAFETY: These handles belong to the dedicated console, and stay open
    // until its helper process exits.
    if unsafe {
        SetConsoleMode(
            keyboard.as_raw_handle(),
            ENABLE_EXTENDED_FLAGS | ENABLE_VIRTUAL_TERMINAL_INPUT,
        ) == 0
            || SetConsoleMode(
                screen.as_raw_handle(),
                ENABLE_PROCESSED_OUTPUT
                    | ENABLE_WRAP_AT_EOL_OUTPUT
                    | ENABLE_VIRTUAL_TERMINAL_PROCESSING,
            ) == 0
    } {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: The title is a static NUL-terminated UTF-16 string.
    unsafe {
        SetConsoleTitleW(windows_sys::w!("Arco Commander · SSH"));
    }
    let pipe = std::sync::Arc::new(Mutex::new(io::stdout()));
    let mut last_size = console_size(&screen)?;
    send_size(&pipe, last_size)?;
    let measure = screen.try_clone()?;
    let updates = pipe.clone();
    std::thread::Builder::new()
        .name("arco-console-size".into())
        .spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(200));
                let Ok(size) = console_size(&measure) else {
                    return;
                };
                if size != last_size {
                    if send_size(&updates, size).is_err() {
                        return;
                    }
                    last_size = size;
                }
            }
        })?;
    std::thread::Builder::new()
        .name("arco-console-keys".into())
        .spawn(move || {
            let _ = console_input(&keyboard, &pipe);
        })?;
    console_output(&screen, &mut io::stdin().lock())
}

fn console_size(screen: &File) -> io::Result<(u16, u16)> {
    let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
    // SAFETY: The live screen handle and layout-matching output buffer are valid.
    if unsafe { GetConsoleScreenBufferInfo(screen.as_raw_handle(), &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let rect = info.srWindow;
    Ok((
        (rect.Right - rect.Left + 1).max(1) as u16,
        (rect.Bottom - rect.Top + 1).max(1) as u16,
    ))
}

fn console_input(keyboard: &File, pipe: &Mutex<io::Stdout>) -> io::Result<()> {
    let mut units = [0u16; 1025];
    let mut pending = 0;
    loop {
        let mut read = 0;
        let target = &mut units[pending..];
        // SAFETY: The target is writable and valid for the requested character count.
        if unsafe {
            ReadConsoleW(
                keyboard.as_raw_handle(),
                target.as_mut_ptr().cast(),
                target.len() as u32,
                &mut read,
                null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let count = read as usize + pending;
        pending = usize::from(count > 0 && (0xD800..=0xDBFF).contains(&units[count - 1]));
        let text = String::from_utf16_lossy(&units[..count - pending]);
        if pending != 0 {
            units[0] = units[count - 1];
        }
        if !text.is_empty() {
            send_frame(pipe, INPUT, text.as_bytes())?;
        }
    }
}

fn console_output(screen: &File, input: &mut impl Read) -> io::Result<()> {
    let mut buffer = [0u8; FRAME_LIMIT + 4];
    let mut pending = 0;
    loop {
        let read = input.read(&mut buffer[pending..])?;
        if read == 0 {
            if pending != 0 {
                write_console(screen, "\u{fffd}")?;
            }
            return Ok(());
        }
        let mut bytes = &buffer[..pending + read];
        pending = 0;
        while !bytes.is_empty() {
            match std::str::from_utf8(bytes) {
                Ok(text) => {
                    write_console(screen, text)?;
                    break;
                }
                Err(error) => {
                    let start = error.valid_up_to();
                    write_console(screen, &String::from_utf8_lossy(&bytes[..start]))?;
                    if let Some(length) = error.error_len() {
                        write_console(screen, "\u{fffd}")?;
                        bytes = &bytes[start + length..];
                    } else {
                        pending = bytes.len() - start;
                        let offset = bytes.as_ptr() as usize - buffer.as_ptr() as usize;
                        buffer.copy_within(offset + start..offset + start + pending, 0);
                        break;
                    }
                }
            }
        }
    }
}

fn write_console(screen: &File, text: &str) -> io::Result<()> {
    let units: Vec<_> = text.encode_utf16().collect();
    let mut tail = units.as_slice();
    while !tail.is_empty() {
        let mut written = 0;
        // SAFETY: The complete UTF-16 input and output count live for the call.
        if unsafe {
            WriteConsoleW(
                screen.as_raw_handle(),
                tail.as_ptr(),
                tail.len() as u32,
                &mut written,
                null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        if written == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        tail = &tail[written as usize..];
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ftp::{Connection, Security};

    #[test]
    fn terminal_frames_distinguish_raw_input_and_resize() {
        let mut data = &b"\x02\x04\x00\x64\x00\x20\x00\x01\x03\x00\x1b[A"[..];
        assert!(matches!(
            Frame::read(&mut data).unwrap(),
            Some(Frame::Size(100, 32))
        ));
        assert!(
            matches!(Frame::read(&mut data).unwrap(), Some(Frame::Input(chars)) if chars == b"\x1b[A")
        );
        assert!(Frame::read(&mut data).unwrap().is_none());
        assert!(Frame::read(&mut &b"\x01\xff\xff"[..]).is_err());
    }

    #[test]
    #[ignore = "requires the SSH terminal peer in scripts/test-sftp-reference.py"]
    fn reference_sftp_terminal_pty() {
        let session = Connection::new(
            &std::env::var("ARCO_SFTP_TEST_URL").unwrap(),
            "arco",
            Security::Sftp,
        )
        .unwrap()
        .with_ssh(&std::env::var("ARCO_SFTP_TEST_FINGERPRINT").unwrap(), None)
        .unwrap()
        .authenticate("fixture-only")
        .unwrap();
        let directory = session.directory("/Büro's Ablage").unwrap();
        let command = terminal_command(&directory).unwrap();
        let cancel = AtomicBool::new(false);
        runtime().unwrap().block_on(async {
            let ssh = session.open_ssh(&cancel).await.unwrap();
            for command in [Some(command.as_str()), None] {
                let mut channel = ssh.terminal(command, 80, 24, 5, &cancel).await.unwrap();
                let response = tokio::time::timeout(Duration::from_secs(5), channel.wait())
                    .await
                    .unwrap();
                assert!(matches!(response, Some(ChannelMsg::Success)));
                channel.window_change(100, 32, 0, 0).await.unwrap();
                channel.data_bytes("Grüße\r".as_bytes()).await.unwrap();
                let mut output = Vec::new();
                let status = tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        match channel.wait().await {
                            Some(ChannelMsg::Data { data }) => output.extend_from_slice(&data),
                            Some(ChannelMsg::ExitStatus { exit_status }) => {
                                break Some(exit_status);
                            }
                            Some(ChannelMsg::Close) | None => break None,
                            _ => {}
                        }
                    }
                })
                .await
                .unwrap();
                assert_eq!(status, Some(0));
                assert_eq!(output, "PTY 100×32: Grüße\r".as_bytes());
            }
        });
    }
}
