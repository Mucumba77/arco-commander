use crate::files;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};
use zeroize::{Zeroize, Zeroizing};

mod operations;
mod registry;
mod wdx;
mod wfx_content;
mod wlx_bitmap;
pub use operations::{
    DownloadConflict, WFX_TREE_DEPTH, WFX_TREE_ENTRIES, WcxOperation, WfxOperation, WfxOverwrite,
    WfxVersion, operate, operate_interactive, wfx_batch, wfx_batch_filtered,
};
pub use registry::*;
pub use wdx::{WdxKey, WdxMatcher, WdxValue, wdx_order};
pub use wfx_content::{
    WFX_CONTENT_BATCH, WFX_CONTENT_COLUMNS, WfxDefaultView, WfxField, wfx_content_fields,
    wfx_content_paths, wfx_content_values, wfx_default_view,
};
pub use wlx_bitmap::{
    WLX_BITMAP_BATCH, WLX_BITMAP_HEIGHT, WLX_BITMAP_WIDTH, WlxBitmap, wlx_thumbnails,
};

pub const PROTOCOL_VERSION: u32 = 12;
pub const MAX_REQUEST: u64 = 768 * 1024;
const MAX_REPLY: u64 = 24 * 1024 * 1024;
const HOST_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum Interface {
    Wcx,
    Wdx,
    Wfx,
    Wlx,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
pub enum Operation {
    #[default]
    Run,
    Probe,
    Wcx(WcxOperation),
    Wfx(WfxOperation),
    WfxBatch(Vec<WfxOperation>),
    WfxFilteredBatch {
        operations: Vec<WfxOperation>,
        plugin_name: String,
        ignore: String,
    },
    WfxContentFields,
    WfxDefaultView,
    WfxContentValues {
        fields: Vec<WdxField>,
        names: Vec<String>,
    },
    WdxFields,
    WdxEditableFields,
    WdxSet {
        field: WdxField,
        value: String,
        files: Vec<PathBuf>,
    },
    WdxEdit {
        field: WdxField,
        files: Vec<PathBuf>,
        parent: usize,
    },
    WdxValues {
        field: WdxField,
        files: Vec<PathBuf>,
    },
    WlxEmbedded {
        parent: usize,
    },
    WlxThumbnails {
        files: Vec<PathBuf>,
    },
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Request {
    pub version: u32,
    pub interface: Interface,
    pub plugin: PathBuf,
    pub file: PathBuf,
    #[serde(default)]
    pub operation: Operation,
    #[serde(default)]
    pub interactive: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Reply {
    pub version: u32,
    pub result: Result<Vec<Row>, String>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Row {
    pub name: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wfx_version: Option<WfxVersion>,
    #[serde(default)]
    pub wfx_skipped: bool,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub wfx_ignored: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wfx_content: Vec<Option<WdxValue>>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub wfx_descending: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wfx_default_view: Option<WfxDefaultView>,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct WdxColumn {
    pub plugin: PathBuf,
    #[serde(flatten)]
    pub selection: WdxField,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct WdxField {
    pub field: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WdxEditableField {
    pub selection: WdxField,
    pub kind: i32,
    pub choices: Vec<String>,
    #[serde(default)]
    pub editor: bool,
}

pub fn wdx_editable_fields(
    plugin: &Path,
    cancel: &AtomicBool,
) -> io::Result<Vec<WdxEditableField>> {
    inspect_host(
        Interface::Wdx,
        plugin,
        Path::new(""),
        Operation::WdxEditableFields,
        cancel,
        |_| (),
    )?
    .into_iter()
    .map(|row| serde_json::from_str(&row.value).map_err(io::Error::other))
    .collect()
}

pub fn wdx_set(
    plugin: &Path,
    field: WdxField,
    value: String,
    files: Vec<PathBuf>,
    cancel: &AtomicBool,
    progress: impl FnMut(String),
) -> io::Result<Vec<Row>> {
    inspect_host(
        Interface::Wdx,
        plugin,
        Path::new(""),
        Operation::WdxSet {
            field,
            value,
            files,
        },
        cancel,
        progress,
    )
}

pub fn wdx_edit(
    plugin: &Path,
    field: WdxField,
    files: Vec<PathBuf>,
    parent: usize,
    cancel: &AtomicBool,
    progress: impl FnMut(String),
) -> io::Result<Vec<Row>> {
    inspect_host(
        Interface::Wdx,
        plugin,
        Path::new(""),
        Operation::WdxEdit {
            field,
            files,
            parent,
        },
        cancel,
        progress,
    )
}

impl WdxField {
    pub fn label(&self) -> String {
        match &self.unit {
            Some(unit) => format!("{} ({unit})", self.field),
            None => self.field.clone(),
        }
    }
}

pub fn wdx_fields(plugin: &Path, cancel: &AtomicBool) -> io::Result<Vec<Row>> {
    inspect_host(
        Interface::Wdx,
        plugin,
        Path::new(""),
        Operation::WdxFields,
        cancel,
        |_| (),
    )
}

pub fn wdx_values(
    plugin: &Path,
    field: String,
    files: Vec<PathBuf>,
    cancel: &AtomicBool,
) -> io::Result<Vec<Row>> {
    wdx_typed_values(plugin, field, files, cancel).map(|values| {
        values
            .into_iter()
            .map(|(index, value)| Row {
                name: index.to_string(),
                value: value.display,
                ..Default::default()
            })
            .collect()
    })
}

pub fn wdx_typed_values(
    plugin: &Path,
    field: String,
    files: Vec<PathBuf>,
    cancel: &AtomicBool,
) -> io::Result<Vec<(usize, WdxValue)>> {
    wdx_typed_field_values(plugin, WdxField { field, unit: None }, files, cancel)
}

pub fn wdx_typed_field_values(
    plugin: &Path,
    field: WdxField,
    files: Vec<PathBuf>,
    cancel: &AtomicBool,
) -> io::Result<Vec<(usize, WdxValue)>> {
    if files.len() > 128 || files.iter().any(|file| !file.is_absolute()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Ungültige WDX-Dateiliste.",
        ));
    }
    let count = files.len();
    let rows = inspect_host(
        Interface::Wdx,
        plugin,
        Path::new(""),
        Operation::WdxValues { field, files },
        cancel,
        |_| (),
    )?;
    let mut previous = None;
    rows.into_iter()
        .map(|row| {
            let index = row
                .name
                .parse::<usize>()
                .ok()
                .filter(|index| *index < count && previous.is_none_or(|last| *index > last))
                .ok_or_else(|| io::Error::other("Ungültiger WDX-Ergebnisindex."))?;
            previous = Some(index);
            let value = serde_json::from_str(&row.value).map_err(io::Error::other)?;
            Ok((index, value))
        })
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum PromptKind {
    Text,
    Secret,
    Acknowledge,
    Confirm,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Prompt {
    pub kind: PromptKind,
    pub title: String,
    pub text: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PromptMessage {
    pub version: u32,
    pub prompt: Prompt,
}

#[derive(Deserialize, Serialize)]
pub struct PromptAnswer {
    pub value: String,
}

impl Drop for PromptAnswer {
    fn drop(&mut self) {
        self.value.zeroize();
    }
}

pub type PromptHandler<'a> = dyn FnMut(Prompt) -> Option<PromptAnswer> + 'a;

#[derive(Deserialize)]
#[serde(untagged)]
enum HostMessage {
    Reply(Reply),
    Prompt(PromptMessage),
}

pub fn inspect(
    interface: Interface,
    plugin: &Path,
    file: &Path,
    cancel: &AtomicBool,
) -> io::Result<Vec<Row>> {
    if matches!(interface, Interface::Wlx) {
        return Err(io::Error::other("WLX benötigt einen Vorschauprozess"));
    }
    inspect_host(interface, plugin, file, Operation::Run, cancel, |_| ())
}

pub fn probe(interface: Interface, plugin: &Path, cancel: &AtomicBool) -> io::Result<Vec<Row>> {
    inspect_host(
        interface,
        plugin,
        Path::new(""),
        Operation::Probe,
        cancel,
        |_| (),
    )
}

pub fn list_wfx(
    plugin: &Path,
    directory: &Path,
    cancel: &AtomicBool,
    prompt: &mut PromptHandler<'_>,
) -> io::Result<Vec<Row>> {
    inspect_interactive(
        Interface::Wfx,
        plugin,
        directory,
        Operation::Run,
        cancel,
        |_| (),
        Some(prompt),
    )
}

fn inspect_host(
    interface: Interface,
    plugin: &Path,
    file: &Path,
    operation: Operation,
    cancel: &AtomicBool,
    progress: impl FnMut(String),
) -> io::Result<Vec<Row>> {
    inspect_interactive(interface, plugin, file, operation, cancel, progress, None)
}

fn inspect_interactive(
    interface: Interface,
    plugin: &Path,
    file: &Path,
    operation: Operation,
    cancel: &AtomicBool,
    mut progress: impl FnMut(String),
    mut prompt: Option<&mut PromptHandler<'_>>,
) -> io::Result<Vec<Row>> {
    let report_after_cancel = matches!(
        operation,
        Operation::WdxSet { .. } | Operation::WdxEdit { .. }
    );
    let file_operation = matches!(
        operation,
        Operation::Wcx(_)
            | Operation::Wfx(_)
            | Operation::WfxBatch(_)
            | Operation::WfxFilteredBatch { .. }
    ) || report_after_cancel;
    let timeout = if matches!(
        operation,
        Operation::WlxThumbnails { .. } | Operation::WfxDefaultView
    ) {
        Duration::from_secs(10)
    } else if file_operation {
        Duration::from_secs(3600)
    } else {
        HOST_TIMEOUT
    };
    let (mut host, request) =
        start_host(interface, plugin, file, operation, cancel, prompt.is_some())?;
    let mut input = host.child.stdin.take().expect("stdin piped");
    let output = host.child.stdout.take().expect("stdout piped");
    let mut writer = Some(std::thread::spawn(move || {
        input.write_all(&request)?;
        Ok::<_, io::Error>(input)
    }));
    let mut input = None;
    let (sender, replies) = mpsc::channel();
    std::thread::spawn(move || {
        let mut output = BufReader::new(output);
        loop {
            let mut bytes = Vec::new();
            let result = (&mut output)
                .take(MAX_REPLY + 1)
                .read_until(b'\n', &mut bytes)
                .and_then(|count| {
                    if count as u64 > MAX_REPLY || bytes.last() != Some(&b'\n') {
                        return Err(io::Error::other(
                            "Der Plugin-Host hat keine gültige Antwort gesendet.",
                        ));
                    }
                    serde_json::from_slice::<HostMessage>(&bytes).map_err(io::Error::other)
                });
            let finished = !matches!(result, Ok(HostMessage::Prompt(_)));
            if sender.send(result).is_err() || finished {
                break;
            }
        }
    });
    let mut started = Instant::now();
    let mut last_report = String::new();
    let mut last_poll = started;
    let mut result = None;
    let mut cancelling = None;
    loop {
        if file_operation && cancel.load(std::sync::atomic::Ordering::Relaxed) {
            if cancelling.is_none() {
                let _ = std::fs::write(host.workspace.path().join("cancel"), []);
                cancelling = Some(Instant::now());
            }
            if report_after_cancel {
                if cancelling.is_some_and(|since| since.elapsed() >= Duration::from_secs(1)) {
                    return Err(io::Error::other(
                        "WDX-Schreibauftrag abgebrochen. Bereits geschriebene Werte können erhalten sein.",
                    ));
                }
            } else {
                let grace = Instant::now();
                while grace.elapsed() < Duration::from_millis(500) && host.try_wait()?.is_none() {
                    std::thread::sleep(Duration::from_millis(25));
                }
                return Err(io::ErrorKind::Interrupted.into());
            }
        } else {
            check_host_timeout(started, timeout, cancel)?;
        }
        if file_operation && last_poll.elapsed() >= Duration::from_millis(100) {
            last_poll = Instant::now();
            let mut report = String::new();
            if File::open(host.workspace.path().join("progress"))
                .and_then(|file| file.take(128).read_to_string(&mut report))
                .is_ok()
                && !report.is_empty()
                && report != last_report
            {
                last_report.clone_from(&report);
                progress(report);
            }
        }
        let status = host.try_wait()?;
        if let Some(status) = status
            && !status.success()
        {
            return Err(io::Error::other(format!(
                "Plugin-Host beendet mit Status {status}"
            )));
        }
        if status.is_some()
            && let Some(result) = result.take()
        {
            return result;
        }
        if result.is_some() {
            std::thread::sleep(Duration::from_millis(25));
            continue;
        }
        match replies.recv_timeout(Duration::from_millis(25)) {
            Ok(Ok(HostMessage::Reply(reply))) => result = Some(checked_reply(reply)),
            Ok(Ok(HostMessage::Prompt(message))) => {
                if message.version != PROTOCOL_VERSION {
                    return Err(io::Error::other("Ungültige Version der Plugin-Abfrage."));
                }
                let handler = prompt
                    .as_mut()
                    .ok_or_else(|| io::Error::other("Unerwartete Plugin-Abfrage."))?;
                let waiting = Instant::now();
                let answer = handler(message.prompt);
                started += waiting.elapsed();
                files::check_cancel(cancel)?;
                if let Some(writer) = writer.take() {
                    input = Some(
                        writer
                            .join()
                            .map_err(|_| io::Error::other("Plugin-Eingabe fehlgeschlagen"))??,
                    );
                }
                let mut bytes =
                    Zeroizing::new(serde_json::to_vec(&answer).map_err(io::Error::other)?);
                if bytes.len() >= 8192 {
                    bytes.zeroize();
                    bytes.extend_from_slice(b"null");
                }
                bytes.push(b'\n');
                input
                    .as_mut()
                    .expect("stdin for interactive host")
                    .write_all(&bytes)?;
            }
            Ok(Err(error)) => result = Some(Err(error)),
            Err(RecvTimeoutError::Timeout) => (),
            Err(RecvTimeoutError::Disconnected) => {
                return Err(io::Error::other("Plugin-Antwort fehlt."));
            }
        }
    }
}

pub struct HostProcess {
    child: Child,
    workspace: tempfile::TempDir,
}

impl HostProcess {
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for HostProcess {
    fn drop(&mut self) {
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

pub fn view(plugin: &Path, file: &Path, cancel: &AtomicBool) -> io::Result<HostProcess> {
    launch_view(plugin, file, Operation::Run, cancel).map(|(host, _)| host)
}

pub fn embedded_view(
    plugin: &Path,
    file: &Path,
    parent: usize,
    cancel: &AtomicBool,
) -> io::Result<(HostProcess, usize)> {
    let (host, rows) = launch_view(plugin, file, Operation::WlxEmbedded { parent }, cancel)?;
    let window = rows
        .into_iter()
        .find(|row| row.name == "hwnd")
        .and_then(|row| row.value.parse::<usize>().ok())
        .filter(|window| *window != 0)
        .ok_or_else(|| io::Error::other("WLX-Host meldet kein eingebettetes Fenster."))?;
    Ok((host, window))
}

fn launch_view(
    plugin: &Path,
    file: &Path,
    operation: Operation,
    cancel: &AtomicBool,
) -> io::Result<(HostProcess, Vec<Row>)> {
    let (mut host, request) = start_host(Interface::Wlx, plugin, file, operation, cancel, false)?;
    let mut input = host.child.stdin.take().expect("stdin piped");
    let output = host.child.stdout.take().expect("stdout piped");
    std::thread::spawn(move || input.write_all(&request));
    let (sender, replies) = mpsc::channel();
    std::thread::spawn(move || {
        let mut response = Vec::new();
        let result = BufReader::new(output.take(MAX_REQUEST + 1))
            .read_until(b'\n', &mut response)
            .and_then(|count| {
                if count as u64 > MAX_REQUEST || response.last() != Some(&b'\n') {
                    Err(io::Error::other(
                        "WLX-Host hat keine gültige Startmeldung gesendet",
                    ))
                } else {
                    decode_reply(&response)
                }
            });
        let _ = sender.send(result);
    });
    let started = Instant::now();
    loop {
        check_host_timeout(started, HOST_TIMEOUT, cancel)?;
        match replies.recv_timeout(Duration::from_millis(25)) {
            Ok(result) => {
                return Ok((host, result?));
            }
            Err(RecvTimeoutError::Timeout) => (),
            Err(RecvTimeoutError::Disconnected) => {
                return Err(io::Error::other("WLX-Startmeldung fehlt"));
            }
        }
    }
}

fn start_host(
    interface: Interface,
    plugin: &Path,
    file: &Path,
    operation: Operation,
    cancel: &AtomicBool,
    interactive: bool,
) -> io::Result<(HostProcess, Vec<u8>)> {
    let plugin = std::fs::canonicalize(plugin)?;
    if Interface::for_path(&plugin) != Some(interface) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "die Datei hat keine passende Plugin-Erweiterung",
        ));
    }
    let host = locate_host(&plugin)?;
    let workspace = tempfile::tempdir()?;
    let file = if matches!(
        operation,
        Operation::Probe
            | Operation::WdxFields
            | Operation::WdxValues { .. }
            | Operation::WdxEditableFields
            | Operation::WdxSet { .. }
            | Operation::WdxEdit { .. }
            | Operation::WlxThumbnails { .. }
    ) || interface == Interface::Wfx
    {
        file.to_path_buf()
    } else {
        std::path::absolute(file)?
    };
    let mut request = serde_json::to_vec(&Request {
        version: PROTOCOL_VERSION,
        interface,
        plugin,
        file,
        operation,
        interactive,
    })
    .map_err(io::Error::other)?;
    if request.len() as u64 >= MAX_REQUEST {
        return Err(io::Error::other("Plugin-Anfrage ist zu groß"));
    }
    request.push(b'\n');
    let mut command = Command::new(host);
    command
        .current_dir(workspace.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    files::check_cancel(cancel)?;
    Ok((
        HostProcess {
            child: command.spawn()?,
            workspace,
        },
        request,
    ))
}

fn check_host_timeout(started: Instant, timeout: Duration, cancel: &AtomicBool) -> io::Result<()> {
    files::check_cancel(cancel)?;
    if started.elapsed() > timeout {
        return Err(io::Error::other(format!(
            "Plugin-Host hat das Zeitlimit von {} s erreicht",
            timeout.as_secs()
        )));
    }
    Ok(())
}

fn decode_reply(bytes: &[u8]) -> io::Result<Vec<Row>> {
    let reply: Reply = serde_json::from_slice(bytes).map_err(io::Error::other)?;
    checked_reply(reply)
}

fn checked_reply(reply: Reply) -> io::Result<Vec<Row>> {
    if reply.version != PROTOCOL_VERSION {
        return Err(io::Error::other(
            "Plugin-Host verwendet eine andere Protokollversion",
        ));
    }
    reply.result.map_err(io::Error::other)
}

fn locate_host(plugin: &Path) -> io::Result<PathBuf> {
    let machine = pe_machine(plugin)?;
    let executable = std::env::current_exe()?;
    let folder = executable
        .parent()
        .ok_or_else(|| io::Error::other("fehlender Programmordner"))?;
    let name = match machine {
        0x8664 => "arco-plugin-host.exe",
        0x014c => "arco-plugin-host-x86.exe",
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "nur x86- und x64-Plugins werden unterstützt",
            ));
        }
    };
    let host = folder.join(name);
    if !host.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{name} fehlt neben Arco Commander"),
        ));
    }
    if pe_machine(&host)? != machine {
        return Err(io::Error::other(
            "Host und Plugin haben unterschiedliche Architekturen",
        ));
    }
    Ok(host)
}

fn pe_machine(path: &Path) -> io::Result<u16> {
    let header = pe_header(path)?;
    Ok(u16::from_le_bytes([header[4], header[5]]))
}

fn pe_header(path: &Path) -> io::Result<[u8; 24]> {
    let mut file = File::open(path)?;
    let mut dos = [0u8; 64];
    file.read_exact(&mut dos)?;
    if &dos[..2] != b"MZ" {
        return Err(io::Error::other("ungültiges Windows-Binärformat"));
    }
    let offset = u32::from_le_bytes(dos[60..64].try_into().unwrap());
    if !(64..=1_048_576).contains(&offset) {
        return Err(io::Error::other("ungültiger PE-Kopf"));
    }
    file.seek(SeekFrom::Start(u64::from(offset)))?;
    let mut header = [0u8; 24];
    file.read_exact(&mut header)?;
    if &header[..4] != b"PE\0\0" {
        return Err(io::Error::other("ungültiges Windows-Binärformat"));
    }
    Ok(header)
}
