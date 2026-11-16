//! ACE volume identities, including the bounded SFX prefix. No stub is executed.
use crate::{files, tools};
use std::{
    fs::{self, File, Metadata},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

const MULTI: u16 = 0x0800;
const NEXT: u16 = 0x2000;

fn header(input: &mut File) -> io::Result<Vec<u8>> {
    let mut prefix = [0; 4];
    input.read_exact(&mut prefix)?;
    let mut body = vec![0; u16::from_le_bytes([prefix[2], prefix[3]]) as usize];
    input.read_exact(&mut body)?;
    if body.len() < 3
        || !crc32fast::hash(&body) as u16 != u16::from_le_bytes([prefix[0], prefix[1]])
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Ungültiger ACE-Header.",
        ));
    }
    Ok(body)
}

fn main_header(path: &Path) -> io::Result<(File, Vec<u8>)> {
    files::regular_metadata(path)?;
    let mut input = File::open(path)?;
    let mut prefix = Vec::new();
    (&mut input)
        .take(524_288 + 65_540)
        .read_to_end(&mut prefix)?;
    for (signature, _) in prefix
        .windows(7)
        .enumerate()
        .filter(|(_, b)| *b == b"**ACE**")
    {
        if signature < 7 || signature - 7 > 524_288 {
            continue;
        }
        input.seek(SeekFrom::Start((signature - 7) as u64))?;
        if let Ok(body) = header(&mut input)
            && body.len() >= 26
            && body[0] == 0
            && &body[3..10] == b"**ACE**"
        {
            return Ok((input, body));
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "Kein gültiger ACE-Hauptheader gefunden.",
    ))
}

pub(crate) fn recognizes(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();
    (ext.eq_ignore_ascii_case("exe")
        || ext.len() >= 3
            && ext.as_bytes()[0].eq_ignore_ascii_case(&b'c')
            && ext.as_bytes()[1..].iter().all(u8::is_ascii_digit))
        && main_header(path).is_ok()
}

pub(crate) fn ensure_writable(path: &Path) -> io::Result<()> {
    let (_, main) = main_header(path)?;
    if u16::from_le_bytes([main[1], main[2]]) & 0x4000 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Das ACE-Archiv ist gegen Änderungen gesperrt.",
        ));
    }
    Ok(())
}

pub(crate) fn snapshot(path: &Path, cancel: &AtomicBool) -> io::Result<Vec<(PathBuf, Metadata)>> {
    let (mut input, mut main) = main_header(path)?;
    if main[13] != 0 {
        return Err(io::Error::other(
            "Öffne den ersten ACE-Teil (.ace oder .exe), nicht einen Folgeteil.",
        ));
    }
    if u16::from_le_bytes([main[1], main[2]]) & MULTI == 0 {
        return Ok(Vec::new());
    }
    let mut parts = Vec::new();
    let mut current = path.to_path_buf();
    for volume in 0..=255usize {
        files::check_cancel(cancel)?;
        let metadata = files::regular_metadata(&current)?;
        if usize::from(main[13]) != volume || u16::from_le_bytes([main[1], main[2]]) & MULTI == 0 {
            return Err(io::Error::other(
                "Die ACE-Teile gehören nicht zum selben Satz.",
            ));
        }
        let mut needs_next = false;
        while input.stream_position()? < metadata.len() {
            files::check_cancel(cancel)?;
            let entry = header(&mut input)?;
            let flags = u16::from_le_bytes([entry[1], entry[2]]);
            needs_next = entry[0] == 1 && flags & NEXT != 0;
            if flags & 1 != 0 {
                if entry.len() < 7 {
                    return Err(io::Error::other("Abgeschnittener ACE-Dateiheader."));
                }
                let size = u64::from(u32::from_le_bytes(entry[3..7].try_into().unwrap()));
                let end = input
                    .stream_position()?
                    .checked_add(size)
                    .filter(|end| *end <= metadata.len())
                    .ok_or_else(|| io::Error::other("Abgeschnittener ACE-Teil."))?;
                input.seek(SeekFrom::Start(end))?;
            }
        }
        parts.push((current.clone(), metadata));
        let next = path.with_extension(format!("c{volume:02}"));
        match fs::symlink_metadata(&next) {
            Ok(_) => {
                current = next;
                (input, main) = main_header(&current)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound && !needs_next => {
                return Ok(parts);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(io::Error::other(format!(
                    "ACE-Folgeteil fehlt: {}",
                    next.display()
                )));
            }
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::other("Zu viele ACE-Teile."))
}

fn write_header(output: &mut File, body: &[u8]) -> io::Result<()> {
    output.write_all(&(!crc32fast::hash(body) as u16).to_le_bytes())?;
    output.write_all(&(body.len() as u16).to_le_bytes())?;
    output.write_all(body)
}

pub(crate) fn part_path(first: &Path, index: usize) -> PathBuf {
    if index == 0 {
        first.to_path_buf()
    } else {
        first.with_extension(format!("c{:02}", index - 1))
    }
}

fn new_volume(first: &Path, main: &[u8], index: usize) -> io::Result<(PathBuf, File)> {
    let mut main = main.to_vec();
    let volume: u8 = index
        .try_into()
        .map_err(|_| io::Error::other("ACE unterstützt hier höchstens 256 Teile."))?;
    let flags = u16::from_le_bytes([main[1], main[2]]) | MULTI;
    main[1..3].copy_from_slice(&flags.to_le_bytes());
    main[13] = volume;
    let path = part_path(first, index);
    let mut file = File::options().create_new(true).write(true).open(&path)?;
    write_header(&mut file, &main)?;
    Ok((path, file))
}

pub(crate) fn split(
    source: &Path,
    first: &Path,
    capacity: u64,
    password: Option<&str>,
    cancel: &AtomicBool,
) -> io::Result<Vec<PathBuf>> {
    let original = files::regular_metadata(source)?;
    let (mut input, main) = main_header(source)?;
    if u16::from_le_bytes([main[1], main[2]]) & MULTI != 0 {
        return Err(io::Error::other(
            "Die ACE-Teilquelle muss ein Einzelarchiv sein.",
        ));
    }
    if capacity < main.len() as u64 + 4 + 64 || capacity > u64::from(u32::MAX) {
        return Err(io::Error::other(
            "Die ACE-Teilgröße ist für Haupt- und Dateiheader zu klein oder größer als 4 GiB.",
        ));
    }
    let (path, mut output) = new_volume(first, &main, 0)?;
    let mut paths = vec![path];
    while input.stream_position()? < original.len() {
        files::check_cancel(cancel)?;
        let entry = header(&mut input)?;
        if entry.len() < 31 || entry[0] != 1 {
            return Err(io::Error::other(
                "Dieser ACE-Headertyp kann nicht nativ aufgeteilt werden.",
            ));
        }
        let mut remaining = u64::from(u32::from_le_bytes(entry[3..7].try_into().unwrap()));
        let mut continuation = false;
        loop {
            files::check_cancel(cancel)?;
            let overhead = entry.len() as u64 + 4;
            if overhead + main.len() as u64 + 4 + u64::from(remaining != 0) > capacity {
                return Err(io::Error::other(
                    "Die ACE-Teilgröße ist für einen Dateinamen zu klein.",
                ));
            }
            if capacity - output.stream_position()? < overhead + u64::from(remaining != 0) {
                output.sync_all()?;
                let (path, next) = new_volume(first, &main, paths.len())?;
                paths.push(path);
                output = next;
            }
            let size = remaining.min(capacity - output.stream_position()? - overhead);
            let mut segment = entry.clone();
            let flags = (u16::from_le_bytes([entry[1], entry[2]]) & !0x3000)
                | if continuation { 0x1000 } else { 0 }
                | if size < remaining { NEXT } else { 0 };
            segment[1..3].copy_from_slice(&flags.to_le_bytes());
            segment[3..7].copy_from_slice(&(size as u32).to_le_bytes());
            write_header(&mut output, &segment)?;
            if tools::copy_cancelled((&mut input).take(size), &mut output, cancel)? != size {
                return Err(io::Error::other("ACE-Nutzdaten sind abgeschnitten."));
            }
            remaining -= size;
            if remaining == 0 {
                break;
            }
            continuation = true;
        }
    }
    output.sync_all()?;
    files::ensure_unchanged(source, &original)?;
    crate::archive_view::test(&paths[0], password, cancel)?;
    Ok(paths)
}

pub fn pack(
    paths: &[PathBuf],
    base: &Path,
    target: &Path,
    capacity: u64,
    mode: u8,
    password: Option<&str>,
    cancel: &AtomicBool,
) -> io::Result<usize> {
    let parent = target
        .parent()
        .ok_or_else(|| io::Error::other("Archivziel fehlt."))?;
    for index in 0..256 {
        if part_path(target, index).try_exists()? {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
    }
    let workspace = tempfile::tempdir_in(parent)?;
    let source = workspace.path().join("source.ace");
    if let Some(password) = password {
        crate::legacy_archives::pack_ace_encrypted(paths, base, &source, mode, password, cancel)?;
    } else {
        match mode {
            0 => crate::ace::pack(paths, base, &source, cancel)?,
            1 => crate::ace::pack_compressed(paths, base, &source, cancel)?,
            2 => crate::ace::pack_blocked(paths, base, &source, cancel)?,
            _ => return Err(io::Error::other("Unbekannte ACE-Kompressionsmethode.")),
        }
    }
    let directory = workspace.path().join("parts");
    fs::create_dir(&directory)?;
    let first = directory.join(
        target
            .file_name()
            .ok_or_else(|| io::Error::other("Archivname fehlt."))?,
    );
    let parts = split(&source, &first, capacity, password, cancel)?;
    tools::publish_parts(
        parts
            .into_iter()
            .enumerate()
            .map(|(index, part)| (part, part_path(target, index)))
            .collect(),
        cancel,
    )
}
