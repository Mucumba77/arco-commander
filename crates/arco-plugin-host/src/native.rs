use arco_core::plugin::{
    Interface, Operation, Request, Row, WcxOperation, WdxField, WdxKey, WdxValue,
};
use libloading::os::windows::{
    LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32, Library,
};
use std::ffi::{OsStr, c_void};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::{
    Mutex, OnceLock,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows_sys::Win32::Globalization::{
    CP_ACP, GetACP, MultiByteToWideChar, WC_NO_BEST_FIT_CHARS, WideCharToMultiByte,
};
use windows_sys::Win32::System::Time::FileTimeToSystemTime;
use zeroize::Zeroizing;

mod wdx_edit;
mod wfx;
mod wlx;

const WCX_END_ARCHIVE: i32 = 10;
const MAX_ROWS: usize = 50_000;
static WORKSPACE: OnceLock<PathBuf> = OnceLock::new();

pub fn inspect(request: Request) -> io::Result<Vec<Row>> {
    let _ = WORKSPACE.set(std::env::current_dir()?);
    wfx::set_interactive(request.interface == Interface::Wfx && request.interactive);
    let library = load_library(&request.plugin)?;
    match (request.interface, &request.operation) {
        (interface, Operation::Probe) => probe(&library, interface),
        (Interface::Wcx, Operation::Run) => inspect_wcx(&library, &request.file, None),
        (Interface::Wcx, Operation::Wcx(operation)) => {
            inspect_wcx(&library, &request.file, Some(operation))
        }
        (Interface::Wdx, Operation::Run) => inspect_wdx(&library, &request.file),
        (Interface::Wdx, Operation::WdxFields) => wdx_fields(&library).map(|fields| {
            fields
                .into_iter()
                .map(|field| Row {
                    value: field.units().into(),
                    name: field.name,
                    ..Default::default()
                })
                .collect()
        }),
        (Interface::Wdx, Operation::WdxEditableFields) => wdx_edit::editable_fields(&library),
        (
            Interface::Wdx,
            Operation::WdxSet {
                field,
                value,
                files,
            },
        ) => wdx_edit::set_values(&library, field, wdx_edit::Input::Text(value), files),
        (
            Interface::Wdx,
            Operation::WdxEdit {
                field,
                files,
                parent,
            },
        ) => wdx_edit::set_values(&library, field, wdx_edit::Input::Editor(*parent), files),
        (Interface::Wdx, Operation::WdxValues { field, files }) => {
            wdx_column_values(&library, field, files)
        }
        (Interface::Wfx, Operation::Run) => {
            wfx::inspect(&library, &request.plugin, request.file.as_os_str())
        }
        (Interface::Wfx, Operation::Wfx(operation)) => {
            wfx::operate(&library, &request.plugin, operation)
        }
        (Interface::Wfx, Operation::WfxBatch(operations)) => {
            wfx::batch(&library, &request.plugin, operations)
        }
        _ => Err(io::Error::other(
            "Die Plugin-Operation passt nicht zur Schnittstelle.",
        )),
    }
}

fn probe(library: &Library, interface: Interface) -> io::Result<Vec<Row>> {
    // The exports are inspected as opaque addresses here. This path never calls them.
    let has = |name: &str| symbol::<*const c_void>(library, name.as_bytes()).is_some();
    let exports = |names: &[&str]| names.iter().all(|name| has(name));
    let (wide, ansi, common) = match interface {
        Interface::Wcx => (
            exports(&["OpenArchiveW", "ReadHeaderExW"])
                && (has("ProcessFileW") || has("ProcessFile")),
            exports(&["OpenArchive", "ProcessFile"]) && (has("ReadHeaderEx") || has("ReadHeader")),
            has("CloseArchive"),
        ),
        Interface::Wfx => (
            exports(&["FsInitW", "FsFindFirstW", "FsFindNextW"]),
            exports(&["FsInit", "FsFindFirst", "FsFindNext"]),
            has("FsFindClose"),
        ),
        Interface::Wdx => (
            has("ContentGetValueW"),
            has("ContentGetValue"),
            has("ContentGetSupportedField"),
        ),
        Interface::Wlx => (has("ListLoadW"), has("ListLoad"), true),
    };
    if !common || !(wide || ansi) {
        return Err(io::Error::other(format!(
            "Erforderliche {}-Exporte fehlen.",
            interface.label()
        )));
    }
    let mut rows = vec![
        Row {
            name: "Schnittstelle".into(),
            value: interface.label().into(),
            ..Default::default()
        },
        Row {
            name: "Unicode".into(),
            value: if wide { "ja" } else { "nein" }.into(),
            ..Default::default()
        },
        Row {
            name: "ANSI".into(),
            value: if ansi { "ja" } else { "nein" }.into(),
            ..Default::default()
        },
    ];
    let detect = match interface {
        Interface::Wlx => Some(b"ListGetDetectString".as_slice()),
        Interface::Wdx => Some(b"ContentGetDetectString".as_slice()),
        _ => None,
    };
    type DetectString = unsafe extern "system" fn(*mut u8, i32);
    if let Some(detect) = detect.and_then(|name| symbol::<DetectString>(library, name)) {
        let mut bytes = [0u8; 8192];
        unsafe { detect(bytes.as_mut_ptr(), bytes.len() as i32) };
        let value = ansi_text(&bytes);
        if !value.is_empty() {
            rows.push(Row {
                name: "TC-Erkennung".into(),
                value,
                ..Default::default()
            });
        }
    }
    Ok(rows)
}

pub fn view(
    request: &Request,
    ready: impl FnOnce(Option<usize>) -> io::Result<()>,
) -> io::Result<()> {
    let library = load_library(&request.plugin)?;
    let parent = match request.operation {
        Operation::WlxEmbedded { parent } => Some(parent),
        _ => None,
    };
    wlx::view(&library, &request.file, parent, ready)
}

fn load_library(path: &Path) -> io::Result<Library> {
    let flags = LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32;
    // A native plugin runs only inside this explicitly launched host process.
    unsafe { Library::load_with_flags(path, flags) }.map_err(io::Error::other)
}

fn symbol<T: Copy>(library: &Library, name: &[u8]) -> Option<T> {
    // Each call site declares the matching type from the Total Commander SDK.
    unsafe { library.get::<T>(name) }.ok().map(|symbol| *symbol)
}

fn required<T: Copy>(library: &Library, name: &[u8]) -> io::Result<T> {
    symbol(library, name).ok_or_else(|| {
        io::Error::other(format!(
            "Plugin-Funktion fehlt: {}",
            String::from_utf8_lossy(name)
        ))
    })
}

fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}

fn sdk_path(path: &Path) -> std::borrow::Cow<'_, OsStr> {
    if let Some(text) = path.to_str()
        && text.encode_utf16().count() < 260
    {
        if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
            return std::borrow::Cow::Owned(format!(r"\\{unc}").into());
        }
        if let Some(drive) = text.strip_prefix(r"\\?\")
            && drive.as_bytes().get(1) == Some(&b':')
        {
            return std::borrow::Cow::Borrowed(OsStr::new(drive));
        }
    }
    std::borrow::Cow::Borrowed(path.as_os_str())
}

fn ansi(value: &OsStr) -> io::Result<Vec<u8>> {
    if unsafe { GetACP() } == 65001 {
        return value
            .to_str()
            .map(|text| text.bytes().chain(Some(0)).collect())
            .ok_or_else(|| io::Error::other("Pfad ist für das ANSI-Plugin nicht darstellbar"));
    }
    let wide = Zeroizing::new(wide(value));
    let mut bytes = Zeroizing::new(vec![0; wide.len() * 4]);
    let mut used_default = 0;
    // The source and destination buffers are sized explicitly in their respective units.
    let count = unsafe {
        WideCharToMultiByte(
            CP_ACP,
            WC_NO_BEST_FIT_CHARS,
            wide.as_ptr(),
            wide.len() as i32,
            bytes.as_mut_ptr(),
            bytes.len() as i32,
            null(),
            &mut used_default,
        )
    };
    if count == 0 || used_default != 0 {
        return Err(io::Error::other(
            "Pfad ist für das ANSI-Plugin nicht darstellbar",
        ));
    }
    bytes.truncate(count as usize);
    Ok(std::mem::take(&mut *bytes))
}

fn ansi_text(value: &[u8]) -> String {
    let length = value
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(value.len());
    let mut wide = Zeroizing::new(vec![0; length]);
    // An ANSI string can produce at most one UTF-16 code unit per input byte.
    let count = unsafe {
        MultiByteToWideChar(
            CP_ACP,
            0,
            value.as_ptr(),
            length as i32,
            wide.as_mut_ptr(),
            wide.len() as i32,
        )
    };
    String::from_utf16_lossy(&wide[..count.max(0) as usize])
}

fn wide_text(value: &[u16]) -> String {
    let length = value
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(value.len());
    String::from_utf16_lossy(&value[..length])
}

type Handle = *mut c_void;
type Open<T> = unsafe extern "system" fn(*mut OpenData<T>) -> Handle;
type ReadEx<T> = unsafe extern "system" fn(Handle, *mut HeaderEx<T>) -> i32;
type ReadLegacy = unsafe extern "system" fn(Handle, *mut HeaderLegacy) -> i32;
type Process = unsafe extern "system" fn(Handle, i32, *mut c_void, *mut c_void) -> i32;
type Close = unsafe extern "system" fn(Handle) -> i32;
type SetCallback<T> =
    unsafe extern "system" fn(Handle, unsafe extern "system" fn(*mut T, i32) -> i32);

#[repr(C)]
struct OpenData<T> {
    archive: *mut T,
    mode: i32,
    result: i32,
    comment: *mut T,
    comment_capacity: i32,
    comment_size: i32,
    comment_state: i32,
}

#[repr(C)]
struct HeaderEx<T> {
    archive: [T; 1024],
    name: [T; 1024],
    flags: i32,
    packed: u32,
    packed_high: u32,
    size: u32,
    size_high: u32,
    host_os: i32,
    crc: i32,
    time: i32,
    version: i32,
    method: i32,
    attributes: i32,
    comment: *mut T,
    comment_capacity: i32,
    comment_size: i32,
    comment_state: i32,
    reserved: [u8; 1024],
}

#[repr(C)]
struct HeaderLegacy {
    archive: [u8; 260],
    name: [u8; 260],
    flags: i32,
    packed: i32,
    size: i32,
    host_os: i32,
    crc: i32,
    time: i32,
    version: i32,
    method: i32,
    attributes: i32,
    comment: *mut u8,
    comment_capacity: i32,
    comment_size: i32,
    comment_state: i32,
}

struct ArchiveHandle {
    raw: Handle,
    close: Close,
}

impl Drop for ArchiveHandle {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe { (self.close)(self.raw) };
        }
    }
}

impl ArchiveHandle {
    fn finish(mut self, result: io::Result<Vec<Row>>) -> io::Result<Vec<Row>> {
        let raw = std::mem::replace(&mut self.raw, null_mut());
        let status = unsafe { (self.close)(raw) };
        let rows = result?;
        wcx_status("CloseArchive", status)?;
        Ok(rows)
    }
}

fn inspect_wcx(
    library: &Library,
    file: &Path,
    operation: Option<&WcxOperation>,
) -> io::Result<Vec<Row>> {
    set_wcx_defaults(library)?;
    match operation {
        Some(WcxOperation::Pack { source }) => return pack_wcx(library, file, source),
        Some(WcxOperation::Delete { member }) => return delete_wcx(library, file, member),
        _ => (),
    }
    let close: Close = required(library, b"CloseArchive")?;
    let mode = i32::from(operation.is_some());
    let file = sdk_path(file);
    if let (Some(open), Some(read)) = (
        symbol::<Open<u16>>(library, b"OpenArchiveW"),
        symbol::<ReadEx<u16>>(library, b"ReadHeaderExW"),
    ) {
        let mut name = wide(&file);
        let handle = open_wcx(&mut name, open, close, mode)?;
        callbacks(
            library,
            handle.raw,
            b"SetChangeVolProcW",
            b"SetProcessDataProcW",
            notify_volume::<u16>,
            report_progress::<u16>,
        );
        let process_wide: Option<Process> = symbol(library, b"ProcessFileW");
        let process = process_wide.map_or_else(|| required(library, b"ProcessFile"), Ok)?;
        let result = scan_wcx(
            &handle,
            process,
            |handle| read_wcx_ex(handle, read, wide_text),
            operation,
            process_wide.is_some(),
        );
        return handle.finish(result);
    }
    let open: Open<u8> = required(library, b"OpenArchive")?;
    let process: Process = required(library, b"ProcessFile")?;
    let mut name = ansi(&file)?;
    let handle = open_wcx(&mut name, open, close, mode)?;
    callbacks(
        library,
        handle.raw,
        b"SetChangeVolProc",
        b"SetProcessDataProc",
        notify_volume::<u8>,
        report_progress::<u8>,
    );
    let result = if let Some(read) = symbol::<ReadEx<u8>>(library, b"ReadHeaderEx") {
        scan_wcx(
            &handle,
            process,
            |handle| read_wcx_ex(handle, read, ansi_text),
            operation,
            false,
        )
    } else {
        let read: ReadLegacy = required(library, b"ReadHeader")?;
        scan_wcx(
            &handle,
            process,
            |handle| {
                // The SDK expects the integer fields, buffers and pointers zero-initialized.
                let mut header: HeaderLegacy = unsafe { std::mem::zeroed() };
                let code = unsafe { read(handle, &mut header) };
                (
                    code,
                    ansi_text(&header.name),
                    u64::from(header.size as u32),
                    header.attributes,
                )
            },
            operation,
            false,
        )
    };
    handle.finish(result)
}

fn open_wcx<T>(
    name: &mut [T],
    open: Open<T>,
    close: Close,
    mode: i32,
) -> io::Result<ArchiveHandle> {
    let mut input = OpenData {
        archive: name.as_mut_ptr(),
        mode,
        result: 0,
        comment: null_mut(),
        comment_capacity: 0,
        comment_size: 0,
        comment_state: 0,
    };
    let raw = unsafe { open(&mut input) };
    if raw.is_null() || raw as isize == -1 {
        return Err(io::Error::other(format!(
            "WCX konnte Archiv nicht öffnen (Status {})",
            input.result
        )));
    }
    let handle = ArchiveHandle { raw, close };
    if input.result != 0 {
        return Err(io::Error::other(format!(
            "WCX meldet Status {}",
            input.result
        )));
    }
    Ok(handle)
}

fn read_wcx_ex<T: Copy + Default>(
    handle: Handle,
    read: ReadEx<T>,
    text: impl Fn(&[T]) -> String,
) -> (i32, String, u64, i32) {
    let mut header = HeaderEx {
        archive: [T::default(); 1024],
        name: [T::default(); 1024],
        flags: 0,
        packed: 0,
        packed_high: 0,
        size: 0,
        size_high: 0,
        host_os: 0,
        crc: 0,
        time: 0,
        version: 0,
        method: 0,
        attributes: 0,
        comment: null_mut(),
        comment_capacity: 0,
        comment_size: 0,
        comment_state: 0,
        reserved: [0; 1024],
    };
    let code = unsafe { read(handle, &mut header) };
    (
        code,
        text(&header.name),
        combine(header.size, header.size_high),
        header.attributes,
    )
}

fn scan_wcx(
    handle: &ArchiveHandle,
    process: Process,
    read: impl Fn(Handle) -> (i32, String, u64, i32),
    operation: Option<&WcxOperation>,
    wide_output: bool,
) -> io::Result<Vec<Row>> {
    let mut result = Vec::new();
    let mut tested = 0;
    for _ in 0..MAX_ROWS {
        let (code, name, size, attributes) = read(handle.raw);
        match code {
            WCX_END_ARCHIVE => {
                return match operation {
                    Some(WcxOperation::Extract { .. }) => Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        "Der Archiveintrag fehlt.",
                    )),
                    Some(WcxOperation::Test) => Ok(vec![Row {
                        name: "Dateien geprüft".into(),
                        value: tested.to_string(),
                        ..Default::default()
                    }]),
                    _ => Ok(result),
                };
            }
            0 => (),
            _ => {
                return Err(io::Error::other(format!(
                    "WCX ReadHeader meldet Status {code}"
                )));
            }
        }
        let directory = attributes & 0x10 != 0;
        if let Some(WcxOperation::Extract { member, target }) = operation
            && name.replace('/', "\\") == *member
        {
            if directory {
                return Err(io::Error::other("Wähle eine einzelne Datei im Archiv."));
            }
            let target = sdk_path(target);
            let status = if wide_output {
                unsafe { process(handle.raw, 2, null_mut(), wide(&target).as_mut_ptr().cast()) }
            } else {
                unsafe {
                    process(
                        handle.raw,
                        2,
                        null_mut(),
                        ansi(&target)?.as_mut_ptr().cast(),
                    )
                }
            };
            wcx_status("ProcessFile", status)?;
            return Ok(vec![Row {
                name,
                value: format!("{size} B"),
                ..Default::default()
            }]);
        }
        let action = if matches!(operation, Some(WcxOperation::Test)) && !directory {
            tested += 1;
            1
        } else {
            0
        };
        if operation.is_none() {
            result.push(Row {
                name,
                value: if directory {
                    "Ordner".into()
                } else {
                    format!("{size} B")
                },
                ..Default::default()
            });
        }
        wcx_status("ProcessFile", unsafe {
            process(handle.raw, action, null_mut(), null_mut())
        })?;
    }
    Err(io::Error::other(
        "WCX-Archiv überschreitet das Vorschau-Limit",
    ))
}

fn pack_wcx(library: &Library, archive: &Path, source: &Path) -> io::Result<Vec<Row>> {
    type Pack<T> = unsafe extern "system" fn(*mut T, *mut T, *mut T, *mut T, i32) -> i32;
    let name = source
        .file_name()
        .ok_or_else(|| io::Error::other("Quelldateiname fehlt."))?;
    let parent = source
        .parent()
        .ok_or_else(|| io::Error::other("Quellordner fehlt."))?;
    let parent = sdk_path(parent);
    let mut directory = parent.into_owned();
    directory.push("\\");
    let archive = sdk_path(archive);
    let status = if let Some(pack) = symbol::<Pack<u16>>(library, b"PackFilesW") {
        pack_progress::<u16>(library, b"SetProcessDataProcW");
        let mut name = wide(name);
        name.push(0);
        unsafe {
            pack(
                wide(&archive).as_mut_ptr(),
                null_mut(),
                wide(&directory).as_mut_ptr(),
                name.as_mut_ptr(),
                0,
            )
        }
    } else {
        let pack: Pack<u8> = required(library, b"PackFiles")?;
        pack_progress::<u8>(library, b"SetProcessDataProc");
        let mut name = ansi(name)?;
        name.push(0);
        unsafe {
            pack(
                ansi(&archive)?.as_mut_ptr(),
                null_mut(),
                ansi(&directory)?.as_mut_ptr(),
                name.as_mut_ptr(),
                0,
            )
        }
    };
    wcx_status("PackFiles", status)?;
    Ok(Vec::new())
}

fn delete_wcx(library: &Library, archive: &Path, member: &str) -> io::Result<Vec<Row>> {
    type Delete<T> = unsafe extern "system" fn(*mut T, *mut T) -> i32;
    let archive = sdk_path(archive);
    let member = OsStr::new(member);
    let status = if let Some(delete) = symbol::<Delete<u16>>(library, b"DeleteFilesW") {
        pack_progress::<u16>(library, b"SetProcessDataProcW");
        let mut member = wide(member);
        member.push(0);
        unsafe { delete(wide(&archive).as_mut_ptr(), member.as_mut_ptr()) }
    } else {
        let delete: Delete<u8> = required(library, b"DeleteFiles")?;
        pack_progress::<u8>(library, b"SetProcessDataProc");
        let mut member = ansi(member)?;
        member.push(0);
        unsafe { delete(ansi(&archive)?.as_mut_ptr(), member.as_mut_ptr()) }
    };
    wcx_status("DeleteFiles", status)?;
    Ok(Vec::new())
}

fn pack_progress<T>(library: &Library, name: &[u8]) {
    if let Some(set) = symbol::<SetCallback<T>>(library, name) {
        // For packing/deletion the WCX SDK uses INVALID_HANDLE_VALUE as the global context.
        unsafe { set((-1isize) as Handle, report_progress::<T>) };
    }
}

fn set_wcx_defaults(library: &Library) -> io::Result<()> {
    set_defaults(library, b"PackSetDefaultParams", || {
        Ok(workspace_file("pkplugin.ini"))
    })
}

fn set_defaults(
    library: &Library,
    export: &[u8],
    path: impl FnOnce() -> io::Result<PathBuf>,
) -> io::Result<()> {
    #[repr(C)]
    struct Defaults {
        size: i32,
        version_low: u32,
        version_high: u32,
        ini: [u8; 260],
    }
    type SetDefaults = unsafe extern "system" fn(*mut Defaults);
    let Some(set) = symbol::<SetDefaults>(library, export) else {
        return Ok(());
    };
    let path = path()?;
    let name = ansi(&sdk_path(&path))?;
    let mut defaults = Defaults {
        size: std::mem::size_of::<Defaults>() as i32,
        version_low: 10,
        version_high: 2,
        ini: [0; 260],
    };
    defaults
        .ini
        .get_mut(..name.len())
        .ok_or_else(|| io::Error::other("Pfad für Plugin-Einstellungen ist zu lang."))?
        .copy_from_slice(&name);
    unsafe { set(&mut defaults) };
    Ok(())
}

fn wcx_status(function: &str, status: i32) -> io::Result<()> {
    if status != 0 {
        return Err(io::Error::other(format!(
            "WCX {function} meldet Status {status}."
        )));
    }
    Ok(())
}

fn callbacks<T>(
    library: &Library,
    handle: Handle,
    volume: &[u8],
    progress: &[u8],
    notify: unsafe extern "system" fn(*mut T, i32) -> i32,
    report: unsafe extern "system" fn(*mut T, i32) -> i32,
) {
    if let Some(set) = symbol::<SetCallback<T>>(library, volume) {
        unsafe { set(handle, notify) };
    }
    if let Some(set) = symbol::<SetCallback<T>>(library, progress) {
        unsafe { set(handle, report) };
    }
}

unsafe extern "system" fn notify_volume<T>(_: *mut T, mode: i32) -> i32 {
    i32::from(mode != 0)
}
unsafe extern "system" fn report_progress<T>(_: *mut T, size: i32) -> i32 {
    static BYTES: AtomicU64 = AtomicU64::new(0);
    if size > 0 {
        let bytes = BYTES.fetch_add(size as u64, Ordering::Relaxed) + size as u64;
        progress_message(|| format!("{:.1} MiB verarbeitet", bytes as f64 / (1024.0 * 1024.0)));
    } else if (-100..=-1).contains(&size) {
        progress_message(|| format!("{} %", -size));
    }
    i32::from(!cancelled())
}

fn cancelled() -> bool {
    workspace_file("cancel").is_file()
}

fn progress_message(message: impl FnOnce() -> String) {
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    if let Ok(mut last) = LAST.lock()
        && last.is_none_or(|time| time.elapsed() >= Duration::from_millis(150))
    {
        let _ = std::fs::write(workspace_file("progress"), message());
        *last = Some(Instant::now());
    }
}

fn workspace_file(name: &str) -> PathBuf {
    WORKSPACE
        .get()
        .map_or_else(|| PathBuf::from(name), |root| root.join(name))
}

fn combine(low: u32, high: u32) -> u64 {
    (u64::from(high) << 32) | u64::from(low)
}

type GetField = unsafe extern "system" fn(i32, *mut u8, *mut u8, i32) -> i32;
type GetValue = unsafe extern "system" fn(*mut c_void, i32, i32, *mut c_void, i32, i32) -> i32;

struct NativeWdxField {
    index: i32,
    kind: i32,
    name: String,
    units_or_choices: String,
}

impl NativeWdxField {
    fn units(&self) -> &str {
        // The SDK uses this buffer for the possible values of a choice field.
        if self.kind == 7 {
            ""
        } else {
            &self.units_or_choices
        }
    }

    fn unit_index(&self, unit: Option<&str>) -> io::Result<i32> {
        let Some(unit) = unit else { return Ok(0) };
        self.units()
            .split('|')
            .position(|candidate| !candidate.is_empty() && candidate == unit)
            .map(|index| index as i32)
            .ok_or_else(|| io::Error::other(format!("Die WDX-Einheit „{unit}“ fehlt.")))
    }
}

fn wdx_fields(library: &Library) -> io::Result<Vec<NativeWdxField>> {
    let fields: GetField = required(library, b"ContentGetSupportedField")?;
    let mut result = Vec::new();
    for index in 0..256 {
        let mut name = [0; 256];
        let mut units = [0; 256];
        let kind = unsafe {
            fields(
                index,
                name.as_mut_ptr(),
                units.as_mut_ptr(),
                name.len() as i32,
            )
        };
        if kind == 0 {
            return Ok(result);
        }
        if matches!(kind, 1..=8 | 10 | 11) {
            result.push(NativeWdxField {
                index,
                kind,
                name: ansi_text(&name),
                units_or_choices: ansi_text(&units),
            });
        }
    }
    Err(io::Error::other("WDX überschreitet das Feld-Limit"))
}

fn wdx_column_values(
    library: &Library,
    field: &WdxField,
    files: &[PathBuf],
) -> io::Result<Vec<Row>> {
    let native = wdx_fields(library)?
        .into_iter()
        .find(|native| native.name == field.field)
        .ok_or_else(|| io::Error::other("Das gewählte WDX-Feld fehlt."))?;
    let unit = native.unit_index(field.unit.as_deref())?;
    let index = native.index;
    if files.len() > 128 || files.iter().any(|file| !file.is_absolute()) {
        return Err(io::Error::other("Ungültige WDX-Dateiliste."));
    }
    let unicode = symbol::<GetValue>(library, b"ContentGetValueW");
    let get = unicode.map_or_else(|| required(library, b"ContentGetValue"), Ok)?;
    let mut result = Vec::new();
    for (row, file) in files.iter().enumerate() {
        let filename = sdk_path(file);
        let value = if unicode.is_some() {
            read_wdx_value(get, wide(&filename).as_mut_ptr().cast(), index, unit)
        } else {
            ansi(&filename).ok().and_then(|mut filename| {
                read_wdx_value(get, filename.as_mut_ptr().cast(), index, unit)
            })
        };
        if let Some(value) = value {
            result.push(Row {
                name: row.to_string(),
                value: serde_json::to_string(&value).map_err(io::Error::other)?,
                ..Default::default()
            });
        }
    }
    Ok(result)
}

fn read_wdx_value(get: GetValue, filename: *mut c_void, index: i32, unit: i32) -> Option<WdxValue> {
    // Native values require alignment for integers and floating-point values.
    let mut value = [0u64; 512];
    let code = unsafe {
        get(
            filename,
            index,
            unit,
            value.as_mut_ptr().cast(),
            std::mem::size_of_val(&value) as i32,
            0,
        )
    };
    let display = wdx_value(code, &value)?;
    let key = match code {
        1 => WdxKey::Integer(i64::from(value[0] as i32)),
        2 => WdxKey::Integer(value[0] as i64),
        3 => {
            let number = f64::from_bits(value[0]);
            if !number.is_finite() {
                return None;
            }
            WdxKey::Float(number)
        }
        6 => WdxKey::Boolean(value[0] as u32 != 0),
        10 => WdxKey::DateTime(value[0]),
        _ => WdxKey::Text(display.to_lowercase()),
    };
    Some(WdxValue { display, key })
}

fn inspect_wdx(library: &Library, file: &Path) -> io::Result<Vec<Row>> {
    let fields: GetField = required(library, b"ContentGetSupportedField")?;
    if let Some(get) = symbol::<GetValue>(library, b"ContentGetValueW") {
        let mut filename = wide(file.as_os_str());
        scan_wdx(fields, get, filename.as_mut_ptr().cast())
    } else {
        let get: GetValue = required(library, b"ContentGetValue")?;
        let mut filename = ansi(file.as_os_str())?;
        scan_wdx(fields, get, filename.as_mut_ptr().cast())
    }
}

fn scan_wdx(fields: GetField, get: GetValue, filename: *mut c_void) -> io::Result<Vec<Row>> {
    let mut result = Vec::new();
    for index in 0..256 {
        let mut name = [0; 256];
        let mut units = [0; 256];
        let field = unsafe {
            fields(
                index,
                name.as_mut_ptr(),
                units.as_mut_ptr(),
                name.len() as i32,
            )
        };
        if field == 0 {
            return Ok(result);
        }
        if !(1..=12).contains(&field) {
            continue;
        }
        if let Some(value) = read_wdx_value(get, filename, index, 0) {
            result.push(Row {
                name: ansi_text(&name),
                value: value.display,
                ..Default::default()
            });
        }
    }
    Err(io::Error::other("WDX überschreitet das Feld-Limit"))
}

fn wdx_value(code: i32, value: &[u64; 512]) -> Option<String> {
    let data = value[0].to_le_bytes();
    let short = |index: usize| u16::from_le_bytes([data[index * 2], data[index * 2 + 1]]);
    Some(match code {
        1 => (value[0] as i32).to_string(),
        2 => (value[0] as i64).to_string(),
        3 => f64::from_bits(value[0]).to_string(),
        4 => format!("{:04}-{:02}-{:02}", short(0), short(1), short(2)),
        5 => format!("{:02}:{:02}:{:02}", short(0), short(1), short(2)),
        6 => if value[0] as u32 == 0 { "Nein" } else { "Ja" }.into(),
        7..=9 => ansi_text(
            &value
                .iter()
                .flat_map(|word| word.to_le_bytes())
                .collect::<Vec<_>>(),
        ),
        10 => format_file_time(FILETIME {
            dwLowDateTime: value[0] as u32,
            dwHighDateTime: (value[0] >> 32) as u32,
        })?,
        11 | 12 => {
            let words = value
                .iter()
                .flat_map(|word| {
                    [
                        *word as u16,
                        (*word >> 16) as u16,
                        (*word >> 32) as u16,
                        (*word >> 48) as u16,
                    ]
                })
                .collect::<Vec<_>>();
            wide_text(&words)
        }
        _ => return None,
    })
}

fn format_file_time(value: FILETIME) -> Option<String> {
    let mut system = SYSTEMTIME::default();
    if unsafe { FileTimeToSystemTime(&value, &mut system) } == 0 {
        return None;
    }
    Some(format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC",
        system.wYear, system.wMonth, system.wDay, system.wHour, system.wMinute, system.wSecond
    ))
}
