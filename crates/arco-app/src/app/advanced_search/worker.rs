use super::{ArchivePassword, ArchiveTab, Directory, Entry, Found, Kind, Options, SearchTask};
use arco_core::archive_view::Catalog;
use arco_core::ignore::IgnoreList;
use arco_core::plugin::Registration;
use arco_core::profile::{Profile, WdxSearchProfile};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use zeroize::{Zeroize, Zeroizing};

const REQUEST_LIMIT: u64 = 4 * 1024 * 1024;
const REPLY_LIMIT: u64 = 1024 * 1024;

#[derive(Serialize, Deserialize)]
pub(super) struct Request {
    #[serde(default)]
    progress: bool,
    options: Options,
    roots: Vec<OsString>,
    show_hidden: bool,
    ignore: Option<String>,
    executable: Option<PathBuf>,
    archive: Option<Archive>,
    password: Option<String>,
    wdx: Option<WdxSearchProfile>,
    plugins: Vec<Registration>,
}

#[derive(Serialize, Deserialize)]
struct Archive {
    file: PathBuf,
    folder: PathBuf,
    catalog_password: Option<String>,
}

impl Drop for Request {
    fn drop(&mut self) {
        self.password.zeroize();
        if let Some(archive) = &mut self.archive {
            archive.catalog_password.zeroize();
        }
    }
}

#[derive(Serialize, Deserialize)]
enum Reply {
    Progress(super::Update),
    Entry {
        path: OsString,
        name: OsString,
        kind: Kind,
        size: Option<u64>,
        modified: Option<(bool, u64, u32)>,
        hidden: bool,
        attributes: u32,
    },
    Warning(String),
    Done {
        cancelled: bool,
    },
    Failed {
        interrupted: bool,
        message: String,
    },
}

impl Request {
    pub(super) fn from_task(task: &SearchTask, profile: &Profile) -> Self {
        let wdx = profile
            .wdx_search_profiles
            .get(task.options.wdx_profile.trim())
            .filter(|_| task.wdx.is_some())
            .cloned();
        let plugins = wdx.as_ref().map_or_else(Vec::new, |wdx| {
            profile
                .plugins
                .iter()
                .filter(|plugin| {
                    wdx.conditions.iter().any(|condition| {
                        condition
                            .plugin
                            .as_os_str()
                            .eq_ignore_ascii_case(plugin.path.as_os_str())
                    })
                })
                .cloned()
                .collect()
        });
        Self {
            progress: true,
            options: task.options.clone(),
            roots: task
                .roots
                .iter()
                .map(|root| root.as_os_str().to_owned())
                .collect(),
            show_hidden: task.show_hidden,
            ignore: task.ignore.as_ref().map(|_| profile.ignore_rules.clone()),
            executable: task.executable.clone(),
            archive: task.archive.as_ref().map(|archive| Archive {
                file: archive.file.clone(),
                folder: archive.folder.clone(),
                catalog_password: archive.password().map(str::to_owned),
            }),
            password: task.password.as_ref().map(|password| password.to_string()),
            wdx,
            plugins,
        }
    }

    fn search(
        &self,
        cancel: &AtomicBool,
        progress: impl FnMut(super::Update),
    ) -> io::Result<Found> {
        let password = |value: Option<&String>| -> Option<ArchivePassword> {
            value.map(|value| Arc::new(Zeroizing::new(value.clone())))
        };
        let archive = self
            .archive
            .as_ref()
            .map(|archive| {
                let catalog_password = archive.catalog_password.as_ref().or(self.password.as_ref());
                let catalog = Catalog::read_with_password(
                    &archive.file,
                    catalog_password.map(String::as_str),
                    cancel,
                )?;
                Ok::<_, io::Error>(ArchiveTab {
                    file: archive.file.clone(),
                    folder: archive.folder.clone(),
                    catalog: Arc::new(catalog),
                    password: password(archive.catalog_password.as_ref()),
                })
            })
            .transpose()?;
        let wdx = self
            .wdx
            .as_ref()
            .map(|wdx| super::super::wdx::search::ProfileFilter::compile(wdx, &self.plugins))
            .transpose()?;
        SearchTask {
            query: super::compile_search(&self.options)?,
            roots: self.roots.iter().map(PathBuf::from).collect(),
            options: self.options.clone(),
            indexed: self
                .options
                .everything()
                .map(|(scoped, expression)| (scoped, expression.to_owned())),
            show_hidden: self.show_hidden,
            ignore: self
                .ignore
                .as_deref()
                .map(IgnoreList::parse)
                .transpose()?
                .map(Arc::new),
            executable: self.executable.clone(),
            archive,
            password: password(self.password.as_ref()),
            wdx,
        }
        .run_with_progress(cancel, progress)
    }
}

pub(crate) fn run() -> io::Result<()> {
    let mut input = Zeroizing::new(Vec::new());
    read_frame(&mut io::stdin().lock(), &mut input, REQUEST_LIMIT)?;
    let request: Request = serde_json::from_slice(&input).map_err(io::Error::other)?;
    drop(input);
    let cancel = Arc::new(AtomicBool::new(false));
    let signal = Arc::clone(&cancel);
    std::thread::Builder::new()
        .name("arco-search-cancel".into())
        .spawn(move || {
            let _ = io::stdin().read(&mut [0u8]);
            signal.store(true, Ordering::Relaxed);
        })?;
    let latest = Arc::new(std::sync::Mutex::new(None::<super::Update>));
    let finished = Arc::new(AtomicBool::new(false));
    let reporter = request.progress.then(|| {
        let latest = latest.clone();
        let finished = finished.clone();
        let cancel = cancel.clone();
        std::thread::spawn(move || -> io::Result<()> {
            let result = (|| {
                loop {
                    let done = finished.load(Ordering::Acquire);
                    let update = latest
                        .lock()
                        .map_err(|_| io::Error::other("Suchfortschritt nicht verfügbar."))?
                        .take();
                    if let Some(update) = update {
                        let mut output = BufWriter::new(io::stdout().lock());
                        write_frame(&mut output, &Reply::Progress(update))?;
                        output.flush()?;
                    }
                    if done {
                        return Ok(());
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            })();
            if result.is_err() {
                cancel.store(true, Ordering::Relaxed);
            }
            result
        })
    });
    let result = request.search(&cancel, |update| {
        if request.progress {
            if let Ok(mut latest) = latest.lock() {
                *latest = Some(update);
            }
        }
    });
    finished.store(true, Ordering::Release);
    if let Some(reporter) = reporter {
        reporter
            .join()
            .map_err(|_| io::Error::other("Suchfortschritt fehlgeschlagen."))??;
    }
    drop(request);
    let mut output = BufWriter::new(io::stdout().lock());
    match result {
        Ok(found) => {
            for entry in found.directory.entries {
                let modified = entry
                    .modified
                    .map(|time| match time.duration_since(UNIX_EPOCH) {
                        Ok(duration) => (false, duration.as_secs(), duration.subsec_nanos()),
                        Err(error) => (
                            true,
                            error.duration().as_secs(),
                            error.duration().subsec_nanos(),
                        ),
                    });
                write_frame(
                    &mut output,
                    &Reply::Entry {
                        path: entry.path.into_os_string(),
                        name: entry.name,
                        kind: entry.kind,
                        size: entry.size,
                        modified,
                        hidden: entry.hidden,
                        attributes: entry.attributes,
                    },
                )?;
            }
            for error in found.directory.errors {
                write_frame(&mut output, &Reply::Warning(error))?;
            }
            write_frame(
                &mut output,
                &Reply::Done {
                    cancelled: found.cancelled,
                },
            )?;
        }
        Err(error) => write_frame(
            &mut output,
            &Reply::Failed {
                interrupted: error.kind() == io::ErrorKind::Interrupted,
                message: error.to_string(),
            },
        )?,
    }
    output.flush()
}

pub(super) fn spawn_with_progress(
    request: Request,
    archive: Option<&ArchiveTab>,
    cancel: &AtomicBool,
    mut progress: impl FnMut(super::Update),
) -> io::Result<Found> {
    if let Some(archive) = archive {
        archive.catalog.check_unchanged()?;
    }
    let payload = Zeroizing::new(serde_json::to_vec(&request).map_err(io::Error::other)?);
    drop(request);
    if payload.len() as u64 >= REQUEST_LIMIT {
        return Err(io::Error::other(
            "Der Suchauftrag ist zu groß für den separaten Prozess.",
        ));
    }
    let mut process = Command::new(std::env::current_exe()?);
    process
        .arg("--search-worker")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        process.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let mut child = process.spawn()?;
    let mut input = child.stdin.take();
    let sent = input
        .as_mut()
        .expect("piped stdin")
        .write_all(&payload)
        .and_then(|()| input.as_mut().unwrap().write_all(b"\n"));
    drop(payload);
    if let Err(error) = sent {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    let output = child.stdout.take().expect("piped stdout");
    let result = std::thread::scope(|scope| {
        enum Message {
            Progress(super::Update),
            Done(io::Result<Found>),
        }
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let read = scope.spawn(move || {
            let result = read_reply(output, |update| {
                let _ = sender.send(Message::Progress(update));
            });
            let _ = sender.send(Message::Done(result));
        });
        let mut interrupted = None;
        loop {
            match receiver.recv_timeout(Duration::from_millis(30)) {
                Ok(Message::Progress(update)) => progress(update),
                Ok(Message::Done(result)) => {
                    input.take();
                    if result.is_err() {
                        let _ = child.kill();
                    }
                    let status = child.wait()?;
                    if result.is_ok() && !status.success() {
                        return Err(io::Error::other(format!("Suchprozess beendet ({status}).")));
                    }
                    return if interrupted.is_some() && result.is_err() {
                        Err(io::ErrorKind::Interrupted.into())
                    } else {
                        result
                    };
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = read.join();
                    return Err(io::Error::other(
                        "Der Suchprozess hat keine Antwort geliefert.",
                    ));
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            }
            if cancel.load(Ordering::Relaxed) {
                input.take();
                let start = interrupted.get_or_insert_with(Instant::now);
                if start.elapsed() > Duration::from_secs(3) {
                    let _ = child.kill();
                }
            }
        }
    });
    if let Some(archive) = archive {
        archive.catalog.check_unchanged()?;
    }
    result
}

fn write_frame(output: &mut impl Write, value: &Reply) -> io::Result<()> {
    serde_json::to_writer(&mut *output, value).map_err(io::Error::other)?;
    output.write_all(b"\n")
}

fn read_frame(input: &mut impl BufRead, frame: &mut Vec<u8>, max: u64) -> io::Result<()> {
    frame.clear();
    let count = input.take(max + 1).read_until(b'\n', frame)?;
    if count == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "Der Suchprozess hat die Verbindung geschlossen.",
        ));
    }
    if count as u64 > max || frame.last() != Some(&b'\n') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Ungültige Nachricht des Suchprozesses.",
        ));
    }
    Ok(())
}

fn read_reply(output: impl Read, mut progress: impl FnMut(super::Update)) -> io::Result<Found> {
    let mut input = BufReader::new(output);
    let mut frame = Vec::new();
    let mut entries = Vec::new();
    let mut errors = Vec::new();
    loop {
        read_frame(&mut input, &mut frame, REPLY_LIMIT)?;
        match serde_json::from_slice(&frame).map_err(io::Error::other)? {
            Reply::Progress(update) => progress(update),
            Reply::Entry {
                path,
                name,
                kind,
                size,
                modified,
                hidden,
                attributes,
            } => {
                let modified: Option<SystemTime> = modified.and_then(|(before, seconds, nanos)| {
                    let duration = Duration::new(seconds, nanos);
                    if before {
                        UNIX_EPOCH.checked_sub(duration)
                    } else {
                        UNIX_EPOCH.checked_add(duration)
                    }
                });
                let mut entry =
                    Entry::from_remote(path.into(), kind, size, modified).with_display_name(name);
                entry.hidden = hidden;
                entry.attributes = attributes;
                entries.push(entry);
            }
            Reply::Warning(error) => errors.push(error),
            Reply::Done { cancelled } => {
                return Ok(Found {
                    directory: Directory { entries, errors },
                    cancelled,
                });
            }
            Reply::Failed {
                interrupted,
                message,
            } => {
                return Err(io::Error::new(
                    if interrupted {
                        io::ErrorKind::Interrupted
                    } else {
                        io::ErrorKind::Other
                    },
                    message,
                ));
            }
        }
    }
}
