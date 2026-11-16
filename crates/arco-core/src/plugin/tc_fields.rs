use super::{WdxKey, WdxValue};
use std::sync::atomic::AtomicBool;
use std::{fs::Metadata, io, path::Path, time::SystemTime};

pub const TC_METADATA_FIELDS: &[&str] = &[
    "name",
    "namenoext",
    "ext",
    "extension",
    "fullname",
    "fullpath",
    "path",
    "size",
    "writedate",
    "writedatetime",
    "writetime",
    "creationdate",
    "creationdatetime",
    "creationtime",
    "accessdate",
    "accessdatetime",
    "accesstime",
    "comment",
    "attributes",
    "attributestr",
    "zoneid",
    "hosturl",
    "referrerurl",
    "archive",
    "read only",
    "readonly",
    "hidden",
    "system",
    "compressed",
    "encrypted",
    "sparse",
    "offline",
    "versionstring",
    "versionnr",
    "file type",
    "dosname",
    "directory",
    "path length",
    "case sensitive name",
    "case sensitive in folder",
    "random number",
    "decode html characters (text)",
    "decode html characters (utf-8)",
    "decode html characters (utf-16)",
    "tag present",
    "title",
    "artist",
    "album",
    "release year",
    "description",
    "track nr",
    "genre",
];

/// Built-in metadata available without loading an external content plugin.
/// Metadata already supplied by a virtual provider. Unsupported local-only
/// fields remain absent: a virtual pathname must never be opened on disk.
pub fn tc_entry_field(token: &str, entry: &crate::files::Entry) -> io::Result<Option<WdxValue>> {
    use crate::files::Kind;
    let Some((prefix, field)) = token.split_once('.') else {
        return Ok(None);
    };
    if !prefix.eq_ignore_ascii_case("tc") {
        return Ok(None);
    }
    let field = field.to_ascii_lowercase();
    if let Some(unit) = field.strip_prefix("size.") {
        let divisor = match unit {
            "bytes" | "b" => 1_u64,
            "kbytes" | "kb" | "kib" => 1024,
            "mbytes" | "mb" | "mib" => 1024_u64.pow(2),
            "gbytes" | "gb" | "gib" => 1024_u64.pow(3),
            "tbytes" | "tb" | "tib" => 1024_u64.pow(4),
            _ => return Err(io::Error::other("Unbekannte Einheit für tc.size.")),
        };
        return Ok(entry.size.map(|size| {
            let value = size as f64 / divisor as f64;
            WdxValue {
                display: value.to_string(),
                key: if divisor == 1 {
                    WdxKey::Integer(size.min(i64::MAX as u64) as i64)
                } else {
                    WdxKey::Float(value)
                },
            }
        }));
    }
    let text = |display: String| {
        Some(WdxValue {
            key: WdxKey::Text(display.to_lowercase()),
            display,
        })
    };
    let integer = |value: i64| {
        Some(WdxValue {
            display: value.to_string(),
            key: WdxKey::Integer(value),
        })
    };
    Ok(match field.as_str() {
        "fullname" => text(entry.name.to_string_lossy().into_owned()),
        "name" | "namenoext" => text(if entry.kind == Kind::Directory {
            entry.name.to_string_lossy().into_owned()
        } else {
            entry
                .path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        }),
        "ext" | "extension" => text(
            entry
                .path
                .extension()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        ),
        "path" => text(
            entry
                .path
                .parent()
                .unwrap_or(Path::new(""))
                .to_string_lossy()
                .into_owned(),
        ),
        "fullpath" => text(entry.path.to_string_lossy().into_owned()),
        "directory" => text(
            entry
                .path
                .parent()
                .and_then(Path::file_name)
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        ),
        "attributes" => integer(i64::from(entry.attributes)),
        "comment" => text(entry.comment.as_deref().unwrap_or_default().into()),
        "size" | "size.bytes" | "size.b" => entry
            .size
            .and_then(|size| i64::try_from(size).ok())
            .and_then(integer),
        "writedate" | "writetime" | "writedatetime" => entry.modified.and_then(|time| {
            let nanos = match time.duration_since(SystemTime::UNIX_EPOCH) {
                Ok(value) => value.as_nanos() as i128,
                Err(error) => -(error.duration().as_nanos() as i128),
            };
            let ticks = u64::try_from(nanos / 100 + 116444736000000000_i128).ok()?;
            Some(WdxValue {
                display: chrono::DateTime::<chrono::Local>::from(time)
                    .format(if field == "writetime" {
                        "%H:%M:%S"
                    } else {
                        "%Y-%m-%d"
                    })
                    .to_string(),
                key: WdxKey::DateTime(ticks),
            })
        }),
        _ => None,
    })
}

pub fn tc_field(token: &str, path: &Path, metadata: &Metadata) -> io::Result<Option<WdxValue>> {
    tc_field_with_cancel(token, path, metadata, &AtomicBool::new(false))
}

pub fn tc_field_with_cancel(
    token: &str,
    path: &Path,
    metadata: &Metadata,
    cancel: &AtomicBool,
) -> io::Result<Option<WdxValue>> {
    crate::files::check_cancel(cancel)?;
    let Some((plugin, field)) = token
        .split_once('.')
        .filter(|(p, _)| p.eq_ignore_ascii_case("tc"))
    else {
        return Ok(None);
    };
    let _ = plugin;
    let (field, unit) = field
        .split_once('.')
        .map_or((field, None), |(field, unit)| (field, Some(unit)));
    if unit.is_some() && !field.eq_ignore_ascii_case("size") {
        return Err(io::Error::other(
            "Dieses interne Dateifeld unterstützt keine Einheit.",
        ));
    }
    let field_lower = field.to_ascii_lowercase();
    if super::tc_audio::is_field(&field_lower) {
        return super::tc_audio::field(path, metadata, &field_lower, cancel);
    }
    #[cfg(windows)]
    let flags = {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes()
    };
    #[cfg(not(windows))]
    let flags =
        u32::from(metadata.permissions().readonly()) | if metadata.is_dir() { 16 } else { 0 };
    let text = |text: String| WdxValue {
        key: WdxKey::Text(text.to_lowercase()),
        display: text,
    };
    let integer = |n: i64| WdxValue {
        display: n.to_string(),
        key: WdxKey::Integer(n),
    };
    let time = |time: io::Result<SystemTime>, only_time: bool| -> io::Result<Option<WdxValue>> {
        let Ok(time) = time else {
            return Ok(None);
        };
        let nanos: i128 = match time.duration_since(SystemTime::UNIX_EPOCH) {
            Ok(duration) => duration.as_nanos() as i128,
            Err(error) => -(error.duration().as_nanos() as i128),
        };
        let Some(ticks) = u64::try_from(nanos / 100 + 116444736000000000_i128).ok() else {
            return Ok(None);
        };
        Ok(Some(WdxValue {
            display: chrono::DateTime::<chrono::Local>::from(time)
                .format(if only_time { "%H:%M:%S" } else { "%Y-%m-%d" })
                .to_string(),
            key: WdxKey::DateTime(ticks),
        }))
    };
    let value = match field.to_ascii_lowercase().as_str() {
        "name" | "namenoext" => text(
            if metadata.is_dir() {
                path.file_name()
            } else {
                path.file_stem()
            }
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        ),
        "ext" | "extension" => text(
            path.extension()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        ),
        "fullname" => text(
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        ),
        "fullpath" => text(full_path(path)),
        "path length" => integer(full_path(path).encode_utf16().count() as i64),
        "random number" => integer(super::tc_names::random_number(cancel)?),
        "decode html characters (text)"
        | "decode html characters (utf-8)"
        | "decode html characters (utf-16)" => {
            let name = if metadata.is_dir() {
                path.file_name()
            } else {
                path.file_stem()
            }
            .unwrap_or_default()
            .to_string_lossy();
            text(super::tc_names::decode(&name, &field_lower)?)
        }
        "case sensitive name" | "case sensitive in folder" => {
            let folder = if field_lower == "case sensitive in folder" {
                metadata.is_dir().then_some(path)
            } else {
                path.parent()
            };
            #[cfg(windows)]
            let value = folder
                .map(super::tc_native::case_sensitive)
                .transpose()?
                .unwrap_or(false);
            #[cfg(not(windows))]
            let value = folder.is_some();
            WdxValue {
                display: value.to_string(),
                key: WdxKey::Boolean(value),
            }
        }
        "path" => {
            let mut parent = full_path(path.parent().unwrap_or(path));
            if !parent.ends_with(['\\', '/']) {
                parent.push(std::path::MAIN_SEPARATOR);
            }
            text(parent)
        }
        "directory" => text(
            path.parent()
                .and_then(Path::file_name)
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        ),
        "file type" => text(
            if crate::files::is_link(metadata) {
                "reparse point"
            } else if metadata.is_dir() {
                "folder"
            } else {
                "file"
            }
            .into(),
        ),
        "archive" | "read only" | "readonly" | "hidden" | "system" | "compressed" | "encrypted"
        | "sparse" | "offline" => {
            let flag = match field_lower.as_str() {
                "archive" => 32,
                "read only" | "readonly" => 1,
                "hidden" => 2,
                "system" => 4,
                "compressed" => 2048,
                "encrypted" => 16384,
                "sparse" => 512,
                "offline" => 4096,
                _ => unreachable!(),
            };
            let value = flags & flag != 0;
            WdxValue {
                display: value.to_string(),
                key: WdxKey::Boolean(value),
            }
        }
        "versionstring" | "versionnr" => {
            #[cfg(windows)]
            if let Some((string, number)) = super::tc_native::version(path)? {
                return Ok(Some(text(if field_lower == "versionstring" {
                    string
                } else {
                    number
                })));
            }
            return Ok(None);
        }
        "dosname" => {
            #[cfg(windows)]
            let name = super::tc_native::short_name(path)?;
            #[cfg(not(windows))]
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            text(name)
        }
        "size" => {
            let divisor = match unit.unwrap_or("bytes").to_ascii_lowercase().as_str() {
                "bytes" | "b" => 1_u64,
                "kbytes" | "kb" | "kib" => 1024,
                "mbytes" | "mb" | "mib" => 1024_u64.pow(2),
                "gbytes" | "gb" | "gib" => 1024_u64.pow(3),
                "tbytes" | "tb" | "tib" => 1024_u64.pow(4),
                _ => return Err(io::Error::other("Unbekannte Einheit für tc.size.")),
            };
            if divisor == 1 {
                integer(i64::try_from(metadata.len()).map_err(io::Error::other)?)
            } else {
                let value = metadata.len() as f64 / divisor as f64;
                WdxValue {
                    display: value.to_string(),
                    key: WdxKey::Float(value),
                }
            }
        }
        "writedate" | "writedatetime" => return time(metadata.modified(), false),
        "writetime" => return time(metadata.modified(), true),
        "creationdate" | "creationdatetime" => return time(metadata.created(), false),
        "creationtime" => return time(metadata.created(), true),
        "accessdate" | "accessdatetime" => return time(metadata.accessed(), false),
        "accesstime" => return time(metadata.accessed(), true),
        "comment" => text(
            crate::descriptions::read(path.parent().unwrap_or(path))?
                .remove(&crate::descriptions::key(
                    path.file_name().unwrap_or_default(),
                ))
                .unwrap_or_default(),
        ),
        "zoneid" | "hosturl" | "referrerurl" => {
            let fields = zone_fields(path)?;
            if field.eq_ignore_ascii_case("zoneid") {
                integer(
                    fields
                        .get("zoneid")
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(0),
                )
            } else {
                text(
                    fields
                        .get(&field.to_ascii_lowercase())
                        .cloned()
                        .unwrap_or_default(),
                )
            }
        }
        "attributes" | "attributestr" => {
            if field.eq_ignore_ascii_case("attributes") {
                integer(i64::from(flags))
            } else {
                text(
                    [(1, 'r'), (32, 'a'), (4, 's'), (2, 'h')]
                        .into_iter()
                        .map(|(flag, ch)| if flags & flag != 0 { ch } else { '-' })
                        .collect(),
                )
            }
        }
        _ => {
            return Err(io::Error::other(format!(
                "Unbekanntes internes Dateifeld: {token}"
            )));
        }
    };
    Ok(Some(value))
}

fn full_path(path: &Path) -> String {
    let path = path.to_string_lossy();
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else {
        path.strip_prefix(r"\\?\").unwrap_or(&path).to_owned()
    }
}

fn zone_fields(path: &Path) -> io::Result<std::collections::HashMap<String, String>> {
    #[cfg(windows)]
    {
        use std::io::Read;
        let mut stream = path.as_os_str().to_os_string();
        stream.push(":Zone.Identifier");
        let mut file = match std::fs::File::open(stream) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Default::default()),
            Err(error) => return Err(error),
        };
        let mut bytes = Vec::new();
        (&mut file).take(65537).read_to_end(&mut bytes)?;
        if bytes.len() > 65536 {
            return Err(io::Error::other("Zone.Identifier überschreitet 64 KiB."));
        }
        let text = crate::tools::decode_text(&bytes);
        let mut zone = false;
        let mut fields = std::collections::HashMap::new();
        for line in text.lines().map(str::trim) {
            if line.starts_with('[') {
                zone = line.eq_ignore_ascii_case("[ZoneTransfer]");
            } else if zone && let Some((key, value)) = line.split_once('=') {
                fields.insert(key.trim().to_ascii_lowercase(), value.trim().to_owned());
            }
        }
        Ok(fields)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Ok(Default::default())
    }
}
