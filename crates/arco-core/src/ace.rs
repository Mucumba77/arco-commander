use crate::{
    ace_lz77,
    archive_view::{self, Selection},
    archives::{self, CrcWriter, PlannedSource, ZipEdit},
    files, tools,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, Metadata},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::SystemTime,
};
use tempfile::NamedTempFile;

const OEM: &str = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}";
const SIGNATURE: &[u8] = b"**ACE**";
const DIRECTORY: u32 = 0x10;
const SOLID: u16 = 0x8000;
const WRITABLE_MAIN_FLAGS: u16 = SOLID | 0x1000 | 0x0100 | 0x0002;
const ADD_SIZE: u16 = 1;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Encoding {
    Lz77,
    Blocked,
}

struct Entry {
    path: PathBuf,
    flags: u16,
    packed_size: u32,
    size: u32,
    date: u32,
    attributes: u32,
    crc: u32,
    compression: [u8; 6],
    offset: u64,
}

impl Entry {
    fn is_dir(&self) -> bool {
        self.attributes & DIRECTORY != 0
    }
}

pub fn pack(paths: &[PathBuf], base: &Path, target: &Path, cancel: &AtomicBool) -> io::Result<()> {
    pack_with_mode(paths, base, target, None, cancel)
}

pub fn pack_compressed(
    paths: &[PathBuf],
    base: &Path,
    target: &Path,
    cancel: &AtomicBool,
) -> io::Result<()> {
    pack_with_mode(paths, base, target, Some(Encoding::Lz77), cancel)
}

pub fn pack_blocked(
    paths: &[PathBuf],
    base: &Path,
    target: &Path,
    cancel: &AtomicBool,
) -> io::Result<()> {
    pack_with_mode(paths, base, target, Some(Encoding::Blocked), cancel)
}

fn pack_with_mode(
    paths: &[PathBuf],
    base: &Path,
    target: &Path,
    encoding: Option<Encoding>,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let plan = archives::plan_sources(paths, base, Path::new(""), cancel)?;
    let parent = fs::canonicalize(parent(target)?)?;
    for path in paths {
        if parent.starts_with(fs::canonicalize(path)?) {
            return Err(io::Error::other("Das Zielarchiv liegt im Quellordner."));
        }
    }
    let mut staging = NamedTempFile::new_in(&parent)?;
    write_main(staging.as_file_mut(), encoding == Some(Encoding::Blocked))?;
    write_sources(staging.as_file_mut(), plan, &parent, encoding, cancel)?;
    if verify(staging.as_file_mut(), cancel)? {
        crate::legacy_archives::Format::Ace.test(staging.path(), cancel)?;
    }
    files::check_cancel(cancel)?;
    staging
        .persist_noclobber(target)
        .map_err(|error| error.error)?;
    Ok(())
}

pub(crate) fn append(
    path: &Path,
    folder: &Path,
    paths: &[PathBuf],
    base: &Path,
    cancel: &AtomicBool,
) -> io::Result<usize> {
    let additions = archives::plan_sources(paths, base, folder, cancel)?;
    rewrite(path, folder, None, additions, cancel)
}

pub(crate) fn edit(path: &Path, edit: ZipEdit, cancel: &AtomicBool) -> io::Result<usize> {
    rewrite(path, Path::new(""), Some(edit), Vec::new(), cancel)
}

fn rewrite(
    path: &Path,
    folder: &Path,
    edit: Option<ZipEdit>,
    additions: Vec<PlannedSource>,
    cancel: &AtomicBool,
) -> io::Result<usize> {
    let original = files::regular_metadata(path)?;
    if !original.is_file() || original.permissions().readonly() {
        return Err(io::Error::other(
            "Das Ziel muss ein beschreibbares ACE-Archiv sein.",
        ));
    }
    let target = fs::canonicalize(path)?;
    for (source, _, _) in &additions {
        if fs::canonicalize(source)? == target {
            return Err(io::Error::other(
                "Ein ACE-Archiv kann sich nicht selbst enthalten.",
            ));
        }
    }
    let mut input = File::open(path)?;
    let (main, entries) = read_entries(&mut input, cancel)?;
    let encoding = match entries
        .iter()
        .filter(|entry| !entry.is_dir())
        .map(|entry| entry.compression[0])
        .max()
    {
        Some(2) => Some(Encoding::Blocked),
        Some(1) => Some(Encoding::Lz77),
        _ => None,
    };
    if !folder.as_os_str().is_empty()
        && !entries
            .iter()
            .any(|entry| entry.path.starts_with(folder) && (entry.path != folder || entry.is_dir()))
    {
        return Err(io::Error::other("Der Zielordner fehlt im ACE-Archiv."));
    }
    let (selection, rename) = match edit {
        Some(ZipEdit::Rename(source, name)) => {
            let target = source.with_file_name(files::leaf_name(&name)?);
            if source == target {
                return Ok(0);
            }
            let selection = Selection::new(PathBuf::new(), vec![source.clone()])?;
            (Some(selection), Some((source, target)))
        }
        Some(ZipEdit::Remove(paths)) => (Some(Selection::new(PathBuf::new(), paths)?), None),
        None => (None, None),
    };
    let affected: BTreeSet<_> = match selection {
        Some(selection) => archive_view::project(
            entries.iter().enumerate().collect(),
            Some(&selection),
            |(_, entry)| (&entry.path, 0, entry.is_dir()),
            cancel,
        )?
        .into_iter()
        .map(|((index, _), _)| index)
        .collect(),
        None => BTreeSet::new(),
    };
    if rename.is_none() && u16::from_le_bytes([main[1], main[2]]) & SOLID != 0 {
        let mut lost_dictionary_data = false;
        for (index, entry) in entries.iter().enumerate() {
            if affected.contains(&index) {
                lost_dictionary_data |= entry.size != 0;
            } else if lost_dictionary_data && !entry.is_dir() && entry.compression[0] != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "Ein nachfolgender Solid-ACE-Eintrag kann Daten aus der Auswahl zum Entpacken benötigen.",
                ));
            }
        }
    }
    let mut namespace = BTreeMap::new();
    let mut explicit = BTreeSet::new();
    let mut retained = Vec::new();
    for (index, mut entry) in entries.into_iter().enumerate() {
        if affected.contains(&index) {
            let Some((source, target)) = &rename else {
                continue;
            };
            let relative = entry.path.strip_prefix(source).map_err(io::Error::other)?;
            entry.path = if relative.as_os_str().is_empty() {
                target.clone()
            } else {
                target.join(relative)
            };
        }
        if !explicit.insert(entry.path.to_string_lossy().to_lowercase()) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Der ACE-Eintragsname existiert bereits.",
            ));
        }
        archive_view::register_path(&mut namespace, &entry.path, entry.is_dir())?;
        encode_name(&entry.path)?;
        retained.push(entry);
    }
    for (_, name, metadata) in &additions {
        let path = archives::checked_archive_path(name)?;
        if namespace.contains_key(&path.to_string_lossy().to_lowercase()) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("ACE-Eintrag existiert bereits: {name}"),
            ));
        }
        archive_view::register_path(&mut namespace, &path, metadata.is_dir())?;
    }
    let mut staging = NamedTempFile::new_in(parent(path)?)?;
    let output = staging.as_file_mut();
    write_header(output, &main)?;
    for entry in retained {
        files::check_cancel(cancel)?;
        write_file_header(output, &entry)?;
        input.seek(SeekFrom::Start(entry.offset))?;
        if tools::copy_cancelled(
            (&mut input).take(u64::from(entry.packed_size)),
            &mut *output,
            cancel,
        )? != u64::from(entry.packed_size)
        {
            return Err(io::Error::other("ACE-Eintrag wurde abgeschnitten."));
        }
    }
    let count = affected.len() + write_sources(output, additions, parent(path)?, encoding, cancel)?;
    if verify(output, cancel)? {
        crate::legacy_archives::Format::Ace.test(staging.path(), cancel)?;
    }
    drop(input);
    archives::replace_zip(staging, path, &original, cancel)?;
    Ok(count)
}

fn parent(path: &Path) -> io::Result<&Path> {
    path.parent()
        .ok_or_else(|| io::Error::other("Zielordner fehlt."))
}

fn encode_name(path: &Path) -> io::Result<Vec<u8>> {
    let name = path.to_string_lossy().replace('/', "\\");
    let bytes: Vec<_> = name
        .chars()
        .map(|ch| {
            if ch.is_ascii() {
                Some(ch as u8)
            } else {
                OEM.chars()
                    .position(|value| value == ch)
                    .map(|i| 128 + i as u8)
            }
        })
        .collect::<Option<_>>()
        .ok_or_else(|| {
            io::Error::other(format!(
                "Der ACE-Name ist in CP437 nicht darstellbar: {}",
                path.display()
            ))
        })?;
    if bytes.len() > u16::MAX as usize - 31 {
        return Err(io::Error::other("ACE-Dateiname ist zu lang."));
    }
    Ok(bytes)
}

fn decode_name(name: &[u8]) -> io::Result<PathBuf> {
    let oem: Vec<_> = OEM.chars().collect();
    let name: String = name
        .iter()
        .map(|&byte| {
            if byte < 128 {
                char::from(byte)
            } else {
                oem[usize::from(byte - 128)]
            }
        })
        .collect();
    archives::checked_archive_path(&name.replace('\\', "/"))
}

fn dos_time(time: SystemTime) -> u32 {
    let Some(date) = archives::zip_date(time) else {
        return 0x0021_0000;
    };
    (u32::from(date.year() - 1980) << 25)
        | (u32::from(date.month()) << 21)
        | (u32::from(date.day()) << 16)
        | (u32::from(date.hour()) << 11)
        | (u32::from(date.minute()) << 5)
        | u32::from(date.second() / 2)
}

fn write_header(output: &mut File, header: &[u8]) -> io::Result<()> {
    let crc = !crc32fast::hash(header) as u16;
    output.write_all(&crc.to_le_bytes())?;
    output.write_all(&(header.len() as u16).to_le_bytes())?;
    output.write_all(header)
}

fn write_main(output: &mut File, blocked: bool) -> io::Result<()> {
    let mut header = vec![0, 0, u8::from(blocked)];
    header.extend_from_slice(SIGNATURE);
    header.extend_from_slice(&if blocked {
        [20, 20, 0, 0]
    } else {
        [10, 12, 0, 0]
    });
    header.extend_from_slice(&dos_time(SystemTime::now()).to_le_bytes());
    header.extend_from_slice(&[0; 8]);
    write_header(output, &header)
}

fn write_file_header(output: &mut File, entry: &Entry) -> io::Result<()> {
    let name = encode_name(&entry.path)?;
    let mut header = vec![1];
    header.extend_from_slice(&entry.flags.to_le_bytes());
    for word in [
        entry.packed_size,
        entry.size,
        entry.date,
        entry.attributes,
        entry.crc,
    ] {
        header.extend_from_slice(&word.to_le_bytes());
    }
    header.extend_from_slice(&entry.compression);
    header.extend_from_slice(&(name.len() as u16).to_le_bytes());
    header.extend_from_slice(&name);
    write_header(output, &header)
}

fn write_sources(
    output: &mut File,
    plan: Vec<PlannedSource>,
    parent: &Path,
    encoding: Option<Encoding>,
    cancel: &AtomicBool,
) -> io::Result<usize> {
    let mut count = 0;
    for (source, name, metadata) in plan {
        files::check_cancel(cancel)?;
        files::ensure_unchanged(&source, &metadata)?;
        let size = if metadata.is_dir() {
            0
        } else {
            u32::try_from(metadata.len())
                .map_err(|_| io::Error::other("Ein ACE-Eintrag muss kleiner als 4 GiB sein."))?
        };
        let mut entry = Entry {
            path: archives::checked_archive_path(&name)?,
            flags: ADD_SIZE,
            packed_size: size,
            size,
            date: metadata.modified().map_or(0x0021_0000, dos_time),
            attributes: attributes(&metadata),
            crc: u32::MAX,
            compression: [0; 6],
            offset: 0,
        };
        if let Some(encoding) = encoding.filter(|_| size > 0) {
            let mut packed = tempfile::tempfile_in(parent)?;
            let blocked = encoding == Encoding::Blocked;
            let (actual, packed_size, crc) = ace_lz77::encode(
                File::open(&source)?.take(u64::from(size) + 1),
                &mut packed,
                blocked,
                cancel,
            )?;
            if actual != u64::from(size) {
                return Err(io::Error::other(
                    "Dateigröße hat sich beim ACE-Packen geändert.",
                ));
            }
            if packed_size < u64::from(size) {
                entry.packed_size = packed_size as u32;
                entry.compression = if blocked {
                    ace_lz77::BLOCKED
                } else {
                    ace_lz77::COMPRESSION
                };
                entry.crc = crc;
                write_file_header(output, &entry)?;
                packed.rewind()?;
                tools::copy_cancelled(packed, &mut *output, cancel)?;
                files::ensure_unchanged(&source, &metadata)?;
                count += 1;
                continue;
            }
        }
        let start = output.stream_position()?;
        write_file_header(output, &entry)?;
        if !metadata.is_dir() {
            let mut writer = CrcWriter {
                output: &mut *output,
                crc: crc32fast::Hasher::new(),
            };
            let size = tools::copy_cancelled(
                File::open(&source)?.take(u64::from(entry.size) + 1),
                &mut writer,
                cancel,
            )?;
            if size != u64::from(entry.size) {
                return Err(io::Error::other(
                    "Dateigröße hat sich beim ACE-Packen geändert.",
                ));
            }
            entry.crc = !writer.crc.finalize();
            let end = output.stream_position()?;
            output.seek(SeekFrom::Start(start))?;
            write_file_header(output, &entry)?;
            output.seek(SeekFrom::Start(end))?;
            count += 1;
        }
        files::ensure_unchanged(&source, &metadata)?;
    }
    Ok(count)
}

fn attributes(metadata: &Metadata) -> u32 {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x37
    }
    #[cfg(not(windows))]
    {
        if metadata.is_dir() { DIRECTORY } else { 0x20 }
    }
}

fn read_header(input: &mut File) -> io::Result<Vec<u8>> {
    let mut prefix = [0; 4];
    input.read_exact(&mut prefix)?;
    let size = u16::from_le_bytes([prefix[2], prefix[3]]) as usize;
    let mut header = vec![0; size];
    input.read_exact(&mut header)?;
    if !crc32fast::hash(&header) as u16 != u16::from_le_bytes([prefix[0], prefix[1]]) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "ACE-Header hat eine falsche Prüfsumme.",
        ));
    }
    Ok(header)
}

fn read_entries(input: &mut File, cancel: &AtomicBool) -> io::Result<(Vec<u8>, Vec<Entry>)> {
    input.rewind()?;
    let length = input.metadata()?.len();
    let unsupported = || {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "Die ACE-Bearbeitung unterstützt nur klassische Einzelarchive ohne Sperre, Dateikommentare, Verschlüsselung oder Recovery-Daten.",
        )
    };
    let main = read_header(input)?;
    if main.len() < 26
        || main[0] != 0
        || u16::from_le_bytes([main[1], main[2]]) & !WRITABLE_MAIN_FLAGS != 0
        || &main[3..10] != SIGNATURE
        || main[13] != 0
    {
        return Err(unsupported());
    }
    let mut names = BTreeSet::new();
    let mut entries = Vec::new();
    while input.stream_position()? < length {
        files::check_cancel(cancel)?;
        let header = read_header(input)?;
        if header.len() < 31 || header[0] != 1 || header[23] > 2 {
            return Err(unsupported());
        }
        let flags = u16::from_le_bytes([header[1], header[2]]);
        if flags & !SOLID != ADD_SIZE {
            return Err(unsupported());
        }
        let name_len = u16::from_le_bytes([header[29], header[30]]) as usize;
        if header.len() != 31 + name_len {
            return Err(unsupported());
        }
        let word =
            |offset: usize| u32::from_le_bytes(header[offset..offset + 4].try_into().unwrap());
        let packed_size = word(3);
        let size = word(7);
        let offset = input.stream_position()?;
        let end = offset
            .checked_add(u64::from(packed_size))
            .filter(|&end| end <= length)
            .ok_or_else(|| io::Error::other("ACE-Eintrag wurde abgeschnitten."))?;
        if (header[23] == 0 && packed_size != size)
            || (word(15) & DIRECTORY != 0 && (packed_size != 0 || size != 0))
        {
            return Err(unsupported());
        }
        let path = decode_name(&header[31..])?;
        if !names.insert(path.to_string_lossy().to_lowercase()) || entries.len() >= 500_000 {
            return Err(io::Error::other(
                "ACE enthält zu viele oder mehrdeutige Einträge.",
            ));
        }
        entries.push(Entry {
            path,
            flags,
            packed_size,
            size,
            date: word(11),
            attributes: word(15),
            crc: word(19),
            compression: header[23..29].try_into().unwrap(),
            offset,
        });
        input.seek(SeekFrom::Start(end))?;
    }
    Ok((main, entries))
}

fn verify(output: &mut File, cancel: &AtomicBool) -> io::Result<bool> {
    output.sync_all()?;
    let (_, entries) = read_entries(output, cancel)?;
    archive_view::project(
        entries.iter().collect(),
        None,
        |entry| (&entry.path, u64::from(entry.size), entry.is_dir()),
        cancel,
    )?;
    let mut compressed = false;
    for entry in entries {
        if entry.compression[0] != 0 && !entry.is_dir() {
            compressed = true;
            continue;
        }
        output.seek(SeekFrom::Start(entry.offset))?;
        let mut writer = CrcWriter {
            output: io::sink(),
            crc: crc32fast::Hasher::new(),
        };
        let actual = tools::copy_cancelled(
            (&mut *output).take(u64::from(entry.size)),
            &mut writer,
            cancel,
        )?;
        if actual != u64::from(entry.size) || !writer.crc.finalize() != entry.crc {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "ACE-Eintrag hat eine falsche Prüfsumme.",
            ));
        }
    }
    Ok(compressed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires the optional Python acefile adapter"]
    fn blocked_output_switches_between_delta_and_lz77() {
        if std::env::var_os("ARCO_ARCHIVE_PYTHON").is_none() {
            return;
        }
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        let file = root.join("Daten.bin");
        let mut state = 7235u32;
        let mut value = 0u8;
        let data: Vec<_> = (0..65_536)
            .map(|index| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                if (16_384..49_152).contains(&index) {
                    value = value.wrapping_add(if state % 37 == 0 { 2 } else { 1 });
                    value
                } else {
                    state as u8
                }
            })
            .collect();
        fs::write(&file, &data).unwrap();
        let plain = root.join("lz77.ace");
        let blocked = root.join("blocked.ace");
        let paths = std::slice::from_ref(&file);
        let cancel = AtomicBool::new(false);
        pack_compressed(paths, root, &plain, &cancel).unwrap();
        pack_blocked(paths, root, &blocked, &cancel).unwrap();
        assert!(fs::metadata(&blocked).unwrap().len() < fs::metadata(&plain).unwrap().len());
        let (_, entries) = read_entries(&mut File::open(&blocked).unwrap(), &cancel).unwrap();
        assert_eq!(entries[0].compression, ace_lz77::BLOCKED);
        let output = tempfile::tempdir().unwrap();
        assert_eq!(
            archive_view::extract_all(&blocked, output.path(), None, &cancel).unwrap(),
            1
        );
        assert_eq!(
            blake3::hash(&fs::read(output.path().join("Daten.bin")).unwrap()),
            blake3::hash(&data)
        );
    }

    #[test]
    #[ignore = "requires the optional Python acefile adapter"]
    fn native_lz77_output_is_readable_and_keeps_store_when_smaller() {
        if std::env::var_os("ARCO_ARCHIVE_PYTHON").is_none() {
            return;
        }
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        let repetitive = root.join("Grüße.txt");
        let mut data = (0..1024u32).flat_map(u32::to_le_bytes).collect::<Vec<_>>();
        data.extend(b"Text mit unterschiedlichen Bytes \x80\xfe\xff\n".repeat(2800));
        fs::write(&repetitive, &data).unwrap();
        let tiny = root.join("tiny.bin");
        fs::write(&tiny, [0, 255, 42]).unwrap();
        let archive = root.join("packed.ace");
        let cancel = AtomicBool::new(false);
        pack_compressed(&[repetitive, tiny], root, &archive, &cancel).unwrap();
        let (_, entries) = read_entries(&mut File::open(&archive).unwrap(), &cancel).unwrap();
        assert_eq!(entries[0].compression, ace_lz77::COMPRESSION);
        assert!(entries[0].packed_size < entries[0].size / 4);
        assert_eq!(entries[1].compression, [0; 6]);
        let output = tempfile::tempdir().unwrap();
        assert_eq!(
            archive_view::extract_all(&archive, output.path(), None, &cancel).unwrap(),
            2
        );
        assert_eq!(fs::read(output.path().join("Grüße.txt")).unwrap(), data);
        assert_eq!(
            fs::read(output.path().join("tiny.bin")).unwrap(),
            [0, 255, 42]
        );
    }

    #[test]
    fn ace_store_round_trip_supports_oem_names_and_safe_edits() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        let source = root.join("source");
        fs::create_dir_all(source.join("empty")).unwrap();
        fs::write(source.join("Grüße.txt"), b"payload").unwrap();
        let archive = root.join("stored.ace");
        let cancel = AtomicBool::new(false);
        pack(std::slice::from_ref(&source), root, &archive, &cancel).unwrap();
        let (_, entries) = read_entries(&mut File::open(&archive).unwrap(), &cancel).unwrap();
        assert_eq!(entries.len(), 3);
        assert!(
            entries
                .iter()
                .any(|entry| entry.path == Path::new("source/Grüße.txt") && entry.size == 7)
        );
        assert_eq!(
            edit(
                &archive,
                ZipEdit::Rename("source".into(), "neu".into()),
                &cancel
            )
            .unwrap(),
            3
        );
        let addition = root.join("add.txt");
        fs::write(&addition, b"more").unwrap();
        assert_eq!(
            append(
                &archive,
                Path::new("neu"),
                std::slice::from_ref(&addition),
                root,
                &cancel
            )
            .unwrap(),
            1
        );
        let before = fs::read(&archive).unwrap();
        assert!(append(&archive, Path::new("neu"), &[addition], root, &cancel).is_err());
        assert_eq!(fs::read(&archive).unwrap(), before);
        assert_eq!(
            edit(&archive, ZipEdit::Remove(vec!["neu/empty".into()]), &cancel).unwrap(),
            1
        );
        verify(
            &mut fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&archive)
                .unwrap(),
            &cancel,
        )
        .unwrap();
        let (_, entries) = read_entries(&mut File::open(&archive).unwrap(), &cancel).unwrap();
        assert!(
            entries
                .iter()
                .any(|entry| entry.path == Path::new("neu/add.txt"))
        );
        assert!(
            !entries
                .iter()
                .any(|entry| entry.path == Path::new("neu/empty"))
        );
    }
}
