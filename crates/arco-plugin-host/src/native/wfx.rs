use super::{
    Handle, MAX_ROWS, ansi, ansi_text, cancelled, combine, progress_message, required, sdk_path,
    set_defaults, symbol, wide, wide_text, workspace_file,
};
use arco_core::plugin::{
    PROTOCOL_VERSION, Prompt, PromptAnswer, PromptKind, PromptMessage, Row, WFX_TREE_DEPTH,
    WFX_TREE_ENTRIES, WfxOperation, WfxVersion,
};
use arco_core::{files, tools};
use libloading::os::windows::Library;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, BufRead, Read, Write};
use std::path::Path;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};
use windows_sys::Win32::Foundation::{
    ERROR_NO_MORE_FILES, FILETIME, GetLastError, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{WIN32_FIND_DATAA, WIN32_FIND_DATAW};
use zeroize::{Zeroize, Zeroizing};

mod credentials;

type Progress<T> = unsafe extern "system" fn(i32, *mut T, *mut T, i32) -> i32;
type Log<T> = unsafe extern "system" fn(i32, i32, *mut T);
type Request<T> = unsafe extern "system" fn(i32, i32, *mut T, *mut T, *mut T, i32) -> i32;
type Init<T> = unsafe extern "system" fn(i32, Progress<T>, Log<T>, Request<T>) -> i32;
type First<T, Data> = unsafe extern "system" fn(*mut T, *mut Data) -> Handle;
type Next<Data> = unsafe extern "system" fn(Handle, *mut Data) -> i32;
type Close = unsafe extern "system" fn(Handle) -> i32;

#[repr(C)]
#[derive(Clone, Copy)]
struct RemoteInfo {
    low: u32,
    high: u32,
    modified: FILETIME,
    attributes: i32,
}

struct Entry {
    name: String,
    info: RemoteInfo,
    unix_mode: u32,
}

impl Entry {
    fn directory(&self) -> bool {
        self.info.attributes & 0x10 != 0
    }

    fn link(&self) -> bool {
        self.info.attributes & 0x400 != 0
            || (self.info.attributes as u32 & 0x8000_0000 != 0
                && self.unix_mode & 0o170000 == 0o120000)
    }

    fn version(&self) -> Option<WfxVersion> {
        (!self.directory() && !self.link()).then(|| WfxVersion {
            size: combine(self.info.low, self.info.high),
            modified: combine(
                self.info.modified.dwLowDateTime,
                self.info.modified.dwHighDateTime,
            ),
            attributes: self.info.attributes as u32,
        })
    }
}

impl RemoteInfo {
    fn fingerprint(self) -> (u32, u32, u32, u32, i32) {
        (
            self.low,
            self.high,
            self.modified.dwLowDateTime,
            self.modified.dwHighDateTime,
            self.attributes,
        )
    }
}

pub fn inspect(library: &Library, plugin: &Path, directory: &OsStr) -> io::Result<Vec<Row>> {
    let unicode = wide_listing(library);
    initialize(library, plugin, unicode)?;
    Ok(entries(library, directory, unicode)?
        .into_iter()
        .map(|entry| {
            let value = if entry.directory() {
                "Ordner".into()
            } else {
                format!("{} B", combine(entry.info.low, entry.info.high))
            };
            let wfx_version = entry.version();
            Row {
                name: entry.name,
                value,
                wfx_version,
            }
        })
        .collect())
}

fn wide_listing(library: &Library) -> bool {
    symbol::<Init<u16>>(library, b"FsInitW").is_some()
        && symbol::<First<u16, WIN32_FIND_DATAW>>(library, b"FsFindFirstW").is_some()
        && symbol::<Next<WIN32_FIND_DATAW>>(library, b"FsFindNextW").is_some()
}

fn initialize(library: &Library, plugin: &Path, unicode: bool) -> io::Result<()> {
    let status = if unicode {
        if let Some(init) = symbol::<Init<u8>>(library, b"FsInit") {
            unsafe { init(1, progress::<u8>, log::<u8>, request::<u8>) };
        }
        let init: Init<u16> = required(library, b"FsInitW")?;
        unsafe { init(1, progress::<u16>, log::<u16>, request::<u16>) }
    } else {
        let init: Init<u8> = required(library, b"FsInit")?;
        unsafe { init(1, progress::<u8>, log::<u8>, request::<u8>) }
    };
    if status != 0 {
        return Err(io::Error::other(format!(
            "WFX FsInit meldet Status {status}."
        )));
    }
    let stem = plugin.file_stem().ok_or(io::ErrorKind::InvalidInput)?;
    let dir = plugin.with_file_name("arco-wfx").join(stem);
    set_defaults(library, b"FsSetDefaultParams", || {
        fs::create_dir_all(&dir)?;
        Ok(dir.join("fsplugin.ini"))
    })?;
    credentials::initialize(library, dir, unicode)
}

fn entries(library: &Library, directory: &OsStr, unicode: bool) -> io::Result<Vec<Entry>> {
    let close: Close = required(library, b"FsFindClose")?;
    if unicode {
        let first: First<u16, WIN32_FIND_DATAW> = required(library, b"FsFindFirstW")?;
        let next: Next<WIN32_FIND_DATAW> = required(library, b"FsFindNextW")?;
        return scan(buffer(wide(directory))?, first, next, close, |item| Entry {
            name: wide_text(&item.cFileName),
            unix_mode: item.dwReserved0,
            info: RemoteInfo {
                low: item.nFileSizeLow,
                high: item.nFileSizeHigh,
                modified: item.ftLastWriteTime,
                attributes: item.dwFileAttributes as i32,
            },
        });
    }
    let first: First<u8, WIN32_FIND_DATAA> = required(library, b"FsFindFirst")?;
    let next: Next<WIN32_FIND_DATAA> = required(library, b"FsFindNext")?;
    scan(buffer(ansi(directory)?)?, first, next, close, |item| {
        Entry {
            name: ansi_text(&item.cFileName.map(|byte| byte as u8)),
            unix_mode: item.dwReserved0,
            info: RemoteInfo {
                low: item.nFileSizeLow,
                high: item.nFileSizeHigh,
                modified: item.ftLastWriteTime,
                attributes: item.dwFileAttributes as i32,
            },
        }
    })
}

fn scan<T, Data: Default>(
    mut directory: Vec<T>,
    first: First<T, Data>,
    next: Next<Data>,
    close: Close,
    read: impl Fn(&Data) -> Entry,
) -> io::Result<Vec<Entry>> {
    let mut data = Data::default();
    let raw = unsafe { first(directory.as_mut_ptr(), &mut data) };
    if raw == INVALID_HANDLE_VALUE {
        let error = unsafe { GetLastError() };
        if error == ERROR_NO_MORE_FILES {
            return Ok(Vec::new());
        }
        return Err(io::Error::other(format!(
            "WFX konnte den Ordner nicht öffnen (Status {error})"
        )));
    }
    let handle = FindHandle { raw, close };
    let mut result = Vec::new();
    for _ in 0..MAX_ROWS {
        let entry = read(&data);
        if !matches!(entry.name.as_str(), "" | "." | "..") {
            result.push(entry);
        }
        data = Data::default();
        if unsafe { next(handle.raw, &mut data) } == 0 {
            return Ok(result);
        }
    }
    Err(io::Error::other(
        "WFX-Verzeichnis überschreitet das Vorschau-Limit",
    ))
}

pub fn operate(library: &Library, plugin: &Path, operation: &WfxOperation) -> io::Result<Vec<Row>> {
    let unicode = wide_listing(library);
    initialize(library, plugin, unicode)?;
    operate_initialized(library, operation, unicode)?;
    Ok(Vec::new())
}

pub fn batch(
    library: &Library,
    plugin: &Path,
    operations: &[WfxOperation],
) -> io::Result<Vec<Row>> {
    if operations.len() > 64 {
        return Err(io::Error::other("WFX-Auftrag enthält zu viele Einträge."));
    }
    let unicode = wide_listing(library);
    initialize(library, plugin, unicode)?;
    let mut result = Vec::new();
    for (index, operation) in operations.iter().enumerate() {
        if cancelled() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        progress_message(|| format!("{}/{} Einträge", index + 1, operations.len()));
        let value = operate_initialized(library, operation, unicode)
            .err()
            .map_or_else(String::new, |error| error.to_string());
        result.push(Row {
            name: index.to_string(),
            value,
            ..Default::default()
        });
    }
    Ok(result)
}

fn operate_initialized(
    library: &Library,
    operation: &WfxOperation,
    initialized_unicode: bool,
) -> io::Result<()> {
    match operation {
        WfxOperation::Overwrite {
            operation,
            expected,
        } => {
            return overwrite_file(library, operation, *expected, initialized_unicode);
        }
        WfxOperation::DeleteTree { path } => {
            verify_delete_path(library, path, true, initialized_unicode)?;
            let plan = tree_plan(library, path, true, initialized_unicode)?;
            return delete_plan(library, path, &plan, initialized_unicode, |_| Ok(()));
        }
        WfxOperation::RemoveDownloaded {
            remote,
            local,
            tree,
        } => {
            return remove_downloaded(library, remote, local, *tree, initialized_unicode);
        }
        WfxOperation::DownloadTree { remote, local } => {
            return read_tree(
                library,
                remote,
                TreeTarget::Local(local),
                initialized_unicode,
            );
        }
        WfxOperation::CopyTree { from, to } => {
            return read_tree(library, from, TreeTarget::Remote(to), initialized_unicode);
        }
        WfxOperation::UploadTree { local, remote } => {
            return upload_tree(library, local, remote, initialized_unicode);
        }
        WfxOperation::UploadMove {
            local,
            remote,
            tree,
        } => {
            if *tree {
                upload_tree(library, local, remote, initialized_unicode)?;
            } else {
                prepare_target(library, remote, initialized_unicode)?;
                single(
                    library,
                    &WfxOperation::Upload {
                        local: local.clone(),
                        remote: remote.clone(),
                    },
                    initialized_unicode,
                    None,
                )?;
            }
            verify_remote_copy(library, remote, local, *tree, initialized_unicode).map_err(
                |error| {
                    io::Error::other(format!(
                        "WFX-Ziel geschrieben. Die Prüfung des Ziels ist fehlgeschlagen: {error}"
                    ))
                },
            )?;
            return Ok(());
        }
        WfxOperation::MoveTree { from, to } => {
            if !find_entry(library, from, initialized_unicode)?.directory() {
                return Err(io::Error::other("Der WFX-Quellordner fehlt."));
            }
            prepare_target(library, to, initialized_unicode)?;
            return single(
                library,
                &WfxOperation::Rename {
                    from: from.clone(),
                    to: to.clone(),
                },
                initialized_unicode,
                None,
            );
        }
        WfxOperation::Upload { remote, .. } | WfxOperation::Mkdir { path: remote } => {
            prepare_target(library, remote, initialized_unicode)?;
        }
        WfxOperation::Copy { from, to } | WfxOperation::Rename { from, to }
            if from.rsplit_once('\\').map(|pair| pair.0)
                != to.rsplit_once('\\').map(|pair| pair.0) =>
        {
            prepare_target(library, to, initialized_unicode)?;
        }
        _ => (),
    }
    single(library, operation, initialized_unicode, None)
}

fn expect_file_version(
    library: &Library,
    path: &str,
    version: WfxVersion,
    unicode: bool,
) -> io::Result<()> {
    if find_entry(library, path, unicode)?.version() != Some(version) {
        return Err(io::Error::other(
            "Eine WFX-Datei hat sich seit der Bestätigung geändert.",
        ));
    }
    Ok(())
}

fn overwrite_file(
    library: &Library,
    operation: &WfxOperation,
    expected: WfxVersion,
    unicode: bool,
) -> io::Result<()> {
    let target = operation
        .file_destination()
        .ok_or_else(|| io::Error::other("WFX-Zieldatei fehlt."))?;
    expect_file_version(library, target, expected, unicode)?;
    let (parent, _) = parent_and_name(target)?;
    let reservation = tempfile::Builder::new()
        .prefix("arco-wfx-")
        .tempfile_in(workspace_file(""))?;
    let name = reservation
        .path()
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| io::Error::other("WFX-Zwischenname fehlt."))?;
    let part = tree_path(parent.trim_end_matches('\\'), &format!("{name}.part"))?;
    let backup = tree_path(parent.trim_end_matches('\\'), &format!("{name}.backup"))?;
    prepare_target(library, &part, unicode)?;
    prepare_target(library, &backup, unicode)?;

    let (source_version, local) = match operation {
        WfxOperation::Upload { local, .. } | WfxOperation::UploadMove { local, .. } => {
            (None, Some(local))
        }
        WfxOperation::Copy { from, .. } | WfxOperation::Rename { from, .. }
            if !from.eq_ignore_ascii_case(target) =>
        {
            let version = find_entry(library, from, unicode)?
                .version()
                .ok_or_else(|| io::Error::other("WFX-Quelle ist keine reguläre Datei."))?;
            (Some((from, version)), None)
        }
        _ => return Err(io::Error::other("Ungültige WFX-Dateiersetzung.")),
    };
    let (stage, size) = if let Some((from, version)) = source_version {
        (
            WfxOperation::Copy {
                from: from.clone(),
                to: part.clone(),
            },
            version.size,
        )
    } else {
        let local = local.expect("local WFX replacement source");
        let metadata = files::regular_metadata(local)?;
        if !metadata.is_file() {
            return Err(io::Error::other("WFX-Quelle ist keine reguläre Datei."));
        }
        (
            WfxOperation::Upload {
                local: local.clone(),
                remote: part.clone(),
            },
            metadata.len(),
        )
    };
    let rename = |from: &str, to: &str| {
        single(
            library,
            &WfxOperation::Rename {
                from: from.into(),
                to: to.into(),
            },
            unicode,
            None,
        )
    };
    let delete = |path: &str| {
        single(
            library,
            &WfxOperation::Delete { path: path.into() },
            unicode,
            None,
        )
    };
    let staged = (|| {
        operate_initialized(library, &stage, unicode)?;
        let version = find_entry(library, &part, unicode)?
            .version()
            .filter(|version| version.size == size)
            .ok_or_else(|| {
                io::Error::other("WFX hat die Größe der Zwischendatei nicht bestätigt.")
            })?;
        if let Some((from, before)) = source_version {
            expect_file_version(library, from, before, unicode)?;
        }
        expect_file_version(library, target, expected, unicode)?;
        if cancelled() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        Ok::<_, io::Error>(version)
    })()
    .map_err(|error| io::Error::other(format!("{error}. Prüfe den WFX-Zwischenpfad „{part}“.")))?;
    rename(target, &backup).map_err(|error| {
        io::Error::other(format!(
            "{error}. Prüfe die WFX-Zwischenpfade „{part}“ und „{backup}“."
        ))
    })?;
    let publication = (|| {
        expect_file_version(library, &backup, expected, unicode)?;
        expect_file_version(library, &part, staged, unicode)?;
        prepare_target(library, target, unicode)?;
        rename(&part, target)?;
        expect_file_version(library, target, staged, unicode)
    })();
    if let Err(error) = publication {
        let recovery = prepare_target(library, target, unicode)
            .and_then(|()| rename(&backup, target))
            .and_then(|()| expect_file_version(library, target, expected, unicode));
        return Err(io::Error::other(if recovery.is_ok() {
            format!("{error}. Die WFX-Zieldatei wurde wiederhergestellt. Prüfe „{part}“.")
        } else {
            format!("{error}. Prüfe die WFX-Sicherung „{backup}“ und „{part}“.")
        }));
    }
    let finish = (|| {
        if let WfxOperation::UploadMove { local, .. } = operation {
            verify_remote_copy(library, target, local, false, unicode)?;
        } else if matches!(operation, WfxOperation::Rename { .. }) {
            let (from, version) = source_version.expect("remote WFX move source");
            expect_file_version(library, from, version, unicode)?;
            expect_file_version(library, target, staged, unicode)?;
            delete(from)?;
        }
        expect_file_version(library, &backup, expected, unicode)?;
        delete(&backup)
    })();
    finish.map_err(|error| {
        io::Error::other(format!(
            "{error}. Das WFX-Ziel wurde geschrieben. Prüfe die Sicherung „{backup}“."
        ))
    })
}

fn single(
    library: &Library,
    operation: &WfxOperation,
    initialized_unicode: bool,
    known: Option<RemoteInfo>,
) -> io::Result<()> {
    let wide_symbol = match operation {
        WfxOperation::Download { .. } => b"FsGetFileW".as_slice(),
        WfxOperation::Upload { .. } => b"FsPutFileW".as_slice(),
        WfxOperation::Copy { .. } | WfxOperation::Rename { .. } => b"FsRenMovFileW".as_slice(),
        WfxOperation::Mkdir { .. } => b"FsMkDirW".as_slice(),
        WfxOperation::Delete { .. } => b"FsDeleteFileW".as_slice(),
        WfxOperation::RemoveDir { .. } => b"FsRemoveDirW".as_slice(),
        _ => return Err(io::Error::other("Ungültige WFX-Einzeloperation.")),
    };
    let unicode =
        initialized_unicode && symbol::<*const std::ffi::c_void>(library, wide_symbol).is_some();
    if unicode {
        perform(
            library,
            operation,
            true,
            initialized_unicode,
            known,
            |value| buffer(wide(value)),
        )?;
    } else {
        if initialized_unicode && symbol::<Init<u8>>(library, b"FsInit").is_none() {
            return Err(io::Error::other(
                "Das Plugin stellt diese WFX-Operation nicht in Unicode bereit.",
            ));
        }
        perform(
            library,
            operation,
            false,
            initialized_unicode,
            known,
            |value| buffer(ansi(value)?),
        )?;
    }
    Ok(())
}

fn perform<T>(
    library: &Library,
    operation: &WfxOperation,
    unicode: bool,
    listing_unicode: bool,
    known: Option<RemoteInfo>,
    encode: impl Fn(&OsStr) -> io::Result<Vec<T>>,
) -> io::Result<()> {
    type Get<T> = unsafe extern "system" fn(*mut T, *mut T, i32, *mut RemoteInfo) -> i32;
    type Put<T> = unsafe extern "system" fn(*mut T, *mut T, i32) -> i32;
    type Rename<T> = unsafe extern "system" fn(*mut T, *mut T, i32, i32, *mut RemoteInfo) -> i32;
    type PathOp<T> = unsafe extern "system" fn(*mut T) -> i32;
    let remote_name = |name: &str| encode(OsStr::new(name));
    let info = |path| match known {
        Some(info) => Ok(info),
        None => find_entry(library, path, listing_unicode).map(|entry| entry.info),
    };
    let (label, status) = match operation {
        WfxOperation::Download { remote, local } => {
            let function: Get<T> = function(library, unicode, b"FsGetFile", b"FsGetFileW")?;
            let mut info = info(remote.as_str())?;
            if info.attributes & 0x10 != 0 {
                return Err(io::Error::other(
                    "Wähle eine einzelne Datei zum Herunterladen.",
                ));
            }
            let mut from = remote_name(remote)?;
            let mut to = encode(&sdk_path(local))?;
            ("FsGetFile", unsafe {
                function(from.as_mut_ptr(), to.as_mut_ptr(), 0, &mut info)
            })
        }
        WfxOperation::Upload { local, remote } => {
            let function: Put<T> = function(library, unicode, b"FsPutFile", b"FsPutFileW")?;
            let mut from = encode(&sdk_path(local))?;
            let mut to = remote_name(remote)?;
            ("FsPutFile", unsafe {
                function(from.as_mut_ptr(), to.as_mut_ptr(), 0)
            })
        }
        WfxOperation::Copy { from, to } | WfxOperation::Rename { from, to } => {
            let function: Rename<T> =
                function(library, unicode, b"FsRenMovFile", b"FsRenMovFileW")?;
            let mut info = info(from.as_str())?;
            let moving = matches!(operation, WfxOperation::Rename { .. });
            if info.attributes & 0x10 != 0 {
                if !moving {
                    return Err(io::Error::other("Wähle einzelne Dateien zum Kopieren."));
                }
                info.low = 0;
                info.high = u32::MAX;
            }
            let mut from = remote_name(from)?;
            let mut to = remote_name(to)?;
            ("FsRenMovFile", unsafe {
                function(
                    from.as_mut_ptr(),
                    to.as_mut_ptr(),
                    i32::from(moving),
                    0,
                    &mut info,
                )
            })
        }
        WfxOperation::Mkdir { path } => {
            let function: PathOp<T> = function(library, unicode, b"FsMkDir", b"FsMkDirW")?;
            let status = unsafe { function(remote_name(path)?.as_mut_ptr()) };
            return bool_result("FsMkDir", status);
        }
        WfxOperation::Delete { path } => {
            let function: PathOp<T> =
                function(library, unicode, b"FsDeleteFile", b"FsDeleteFileW")?;
            if info(path.as_str())?.attributes & 0x10 != 0 {
                return Err(io::Error::other("Dieser Befehl löscht einzelne Dateien."));
            }
            let status = unsafe { function(remote_name(path)?.as_mut_ptr()) };
            return bool_result("FsDeleteFile", status);
        }
        WfxOperation::RemoveDir { path } => {
            let function: PathOp<T> = function(library, unicode, b"FsRemoveDir", b"FsRemoveDirW")?;
            if info(path.as_str())?.attributes & 0x10 == 0
                || !entries(library, OsStr::new(path), listing_unicode)?.is_empty()
            {
                return Err(io::Error::other(
                    "Nur leere Plugin-Ordner können gelöscht werden.",
                ));
            }
            let status = unsafe { function(remote_name(path)?.as_mut_ptr()) };
            return bool_result("FsRemoveDir", status);
        }
        _ => return Err(io::Error::other("Ungültige WFX-Einzeloperation.")),
    };
    if status == 0 {
        return Ok(());
    }
    let reason = match status {
        1 | 7 => "Zieldatei existiert bereits",
        2 => "Quelldatei fehlt",
        3 => "Lesefehler",
        4 => "Schreibfehler",
        5 => "Transfer abgebrochen",
        6 => "Operation wird vom Plugin nicht unterstützt",
        _ => "unbekannter Plugin-Status",
    };
    Err(io::Error::other(format!(
        "WFX {label}: {reason} ({status})."
    )))
}

fn prepare_target(library: &Library, path: &str, unicode: bool) -> io::Result<()> {
    let (parent, name) = parent_and_name(path)?;
    // Some plugins establish a connection only through directory enumeration.
    if entries(library, OsStr::new(parent), unicode)?
        .iter()
        .any(|entry| entry.name == name)
    {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "WFX-Ziel existiert bereits.",
        ));
    }
    Ok(())
}

enum TreeTarget<'a> {
    Local(&'a Path),
    Remote(&'a str),
}

struct TreeEntry {
    relative: String,
    depth: usize,
    info: RemoteInfo,
}

impl TreeEntry {
    fn directory(&self) -> bool {
        self.info.attributes & 0x10 != 0
    }
}

fn tree_plan(
    library: &Library,
    root: &str,
    tree: bool,
    unicode: bool,
) -> io::Result<Vec<TreeEntry>> {
    let root_entry = find_entry(library, root, unicode)?;
    if root_entry.directory() != tree || root_entry.link() {
        return Err(io::Error::other(
            "Der WFX-Quelltyp hat sich geändert oder ist eine Verknüpfung.",
        ));
    }
    let mut plan = vec![TreeEntry {
        relative: String::new(),
        depth: 0,
        info: root_entry.info,
    }];
    let mut index = 0;
    while let Some(entry) = plan.get(index) {
        let TreeEntry {
            relative, depth, ..
        } = entry;
        check_tree_item(index, *depth)?;
        if entry.directory() {
            let directory = entries(library, OsStr::new(&tree_path(root, relative)?), unicode)?;
            let mut names = std::collections::HashSet::new();
            let mut children = Vec::new();
            for entry in directory {
                if entry.link() {
                    return Err(io::Error::other(format!(
                        "WFX-Verknüpfung im Quellordner: {}",
                        entry.name
                    )));
                }
                files::leaf_name(OsStr::new(&entry.name))?;
                if !names.insert(entry.name.to_lowercase()) {
                    return Err(io::Error::other(format!(
                        "Mehrdeutiger WFX-Name: {}",
                        entry.name
                    )));
                }
                let child = if relative.is_empty() {
                    entry.name
                } else {
                    format!("{relative}\\{}", entry.name)
                };
                check_tree_item(plan.len() + children.len(), depth + 1)?;
                children.push(TreeEntry {
                    relative: child,
                    depth: depth + 1,
                    info: entry.info,
                });
            }
            plan.extend(children);
        }
        index += 1;
    }
    Ok(plan)
}

fn read_tree(
    library: &Library,
    root: &str,
    target: TreeTarget<'_>,
    unicode: bool,
) -> io::Result<()> {
    // Plan the entire source before creating a target that might alias the source.
    let plan = tree_plan(library, root, true, unicode)?;
    let staging = if let TreeTarget::Remote(root) = target {
        prepare_target(library, root, unicode)?;
        Some(tempfile::tempdir_in(workspace_file(""))?)
    } else {
        None
    };
    for (index, entry) in plan.into_iter().enumerate() {
        let directory = entry.directory();
        let TreeEntry {
            relative,
            depth,
            info,
        } = entry;
        check_tree_item(index, depth)?;
        let from = tree_path(root, &relative)?;
        progress_message(|| format!("WFX · {from}"));
        let result = match target {
            TreeTarget::Local(root) if directory => fs::create_dir(root.join(&relative)),
            TreeTarget::Local(root) => {
                let local = root.join(&relative);
                download(library, &from, &local, unicode, info)
            }
            TreeTarget::Remote(root) if directory => single(
                library,
                &WfxOperation::Mkdir {
                    path: tree_path(root, &relative)?,
                },
                unicode,
                None,
            ),
            TreeTarget::Remote(root) => {
                let name = Path::new(&relative)
                    .file_name()
                    .ok_or(io::ErrorKind::InvalidInput)?;
                let local = staging
                    .as_ref()
                    .expect("remote tree staging")
                    .path()
                    .join(name);
                let to = tree_path(root, &relative)?;
                download(library, &from, &local, unicode, info)
                    .and_then(|()| {
                        single(
                            library,
                            &WfxOperation::Upload {
                                local: local.clone(),
                                remote: to,
                            },
                            unicode,
                            None,
                        )
                    })
                    .and_then(|()| fs::remove_file(local))
            }
        };
        result.map_err(|error| io::Error::other(format!("{from}: {error}")))?;
    }
    Ok(())
}

fn verify_delete_path(library: &Library, root: &str, tree: bool, unicode: bool) -> io::Result<()> {
    let (mut parent, _) = parent_and_name(root)?;
    if tree && parent == "\\" {
        return Err(io::Error::other(
            "WFX-Verbindungen im Stamm können nicht rekursiv gelöscht werden.",
        ));
    }
    while parent != "\\" {
        let (ancestor, _) = parent_and_name(parent)?;
        // Plugins can expose connection aliases as links directly below the virtual root.
        if ancestor == "\\" {
            break;
        }
        let entry = find_entry(library, parent, unicode)?;
        if !entry.directory() || entry.link() {
            return Err(io::Error::other(format!(
                "WFX-Pfad führt über eine Verknüpfung: {parent}"
            )));
        }
        parent = ancestor;
    }
    Ok(())
}

fn delete_plan(
    library: &Library,
    root: &str,
    plan: &[TreeEntry],
    unicode: bool,
    mut verify_target: impl FnMut(&str) -> io::Result<()>,
) -> io::Result<()> {
    for (directory, wide, ansi) in [
        (
            false,
            b"FsDeleteFileW".as_slice(),
            b"FsDeleteFile".as_slice(),
        ),
        (true, b"FsRemoveDirW".as_slice(), b"FsRemoveDir".as_slice()),
    ] {
        if plan.iter().any(|entry| entry.directory() == directory)
            && !(unicode && symbol::<*const std::ffi::c_void>(library, wide).is_some()
                || symbol::<*const std::ffi::c_void>(library, ansi).is_some())
        {
            return Err(io::Error::other(
                "Das Plugin unterstützt die benötigte Löschoperation nicht.",
            ));
        }
    }
    let mut listing_parent = String::new();
    let mut listing = std::collections::HashMap::new();
    for (index, entry) in plan.iter().rev().enumerate() {
        check_tree_item(index, entry.depth)?;
        let path = tree_path(root, &entry.relative)?;
        progress_message(|| format!("WFX · Lösche {path}"));
        let result = (|| {
            let (parent, name) = parent_and_name(&path)?;
            if listing_parent != parent {
                verify_delete_path(library, &path, false, unicode)?;
                listing_parent = parent.into();
                listing = entries(library, OsStr::new(parent), unicode)?
                    .into_iter()
                    .map(|entry| (entry.name.clone(), entry))
                    .collect();
            }
            let current = listing.get(name).ok_or(io::ErrorKind::NotFound)?;
            if current.link()
                || current.directory() != entry.directory()
                || (!entry.directory() && current.info.fingerprint() != entry.info.fingerprint())
            {
                return Err(io::Error::other("Der WFX-Eintrag hat sich geändert."));
            }
            verify_target(&entry.relative)?;
            let operation = if entry.directory() {
                WfxOperation::RemoveDir { path: path.clone() }
            } else {
                WfxOperation::Delete { path: path.clone() }
            };
            single(library, &operation, unicode, Some(current.info))
        })();
        result.map_err(|error| {
            io::Error::other(format!(
                "{} von {} Einträgen gelöscht. Fehler bei {path}: {error}",
                index,
                plan.len()
            ))
        })?;
    }
    Ok(())
}

fn remove_downloaded(
    library: &Library,
    remote: &str,
    local: &Path,
    tree: bool,
    unicode: bool,
) -> io::Result<()> {
    verify_delete_path(library, remote, tree, unicode)?;
    let verified = verify_remote_copy(library, remote, local, tree, unicode)?;
    verify_delete_path(library, remote, tree, unicode)?;
    delete_plan(library, remote, &verified.entries, unicode, |relative| {
        let (path, metadata) = verified
            .local
            .get(relative)
            .ok_or(io::ErrorKind::InvalidData)?;
        files::ensure_unchanged(path, metadata)
    })
}

struct VerifiedCopy {
    entries: Vec<TreeEntry>,
    local: std::collections::HashMap<String, (std::path::PathBuf, fs::Metadata)>,
}

fn verify_remote_copy(
    library: &Library,
    remote: &str,
    local: &Path,
    tree: bool,
    unicode: bool,
) -> io::Result<VerifiedCopy> {
    let plan = tree_plan(library, remote, tree, unicode)?;
    let staging = tempfile::tempdir_in(workspace_file(""))?;
    let local_path = |relative: &str| {
        if relative.is_empty() {
            local.to_path_buf()
        } else {
            local.join(relative)
        }
    };
    let mut targets = std::collections::HashMap::new();
    for (index, entry) in plan.iter().enumerate() {
        check_tree_item(index, entry.depth)?;
        let path = local_path(&entry.relative);
        let metadata = files::regular_metadata(&path)?;
        if metadata.is_dir() != entry.directory() {
            return Err(io::Error::other(format!(
                "Lokales Ziel geändert: {}",
                path.display()
            )));
        }
        if !entry.directory() {
            let source = tree_path(remote, &entry.relative)?;
            let name = path.file_name().ok_or(io::ErrorKind::InvalidInput)?;
            let copy = staging.path().join(name);
            progress_message(|| format!("WFX · Prüfe {source}"));
            download(library, &source, &copy, unicode, entry.info)?;
            let equal = tools::contents_match(&path, &copy, cancelled)?;
            fs::remove_file(copy)?;
            files::ensure_unchanged(&path, &metadata)?;
            if !equal {
                return Err(io::Error::other(format!(
                    "WFX-Quelle stimmt nicht mit dem lokalen Ziel überein: {source}"
                )));
            }
        }
        targets.insert(entry.relative.clone(), (path, metadata));
    }
    let previous: std::collections::HashMap<_, _> = plan
        .iter()
        .map(|entry| (entry.relative.as_str(), entry.info.fingerprint()))
        .collect();
    let current = tree_plan(library, remote, tree, unicode)?;
    if current.len() != plan.len()
        || current
            .iter()
            .any(|entry| previous.get(entry.relative.as_str()) != Some(&entry.info.fingerprint()))
    {
        return Err(io::Error::other(
            "WFX-Quelle hat sich während der Prüfung geändert.",
        ));
    }
    let local_paths: std::collections::HashSet<_> =
        targets.values().map(|(path, _)| path.as_path()).collect();
    for (path, metadata) in targets.values() {
        files::ensure_unchanged(path, metadata)?;
        if metadata.is_dir() {
            for child in fs::read_dir(path)? {
                if cancelled() {
                    return Err(io::ErrorKind::Interrupted.into());
                }
                let child = child?.path();
                if !local_paths.contains(child.as_path()) {
                    return Err(io::Error::other(format!(
                        "Eintrag fehlt im WFX-Abbild: {}",
                        child.display()
                    )));
                }
            }
        }
    }
    Ok(VerifiedCopy {
        entries: current,
        local: targets,
    })
}

fn download(
    library: &Library,
    remote: &str,
    local: &Path,
    unicode: bool,
    info: RemoteInfo,
) -> io::Result<()> {
    let operation = WfxOperation::Download {
        remote: remote.into(),
        local: local.into(),
    };
    single(library, &operation, unicode, Some(info))?;
    if !files::regular_metadata(local)?.is_file() {
        return Err(io::Error::other(
            "Das Plugin hat keine reguläre Datei geliefert.",
        ));
    }
    Ok(())
}

fn upload_tree(library: &Library, local: &Path, remote: &str, unicode: bool) -> io::Result<()> {
    prepare_target(library, remote, unicode)?;
    let mut pending = vec![(local.to_path_buf(), String::new(), 0)];
    let mut processed = 0;
    while let Some((path, relative, depth)) = pending.pop() {
        check_tree_item(processed, depth)?;
        processed += 1;
        let target = tree_path(remote, &relative)?;
        progress_message(|| format!("WFX · {target}"));
        let operation = if files::regular_metadata(&path)?.is_dir() {
            for entry in fs::read_dir(&path)? {
                let entry = entry?;
                let name: OsString = entry.file_name();
                let name = files::leaf_name(&name)?.to_str().ok_or_else(|| {
                    io::Error::other("Der WFX-Dateiname ist kein gültiger Unicode-Text.")
                })?;
                let child = if relative.is_empty() {
                    name.into()
                } else {
                    format!("{relative}\\{name}")
                };
                check_tree_item(processed + pending.len(), depth + 1)?;
                pending.push((entry.path(), child, depth + 1));
            }
            WfxOperation::Mkdir { path: target }
        } else {
            WfxOperation::Upload {
                local: path,
                remote: target,
            }
        };
        single(library, &operation, unicode, None)
            .map_err(|error| io::Error::other(format!("{relative}: {error}")))?;
    }
    Ok(())
}

fn check_tree_item(index: usize, depth: usize) -> io::Result<()> {
    if cancelled() {
        return Err(io::ErrorKind::Interrupted.into());
    }
    if index >= WFX_TREE_ENTRIES || depth > WFX_TREE_DEPTH {
        return Err(io::Error::other(
            "WFX-Ordner überschreitet das Transferlimit.",
        ));
    }
    Ok(())
}

fn tree_path(root: &str, relative: &str) -> io::Result<String> {
    let path = if relative.is_empty() {
        root.to_owned()
    } else {
        format!("{root}\\{relative}")
    };
    if path.encode_utf16().count() > 1023 {
        return Err(io::Error::other("WFX-Pfad überschreitet das Pfadlimit."));
    }
    Ok(path)
}

fn parent_and_name(path: &str) -> io::Result<(&str, &str)> {
    let (parent, name) = path
        .rsplit_once('\\')
        .filter(|(_, name)| !name.is_empty())
        .ok_or_else(|| io::Error::other("Ungültiger Plugin-Pfad."))?;
    Ok((if parent.is_empty() { "\\" } else { parent }, name))
}

fn find_entry(library: &Library, path: &str, unicode: bool) -> io::Result<Entry> {
    let (parent, name) = parent_and_name(path)?;
    entries(library, OsStr::new(parent), unicode)?
        .into_iter()
        .find(|entry| entry.name == name)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "Eintrag im Plugin-Dateisystem fehlt.",
            )
        })
}

fn function<T: Copy>(library: &Library, unicode: bool, ansi: &[u8], wide: &[u8]) -> io::Result<T> {
    required(library, if unicode { wide } else { ansi })
}

fn bool_result(function: &str, status: i32) -> io::Result<()> {
    if status == 0 {
        return Err(io::Error::other(format!(
            "WFX {function} ist fehlgeschlagen."
        )));
    }
    Ok(())
}

fn buffer<T: Copy + Default>(mut name: Vec<T>) -> io::Result<Vec<T>> {
    if name.len() > 1024 {
        return Err(io::Error::other("Der Pfad ist für den WFX-Puffer zu lang."));
    }
    name.resize(1024, T::default());
    Ok(name)
}

struct FindHandle {
    raw: Handle,
    close: Close,
}

impl Drop for FindHandle {
    fn drop(&mut self) {
        unsafe { (self.close)(self.raw) };
    }
}

unsafe extern "system" fn progress<T>(_: i32, _: *mut T, _: *mut T, percent: i32) -> i32 {
    progress_message(|| format!("{} %", percent.clamp(0, 100)));
    i32::from(cancelled())
}
unsafe extern "system" fn log<T>(_: i32, _: i32, _: *mut T) {}
static INTERACTIVE: AtomicBool = AtomicBool::new(false);
static PROMPT_LOCK: Mutex<()> = Mutex::new(());

pub fn set_interactive(enabled: bool) {
    INTERACTIVE.store(enabled, Ordering::Relaxed);
}

trait PromptText: Copy + Default + PartialEq + Zeroize {
    fn decode(units: &[Self]) -> String;
    fn encode(value: &str) -> io::Result<Vec<Self>>;
}

impl PromptText for u8 {
    fn decode(units: &[Self]) -> String {
        ansi_text(units)
    }
    fn encode(value: &str) -> io::Result<Vec<Self>> {
        ansi(OsStr::new(value))
    }
}

impl PromptText for u16 {
    fn decode(units: &[Self]) -> String {
        wide_text(units)
    }
    fn encode(value: &str) -> io::Result<Vec<Self>> {
        Ok(wide(OsStr::new(value)))
    }
}

unsafe fn read_prompt<T: PromptText>(text: *const T) -> String {
    if text.is_null() {
        return String::new();
    }
    let mut units = Vec::new();
    for index in 0..2048 {
        // The SDK supplies a readable NUL-terminated string.
        let unit = unsafe { *text.add(index) };
        if unit == T::default() {
            break;
        }
        units.push(unit);
    }
    T::decode(&units)
}

unsafe extern "system" fn request<T: PromptText>(
    _: i32,
    kind: i32,
    title: *mut T,
    text: *mut T,
    output: *mut T,
    max: i32,
) -> i32 {
    if !INTERACTIVE.load(Ordering::Relaxed) || cancelled() {
        return 0;
    }
    let kind = match kind {
        2 | 5 => PromptKind::Secret,
        0 | 1 | 3 | 4 | 6 | 7 => PromptKind::Text,
        8 => PromptKind::Acknowledge,
        9 | 10 => PromptKind::Confirm,
        _ => return 0,
    };
    let ask = Prompt {
        kind,
        title: unsafe { read_prompt(title) },
        text: unsafe { read_prompt(text) },
    };
    let Some(answer) = read_answer(ask).ok().flatten() else {
        return 0;
    };
    if matches!(kind, PromptKind::Confirm | PromptKind::Acknowledge) {
        return 1;
    }
    if output.is_null() || max <= 0 || answer.value.contains('\0') {
        return 0;
    }
    let Ok(units) = T::encode(&answer.value) else {
        return 0;
    };
    let units = Zeroizing::new(units);
    if units.len() > max as usize {
        return 0;
    }
    // The callback buffer is owned by the plugin and has at least max code units.
    unsafe { std::ptr::copy_nonoverlapping(units.as_ptr(), output, units.len()) };
    1
}

fn read_answer(prompt: Prompt) -> io::Result<Option<PromptAnswer>> {
    let _guard = PROMPT_LOCK
        .lock()
        .map_err(|_| io::Error::other("WFX-Abfrage nicht verfügbar."))?;
    {
        let mut stdout = io::stdout().lock();
        serde_json::to_writer(
            &mut stdout,
            &PromptMessage {
                version: PROTOCOL_VERSION,
                prompt,
            },
        )
        .map_err(io::Error::other)?;
        stdout.write_all(b"\n")?;
        stdout.flush()?;
    }
    let mut bytes = Zeroizing::new(Vec::new());
    let read = io::stdin()
        .lock()
        .take(8193)
        .read_until(b'\n', &mut bytes)?;
    if read > 8192 || bytes.last() != Some(&b'\n') {
        return Ok(None);
    }
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}
