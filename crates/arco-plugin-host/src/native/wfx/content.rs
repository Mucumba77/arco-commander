use super::{
    Close, FindHandle, First, Library, WIN32_FIND_DATAA, WIN32_FIND_DATAW, ansi, buffer, cancelled,
    initialize, required, symbol, wide, wide_listing,
};
use crate::native::{
    GetField, GetValue, NativeWdxField, ansi_text, content_fields, read_content_value, wide_text,
};
use arco_core::plugin::{Row, WdxField, WdxKey, WdxValue, WfxDefaultView, wfx_content_paths};
use std::ffi::OsStr;
use std::io;
use std::path::Path;
use windows_sys::Win32::Foundation::{ERROR_NO_MORE_FILES, GetLastError, INVALID_HANDLE_VALUE};

type DefaultSort = unsafe extern "system" fn(i32) -> i32;
type Unload = unsafe extern "system" fn();
type DefaultView<T> = unsafe extern "system" fn(*mut T, *mut T, *mut T, *mut T, i32) -> i32;

struct ContentScope(Option<Unload>);

impl ContentScope {
    fn new(library: &Library) -> Self {
        Self(symbol(library, b"FsContentPluginUnloading"))
    }
}

impl Drop for ContentScope {
    fn drop(&mut self) {
        if let Some(unload) = self.0 {
            // The plugin is still loaded and the optional SDK callback takes no arguments.
            unsafe { unload() };
        }
    }
}

pub(in crate::native) fn fields(library: &Library, plugin: &Path) -> io::Result<Vec<Row>> {
    check_value_export(library)?;
    initialize(library, plugin, wide_listing(library))?;
    let _scope = ContentScope::new(library);
    field_rows(library)
}

fn field_rows(library: &Library) -> io::Result<Vec<Row>> {
    let default_sort: Option<DefaultSort> = symbol(library, b"FsContentGetDefaultSortOrder");
    Ok(wfx_fields(library)?
        .into_iter()
        .map(|field| Row {
            wfx_descending: default_sort.is_some_and(|sort| unsafe { sort(field.index) } == -1),
            value: field.units().to_owned(),
            name: field.name,
            ..Default::default()
        })
        .collect())
}

pub(in crate::native) fn default_view(library: &Library, plugin: &Path) -> io::Result<Vec<Row>> {
    let wide_view: Option<DefaultView<u16>> = symbol(library, b"FsContentGetDefaultViewW");
    let ansi_view: Option<DefaultView<u8>> = symbol(library, b"FsContentGetDefaultView");
    if wide_view.is_none() && ansi_view.is_none() {
        return Ok(Vec::new());
    }
    initialize(library, plugin, wide_listing(library))?;
    let _scope = ContentScope::new(library);
    let view = if let Some(get) = wide_view {
        read_default_view(get, wide_text)
    } else {
        ansi_view.and_then(|get| read_default_view(get, ansi_text))
    };
    let Some(view) = view else {
        return Ok(Vec::new());
    };
    let mut rows = vec![Row {
        wfx_default_view: Some(view),
        ..Default::default()
    }];
    if check_value_export(library).is_ok()
        && symbol::<GetField>(library, b"FsContentGetSupportedField").is_some()
    {
        rows.extend(field_rows(library)?);
    }
    Ok(rows)
}

fn read_default_view<T: Copy + Default>(
    get: DefaultView<T>,
    decode: impl Fn(&[T]) -> String,
) -> Option<WfxDefaultView> {
    const LENGTH: usize = 4096;
    let [mut contents, mut headers, mut widths, mut options] =
        std::array::from_fn(|_| vec![T::default(); LENGTH + 1]);
    // Each writable buffer has LENGTH elements plus a sentinel outside the advertised range.
    let available = unsafe {
        get(
            contents.as_mut_ptr(),
            headers.as_mut_ptr(),
            widths.as_mut_ptr(),
            options.as_mut_ptr(),
            LENGTH as i32,
        )
    };
    (available != 0).then(|| WfxDefaultView {
        contents: decode(&contents[..LENGTH]),
        headers: decode(&headers[..LENGTH]),
        widths: decode(&widths[..LENGTH]),
        options: decode(&options[..LENGTH]),
    })
}

pub(in crate::native) fn values(
    library: &Library,
    plugin: &Path,
    directory: &Path,
    fields: &[WdxField],
    names: &[String],
) -> io::Result<Vec<Row>> {
    let directory = directory.to_str().ok_or(io::ErrorKind::InvalidInput)?;
    let paths = wfx_content_paths(directory, names, fields)?;
    let (get, unicode) = check_value_export(library)?;
    let listing_unicode = wide_listing(library);
    initialize(library, plugin, listing_unicode)?;
    let _scope = ContentScope::new(library);
    let available = wfx_fields(library)?;
    let selections: Vec<_> = fields
        .iter()
        .map(|field| {
            let native = available.iter().find(|native| native.name == field.field)?;
            Some((native.index, native.unit_index(field.unit.as_deref()).ok()?))
        })
        .collect();
    if selections.iter().all(Option::is_none) {
        return Err(io::Error::other("Die gewählten WFX-Spaltenfelder fehlen."));
    }

    // A separate host needs to establish the same plugin connection as the original listing.
    enter_directory(library, OsStr::new(directory), listing_unicode)?;
    let mut rows = Vec::with_capacity(paths.len());
    for (row, path) in paths.iter().enumerate() {
        if cancelled() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let content = selections
            .iter()
            .map(|selection| {
                let (field, unit) = (*selection)?;
                let path = OsStr::new(path);
                let value = if unicode {
                    let mut native = buffer(wide(path)).ok()?;
                    read_content_value(get, native.as_mut_ptr().cast(), field, unit, false)
                } else {
                    let mut native = buffer(ansi(path).ok()?).ok()?;
                    read_content_value(get, native.as_mut_ptr().cast(), field, unit, false)
                };
                value.map(bounded_value)
            })
            .collect();
        rows.push(Row {
            name: row.to_string(),
            wfx_content: content,
            ..Default::default()
        });
    }
    Ok(rows)
}

fn wfx_fields(library: &Library) -> io::Result<Vec<NativeWdxField>> {
    content_fields(library, b"FsContentGetSupportedField")
}

fn check_value_export(library: &Library) -> io::Result<(GetValue, bool)> {
    if let Some(get) = symbol(library, b"FsContentGetValueW") {
        return Ok((get, true));
    }
    Ok((required(library, b"FsContentGetValue")?, false))
}

fn enter_directory(library: &Library, directory: &OsStr, unicode: bool) -> io::Result<()> {
    let close: Close = required(library, b"FsFindClose")?;
    let raw = if unicode {
        let first: First<u16, WIN32_FIND_DATAW> = required(library, b"FsFindFirstW")?;
        let mut directory = buffer(wide(directory))?;
        unsafe { first(directory.as_mut_ptr(), &mut WIN32_FIND_DATAW::default()) }
    } else {
        let first: First<u8, WIN32_FIND_DATAA> = required(library, b"FsFindFirst")?;
        let mut directory = buffer(ansi(directory)?)?;
        unsafe { first(directory.as_mut_ptr(), &mut WIN32_FIND_DATAA::default()) }
    };
    if raw == INVALID_HANDLE_VALUE {
        let error = unsafe { GetLastError() };
        if error != ERROR_NO_MORE_FILES {
            return Err(io::Error::other(format!(
                "WFX-Spalten konnten den Ordner nicht öffnen (Status {error})."
            )));
        }
    } else {
        drop(FindHandle { raw, close });
    }
    Ok(())
}

fn bounded_value(mut value: WdxValue) -> WdxValue {
    value.display = value
        .display
        .chars()
        .take(1024)
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    if matches!(value.key, WdxKey::Text(_)) {
        value.key = WdxKey::Text(value.display.to_lowercase());
    }
    value
}
