use crate::files;
use chrono::{DateTime, Datelike, Local, Months, NaiveDateTime, TimeDelta, Timelike};
use std::fs::File;
use std::io::{self, Cursor, Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Clone, Copy, Debug)]
pub struct Times {
    pub opened: NaiveDateTime,
    pub edited: NaiveDateTime,
}

impl Times {
    pub fn now() -> Self {
        let now = Local::now().naive_local();
        Self {
            opened: now,
            edited: now,
        }
    }
}

#[derive(Default)]
enum Source {
    #[default]
    Modified,
    Opened,
    Edited,
    Capture,
}

#[derive(Default)]
enum Offset {
    #[default]
    None,
    Duration(TimeDelta),
    AddMonths(Months),
    SubMonths(Months),
}

impl Offset {
    fn parse(spec: &str) -> Option<Self> {
        let (unit, value) = spec.as_bytes().split_last()?;
        if !matches!(value.first(), Some(b'+' | b'-'))
            || value.len() < 2
            || !value[1..].iter().all(u8::is_ascii_digit)
        {
            return None;
        }
        let value = std::str::from_utf8(value).ok()?.parse::<i64>().ok()?;
        let duration = match *unit {
            b's' => TimeDelta::try_seconds(value),
            b'm' => TimeDelta::try_minutes(value),
            b'h' => TimeDelta::try_hours(value),
            b'D' => TimeDelta::try_days(value),
            b'M' | b'Y' => {
                let months =
                    value
                        .unsigned_abs()
                        .checked_mul(if *unit == b'Y' { 12 } else { 1 })?;
                let months = Months::new(u32::try_from(months).ok()?);
                return Some(if value < 0 {
                    Self::SubMonths(months)
                } else {
                    Self::AddMonths(months)
                });
            }
            _ => return None,
        };
        duration.map(Self::Duration)
    }

    fn apply(&self, value: NaiveDateTime) -> io::Result<NaiveDateTime> {
        let result = match self {
            Self::None => Some(value),
            Self::Duration(delta) => value.checked_add_signed(*delta),
            Self::AddMonths(months) => value.checked_add_months(*months),
            Self::SubMonths(months) => value.checked_sub_months(*months),
        };
        result.ok_or_else(|| {
            io::Error::other("Die Zeitverschiebung liegt außerhalb des Datumsbereichs.")
        })
    }
}

pub(super) struct TimeState {
    times: Times,
    source: Source,
    offset: Offset,
    modified: Option<NaiveDateTime>,
    captured: Option<Option<NaiveDateTime>>,
}

impl TimeState {
    pub(super) fn new(times: Times) -> Self {
        Self {
            times,
            source: Source::default(),
            offset: Offset::default(),
            modified: None,
            captured: None,
        }
    }

    pub(super) fn select(&mut self, selector: &str) -> io::Result<()> {
        let invalid = || io::Error::other(format!("Ungültige Zeitmaske [{selector}]."));
        let source = match selector.as_bytes().get(1) {
            Some(b'1') => Source::Modified,
            Some(b'2') => Source::Opened,
            Some(b'3') => Source::Edited,
            Some(b'4') => Source::Capture,
            _ => return Err(invalid()),
        };
        let offset = match &selector[2..] {
            "" => Offset::None,
            _ if matches!(source, Source::Capture) => return Err(invalid()),
            suffix => Offset::parse(suffix).ok_or_else(invalid)?,
        };
        self.source = source;
        self.offset = offset;
        Ok(())
    }

    pub(super) fn value(&mut self, path: &Path) -> io::Result<Option<NaiveDateTime>> {
        let value = match self.source {
            Source::Opened => Some(self.times.opened),
            Source::Edited => Some(self.times.edited),
            Source::Modified => {
                if self.modified.is_none() {
                    let modified = files::regular_metadata(path)?.modified()?;
                    self.modified = Some(DateTime::<Local>::from(modified).naive_local());
                }
                self.modified
            }
            Source::Capture => {
                if self.captured.is_none() {
                    self.captured = Some(capture_time(path)?);
                }
                self.captured.flatten()
            }
        };
        value.map(|value| self.offset.apply(value)).transpose()
    }
}

fn capture_time(path: &Path) -> io::Result<Option<NaiveDateTime>> {
    let metadata = files::regular_metadata(path)?;
    if !metadata.is_file() {
        return Ok(None);
    }
    let mut file = File::open(path)?;
    let extension = path.extension().and_then(|extension| extension.to_str());
    let is_video = extension.is_some_and(|extension| {
        ["mp4", "m4v", "mov"]
            .iter()
            .any(|suffix| extension.eq_ignore_ascii_case(suffix))
    });
    let captured = if is_video {
        movie_time(&mut file, metadata.len())
    } else {
        exif_time(file)
    }?;
    files::ensure_unchanged(path, &metadata)?;
    Ok(captured)
}

fn exif_time(mut file: File) -> io::Result<Option<NaiveDateTime>> {
    // Container and TIFF offsets are confined to a bounded prefix. No pixels are decoded.
    const PREFIX_LIMIT: u64 = 8 * 1024 * 1024;
    let mut data = Vec::new();
    file.by_ref().take(32).read_to_end(&mut data)?;
    let exif_container = data.starts_with(b"\xff\xd8")
        || data.starts_with(b"II\x2a\0")
        || data.starts_with(b"MM\0\x2a")
        || data.starts_with(b"\x89PNG\r\n\x1a\n")
        || data.get(4..8) == Some(b"ftyp")
        || (data.starts_with(b"RIFF") && data.get(8..12) == Some(b"WEBP"));
    if !exif_container {
        return Ok(None);
    }
    file.take(PREFIX_LIMIT - data.len() as u64)
        .read_to_end(&mut data)?;
    let Ok(metadata) = exif::Reader::new().read_from_container(&mut Cursor::new(data)) else {
        return Ok(None);
    };
    let field = |tag| match &metadata.get_field(tag, exif::In::PRIMARY)?.value {
        exif::Value::Ascii(values) => std::str::from_utf8(values.first()?).ok(),
        _ => None,
    };
    for (date, fraction) in [
        (exif::Tag::DateTimeOriginal, exif::Tag::SubSecTimeOriginal),
        (exif::Tag::DateTimeDigitized, exif::Tag::SubSecTimeDigitized),
        (exif::Tag::DateTime, exif::Tag::SubSecTime),
    ] {
        let Some(mut value) = field(date)
            .and_then(|date| NaiveDateTime::parse_from_str(date.trim(), "%Y:%m:%d %H:%M:%S").ok())
        else {
            continue;
        };
        if let Some(fraction) = field(fraction).map(str::trim)
            && !fraction.is_empty()
            && fraction.bytes().all(|byte| byte.is_ascii_digit())
        {
            let nanos = fraction
                .bytes()
                .chain(std::iter::repeat(b'0'))
                .take(9)
                .fold(0u32, |nanos, digit| nanos * 10 + u32::from(digit - b'0'));
            value = value.with_nanosecond(nanos).unwrap_or(value);
        }
        return Ok(Some(value));
    }
    Ok(None)
}

fn movie_time(file: &mut File, length: u64) -> io::Result<Option<NaiveDateTime>> {
    let result: io::Result<_> = (|| {
        let Some((movie_start, movie_end)) = find_box(file, 0, length, *b"moov")? else {
            return Ok(None);
        };
        let Some((start, end)) = find_box(file, movie_start, movie_end, *b"mvhd")? else {
            return Ok(None);
        };
        if end - start < 8 {
            return Ok(None);
        }
        file.seek(SeekFrom::Start(start))?;
        let mut header = [0; 8];
        file.read_exact(&mut header)?;
        let first = u32::from_be_bytes(header[4..8].try_into().unwrap());
        let seconds = match header[0] {
            0 => u64::from(first),
            1 if end - start >= 12 => {
                let mut tail = [0; 4];
                file.read_exact(&mut tail)?;
                (u64::from(first) << 32) | u64::from(u32::from_be_bytes(tail))
            }
            _ => return Ok(None),
        };
        // MP4 records seconds since 1904-01-01 UTC. Zero is used for unknown dates.
        let captured = (seconds != 0)
            .then_some(seconds)
            .and_then(|seconds| i64::try_from(seconds).ok())
            .and_then(|seconds| DateTime::from_timestamp(seconds - 2_082_844_800, 0))
            .filter(|date| date.year() <= 9999)
            .and_then(|date| {
                let local = date.with_timezone(&Local);
                local.naive_utc().checked_add_offset(*local.offset())
            });
        Ok(captured)
    })();
    match result {
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(None),
        other => other,
    }
}

fn find_box(
    file: &mut File,
    mut offset: u64,
    end: u64,
    name: [u8; 4],
) -> io::Result<Option<(u64, u64)>> {
    for _ in 0..4096 {
        if end - offset < 8 {
            break;
        }
        file.seek(SeekFrom::Start(offset))?;
        let mut header = [0; 8];
        file.read_exact(&mut header)?;
        let size = u32::from_be_bytes(header[..4].try_into().unwrap());
        let (size, header_len) = match size {
            0 => (end - offset, 8),
            1 if end - offset >= 16 => {
                let mut extended = [0; 8];
                file.read_exact(&mut extended)?;
                (u64::from_be_bytes(extended), 16)
            }
            1 => break,
            size => (u64::from(size), 8),
        };
        if size < header_len || size > end - offset {
            break;
        }
        if header[4..] == name {
            return Ok(Some((offset + header_len, offset + size)));
        }
        offset += size;
    }
    Ok(None)
}
