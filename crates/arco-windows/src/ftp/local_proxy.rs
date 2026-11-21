use super::{invalid_input, proxy_template};
use std::io;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, Join, ReadBuf};
use tokio::process::{Child, ChildStdin, ChildStdout};

pub(super) struct Command {
    program: String,
    arguments: Vec<proxy_template::Command>,
    pub scope: blake3::Hash,
    pub user: bool,
    pub password: bool,
}

impl Command {
    pub fn parse(template: &str) -> io::Result<Self> {
        if template.trim().is_empty()
            || template.len() > 4096
            || template.chars().any(char::is_control)
        {
            return Err(invalid_input(
                "Gib ein gültiges lokales SSH-Proxy-Kommando ein.",
            ));
        }
        let mut words = crate::parse_windows_arguments(template.trim())?.into_iter();
        let program = words
            .next()
            .filter(|word| !word.trim().is_empty())
            .ok_or_else(|| invalid_input("Das Proxy-Programm fehlt."))?;
        let arguments = words
            .map(|word| proxy_template::Command::argument(&word))
            .collect::<io::Result<Vec<_>>>()?;
        Ok(Self {
            program,
            user: arguments.iter().any(|argument| argument.user),
            password: arguments.iter().any(|argument| argument.password),
            arguments,
            scope: blake3::hash(template.trim().as_bytes()),
        })
    }

    pub fn spawn(
        &self,
        host: &str,
        port: u16,
        user: &str,
        password: &str,
    ) -> io::Result<(Stream, Process)> {
        let mut command = tokio::process::Command::new(&self.program);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
        for argument in &self.arguments {
            let bytes = argument.render(host, port, user, password)?;
            let value = std::str::from_utf8(&bytes)
                .ok()
                .filter(|value| !value.contains('\0'))
                .ok_or_else(|| {
                    invalid_input("Die Argumente des lokalen SSH-Proxys müssen gültiger Text sein.")
                })?;
            command.arg(value);
        }
        let mut process = command.spawn().map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("Der lokale SSH-Proxy konnte nicht starten. {error}"),
            )
        })?;
        let input = process.stdin.take().expect("piped proxy input");
        let output = process.stdout.take().expect("piped proxy output");
        let child = Arc::new(Mutex::new(process));
        Ok((
            Stream {
                child: child.clone(),
                pipes: tokio::io::join(output, input),
            },
            Process(child),
        ))
    }
}

pub(super) struct Process(Arc<Mutex<Child>>);

impl Drop for Process {
    fn drop(&mut self) {
        // The SSH worker may still own its pipes after the connection is cancelled.
        let _ = self
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .start_kill();
    }
}

pub(super) struct Stream {
    child: Arc<Mutex<Child>>,
    pipes: Join<ChildStdout, ChildStdin>,
}

impl AsyncRead for Stream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buffer.filled().len();
        let result = Pin::new(&mut self.pipes).poll_read(cx, buffer);
        if matches!(result, Poll::Ready(Ok(())))
            && buffer.remaining() != 0
            && before == buffer.filled().len()
            && let Some(status) = self
                .child
                .lock()
                .ok()
                .and_then(|mut child| child.try_wait().ok().flatten())
            && !status.success()
        {
            return Poll::Ready(Err(io::Error::other(format!(
                "Der lokale SSH-Proxy wurde vorzeitig beendet ({status})."
            ))));
        }
        result
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.pipes).poll_write(cx, buffer)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.pipes).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.pipes).poll_shutdown(cx)
    }
}
