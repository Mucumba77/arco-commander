use super::{
    GetValue, Library, NativeWdxField, ansi, cancelled, format_file_time, progress_message,
    sdk_path, symbol, wdx_fields, wdx_value, wide,
};
use arco_core::{
    files::regular_metadata,
    plugin::{Row, WdxEditableField, WdxField},
};
use std::ffi::{OsStr, c_void};
use std::io;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use windows_sys::Win32::{
    Foundation::{FILETIME, HWND, RECT, SYSTEMTIME},
    System::Time::SystemTimeToFileTime,
    UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, GetWindowRect, WS_POPUP},
};

type Flags = unsafe extern "system" fn(i32) -> i32;
type Set = unsafe extern "system" fn(*mut c_void, i32, i32, i32, *mut c_void, i32) -> i32;
type Editor = unsafe extern "system" fn(HWND, i32, i32, i32, *mut c_void, i32, i32, *mut u8) -> i32;
const EDIT: i32 = 1;
const CUSTOM_EDITOR: i32 = 16;
const ONLY_ATTRIBUTE: i32 = 1 | 2;
const INITIALIZE: i32 = 1;

pub(super) enum Input<'a> {
    Text(&'a str),
    Editor(usize),
}

fn setter(library: &Library) -> Option<(Set, bool)> {
    symbol::<Set>(library, b"ContentSetValueW")
        .map(|set| (set, true))
        .or_else(|| symbol::<Set>(library, b"ContentSetValue").map(|set| (set, false)))
}

fn writable(library: &Library) -> io::Result<Vec<(NativeWdxField, i32)>> {
    let Some(flags) = symbol::<Flags>(library, b"ContentGetSupportedFieldFlags") else {
        return Ok(Vec::new());
    };
    if setter(library).is_none() {
        return Ok(Vec::new());
    }
    let supported = unsafe { flags(-1) };
    if supported < 0 || supported & EDIT == 0 {
        return Ok(Vec::new());
    }
    Ok(wdx_fields(library)?
        .into_iter()
        .filter_map(|field| {
            let flags = unsafe { flags(field.index) };
            (flags >= 0 && flags & EDIT != 0).then_some((field, flags))
        })
        .collect())
}

fn custom_editor(library: &Library, field: &NativeWdxField, flags: i32) -> Option<Editor> {
    if flags & CUSTOM_EDITOR != 0 && matches!(field.kind, 1..=5 | 8 | 10) {
        symbol::<Editor>(library, b"ContentEditValue")
    } else {
        None
    }
}

pub(super) fn editable_fields(library: &Library) -> io::Result<Vec<Row>> {
    let mut rows = Vec::new();
    for (field, flags) in writable(library)? {
        let editor = custom_editor(library, &field, flags).is_some();
        let choices: Vec<_> = if field.kind == 7 {
            field
                .units_or_choices
                .split('|')
                .map(str::to_owned)
                .collect()
        } else {
            Vec::new()
        };
        let units = field.units();
        let units: Vec<_> = if units.is_empty() {
            vec![None]
        } else {
            units.split('|').map(|unit| Some(unit.to_owned())).collect()
        };
        for unit in units {
            let editable = WdxEditableField {
                selection: WdxField {
                    field: field.name.clone(),
                    unit,
                },
                kind: field.kind,
                choices: choices.clone(),
                editor,
            };
            rows.push(Row {
                name: editable.selection.label(),
                value: serde_json::to_string(&editable).map_err(io::Error::other)?,
                ..Default::default()
            });
        }
    }
    Ok(rows)
}

pub(super) fn set_values(
    library: &Library,
    selection: &WdxField,
    input: Input<'_>,
    files: &[PathBuf],
) -> io::Result<Vec<Row>> {
    if files.is_empty() || files.len() > 128 || files.iter().any(|path| !path.is_absolute()) {
        return Err(io::Error::other("Ungültige WDX-Dateiliste."));
    }
    let (field, flags) = writable(library)?
        .into_iter()
        .find(|(field, _)| field.name == selection.field)
        .ok_or_else(|| io::Error::other("Das WDX-Feld ist nicht beschreibbar."))?;
    let unit = field.unit_index(selection.unit.as_deref())?;
    let (set, unicode) = setter(library).expect("writable verifies the setter");
    if cancelled() {
        return Err(io::ErrorKind::Interrupted.into());
    }
    let parsed = match input {
        Input::Text(text) => encode(&field, text, unicode)?,
        Input::Editor(parent) => {
            let edit = custom_editor(library, &field, flags)
                .ok_or_else(|| io::Error::other("Das WDX-Feld bietet keinen eigenen Editor an."))?;
            let Some(value) = edit_value(library, edit, &field, unit, &files[0], parent, unicode)?
            else {
                return Ok(vec![Row {
                    name: field.name,
                    value: "Plugin-Editor abgebrochen. Keine Werte geschrieben.".into(),
                    ..Default::default()
                }]);
            };
            value
        }
    };
    let mut rows = Vec::with_capacity(files.len());
    let mut stop = false;
    let mut called = false;
    for (index, path) in files.iter().enumerate() {
        progress_message(|| format!("Ändere WDX-Feld · {} / {}", index + 1, files.len()));
        let result = if stop || cancelled() {
            "Übersprungen (abgebrochen)".into()
        } else {
            let result = regular_metadata(path).and_then(|metadata| {
                if !metadata.is_file() {
                    return Err(io::Error::other(
                        "WDX-Feldänderung erwartet eine reguläre Datei.",
                    ));
                }
                let name = sdk_path(path);
                let mut data = parsed.clone();
                let mut call = |name| {
                    called = true;
                    unsafe {
                        set(
                            name,
                            field.index,
                            unit,
                            data.kind(),
                            data.as_mut_ptr(),
                            ONLY_ATTRIBUTE,
                        )
                    }
                };
                if unicode {
                    Ok(call(wide(&name).as_mut_ptr().cast()))
                } else {
                    Ok(call(ansi(&name)?.as_mut_ptr().cast()))
                }
            });
            match result {
                Ok(0) => "Geändert".into(),
                Ok(-6) => {
                    stop = true;
                    "Plugin hat abgebrochen".into()
                }
                Ok(code) => format!("Plugin meldet Fehler {code}"),
                Err(error) => error.to_string(),
            }
        };
        rows.push(Row {
            name: path.display().to_string(),
            value: result,
            ..Default::default()
        });
    }
    if called {
        // The SDK allows a plugin to defer writes until this end-of-operation call.
        let code = unsafe { set(null_mut(), -1, 0, 0, null_mut(), 0) };
        if code != 0 {
            rows.push(Row {
                name: "Abschluss der WDX-Änderung".into(),
                value: format!("Plugin meldet Status {code}"),
                ..Default::default()
            });
        }
    }
    Ok(rows)
}

struct EditorOwner(HWND);

impl EditorOwner {
    fn new(parent: usize) -> io::Result<Self> {
        let mut bounds = RECT::default();
        let parent = parent as HWND;
        if parent.is_null() {
            return Ok(Self(null_mut()));
        }
        if unsafe { GetWindowRect(parent, &mut bounds) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // Keep a separate owner on the plugin thread. A modal dialog can disable
        // it without disabling the main window or leaving it disabled on cancellation.
        let class = wide(OsStr::new("STATIC"));
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                null(),
                WS_POPUP,
                bounds.left,
                bounds.top,
                bounds.right - bounds.left,
                bounds.bottom - bounds.top,
                parent,
                null_mut(),
                null_mut(),
                null(),
            )
        };
        if hwnd.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(hwnd))
        }
    }
}

impl Drop for EditorOwner {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { DestroyWindow(self.0) };
        }
    }
}

fn edit_value(
    library: &Library,
    edit: Editor,
    field: &NativeWdxField,
    unit: i32,
    first: &Path,
    parent: usize,
    unicode_setter: bool,
) -> io::Result<Option<Value>> {
    let metadata = regular_metadata(first)?;
    if !metadata.is_file() {
        return Err(io::Error::other(
            "WDX-Feldänderung erwartet eine reguläre Datei.",
        ));
    }
    let mut data = [0u64; 512];
    let bytes = std::mem::size_of_val(&data) as i32;
    let name = sdk_path(first);
    let pointer = data.as_mut_ptr().cast::<c_void>();
    let code = if let Some(get) = symbol::<GetValue>(library, b"ContentGetValueW") {
        unsafe {
            get(
                wide(&name).as_mut_ptr().cast(),
                field.index,
                unit,
                pointer,
                bytes,
                0,
            )
        }
    } else if let Some(get) = symbol::<GetValue>(library, b"ContentGetValue") {
        match ansi(&name) {
            Ok(mut name) => unsafe {
                get(
                    name.as_mut_ptr().cast(),
                    field.index,
                    unit,
                    pointer,
                    bytes,
                    0,
                )
            },
            Err(_) => -3,
        }
    } else {
        -3
    };
    let initialize = if code == field.kind {
        true
    } else if field.kind == 8 && code == 11 {
        let encoded = wdx_value(code, &data)
            .and_then(|value| ansi(OsStr::new(&value)).ok())
            .filter(|value| value.len() <= bytes as usize);
        data.fill(0);
        if let Some(encoded) = encoded {
            // The plugin buffer is an aligned array of initialized bytes.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    encoded.as_ptr(),
                    data.as_mut_ptr().cast(),
                    encoded.len(),
                )
            };
            true
        } else {
            false
        }
    } else {
        data.fill(0);
        false
    };
    let owner = EditorOwner::new(parent)?;
    let mut language = *b"deu\0";
    progress_message(|| "Plugin-Editor geöffnet …".into());
    let flags = if initialize { INITIALIZE } else { 0 };
    match unsafe {
        edit(
            owner.0,
            field.index,
            unit,
            field.kind,
            data.as_mut_ptr().cast(),
            bytes,
            flags,
            language.as_mut_ptr(),
        )
    } {
        0 => {
            if field.kind == 10 {
                let stamp = FILETIME {
                    dwLowDateTime: data[0] as u32,
                    dwHighDateTime: (data[0] >> 32) as u32,
                };
                format_file_time(stamp)
                    .ok_or_else(|| io::Error::other("Ungültiger WDX-Zeitstempel."))?;
                return Ok(Some(Value::Timestamp(stamp)));
            }
            if field.kind == 8 && !data.iter().any(|word| word.to_le_bytes().contains(&0)) {
                return Err(io::Error::other(
                    "WDX-Editor meldet eine nicht abgeschlossene Zeichenfolge.",
                ));
            }
            let text = wdx_value(field.kind, &data)
                .ok_or_else(|| io::Error::other("WDX-Editor meldet einen ungültigen Wert."))?;
            encode(field, &text, unicode_setter).map(Some)
        }
        -6 => Ok(None),
        status => Err(io::Error::other(format!(
            "WDX-Editor meldet Fehler {status}."
        ))),
    }
}

#[derive(Clone)]
enum Value {
    Int(i32, i32),
    Long(i64),
    Float(f64),
    Parts(i32, [u16; 3]),
    Ansi(i32, Vec<u8>),
    Wide(Vec<u16>),
    Timestamp(FILETIME),
}

impl Value {
    fn kind(&self) -> i32 {
        match self {
            Self::Int(kind, _) | Self::Parts(kind, _) | Self::Ansi(kind, _) => *kind,
            Self::Long(_) => 2,
            Self::Float(_) => 3,
            Self::Timestamp(_) => 10,
            Self::Wide(_) => 11,
        }
    }

    fn as_mut_ptr(&mut self) -> *mut c_void {
        match self {
            Self::Int(_, value) => (value as *mut i32).cast(),
            Self::Long(value) => (value as *mut i64).cast(),
            Self::Float(value) => (value as *mut f64).cast(),
            Self::Parts(_, value) => value.as_mut_ptr().cast(),
            Self::Ansi(_, value) => value.as_mut_ptr().cast(),
            Self::Wide(value) => value.as_mut_ptr().cast(),
            Self::Timestamp(value) => (value as *mut FILETIME).cast(),
        }
    }
}

fn encode(field: &NativeWdxField, input: &str, unicode: bool) -> io::Result<Value> {
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Ungültiger Wert für das WDX-Feld.",
        )
    };
    if input.contains('\0') || input.len() > 4096 {
        return Err(invalid());
    }
    let trimmed = input.trim();
    Ok(match field.kind {
        1 => Value::Int(1, trimmed.parse().map_err(|_| invalid())?),
        2 => Value::Long(trimmed.parse().map_err(|_| invalid())?),
        3 => Value::Float(
            trimmed
                .replace(',', ".")
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite())
                .ok_or_else(invalid)?,
        ),
        4 => {
            let date = parts(trimmed, '-', [4, 2, 2]).ok_or_else(invalid)?;
            timestamp(date, [0; 3]).ok_or_else(invalid)?;
            Value::Parts(4, date)
        }
        5 => {
            let time = parts(trimmed, ':', [2; 3])
                .filter(|[h, m, s]| *h < 24 && *m < 60 && *s < 60)
                .ok_or_else(invalid)?;
            Value::Parts(5, time)
        }
        6 => Value::Int(
            6,
            match trimmed.to_ascii_lowercase().as_str() {
                "ja" | "true" | "1" => 1,
                "nein" | "false" | "0" => 0,
                _ => return Err(invalid()),
            },
        ),
        7 if field
            .units_or_choices
            .split('|')
            .any(|choice| choice == input) =>
        {
            Value::Ansi(7, ansi(OsStr::new(input))?)
        }
        8 | 11 if unicode => Value::Wide(wide(OsStr::new(input))),
        8 => Value::Ansi(8, ansi(OsStr::new(input))?),
        10 => {
            let trimmed = trimmed.strip_suffix(" UTC").unwrap_or(trimmed);
            let (date, time) = trimmed.split_once(' ').ok_or_else(invalid)?;
            let date = parts(date, '-', [4, 2, 2]).ok_or_else(invalid)?;
            let time = parts(time, ':', [2; 3]).ok_or_else(invalid)?;
            Value::Timestamp(timestamp(date, time).ok_or_else(invalid)?)
        }
        _ => return Err(invalid()),
    })
}

fn parts(value: &str, separator: char, widths: [usize; 3]) -> Option<[u16; 3]> {
    let mut values = value.split(separator);
    let mut output = [0; 3];
    for (slot, width) in output.iter_mut().zip(widths) {
        let part = values.next()?;
        if part.len() != width || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        *slot = part.parse().ok()?;
    }
    values.next().is_none().then_some(output)
}

fn timestamp([year, month, day]: [u16; 3], [hour, minute, second]: [u16; 3]) -> Option<FILETIME> {
    let date = SYSTEMTIME {
        wYear: year,
        wMonth: month,
        wDay: day,
        wHour: hour,
        wMinute: minute,
        wSecond: second,
        ..SYSTEMTIME::default()
    };
    let mut time = FILETIME::default();
    (unsafe { SystemTimeToFileTime(&date, &mut time) } != 0).then_some(time)
}
