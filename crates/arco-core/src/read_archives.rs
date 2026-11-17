//! Supplementary readers for CAB LZX and LHA header 3, which the bundled
//! console reader cannot decode in all valid upstream regression fixtures.
use crate::{
    archive_view::{self, Item, MAX_EXTRACT, Selection},
    archives, files, tools,
};
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
    sync::atomic::AtomicBool,
};

pub(crate) fn recognizes(path: &Path) -> bool {
    let mut head = [0; 36];
    if File::open(path)
        .and_then(|mut file| file.read_exact(&mut head))
        .is_err()
    {
        return false;
    }
    if path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("lha") || ext.eq_ignore_ascii_case("lzh"))
    {
        return head[20] == 3 && &head[2..5] == b"-lh";
    }
    // CAB spanning is handled by the existing volume-aware console reader.
    &head[..4] == b"MSCF"
        && u16::from_le_bytes([head[30], head[31]]) & 3 == 0
        && cab_without_e8(path, &head).unwrap_or(false)
        && File::open(path)
            .and_then(cab::Cabinet::new)
            .is_ok_and(|cabinet| {
                cabinet
                    .folder_entries()
                    .any(|folder| matches!(folder.compression_type(), cab::CompressionType::Lzx(_)))
                    && cabinet.folder_entries().all(|folder| {
                        !matches!(folder.compression_type(), cab::CompressionType::Quantum(..))
                    })
            })
}

fn cab_without_e8(path: &Path, head: &[u8; 36]) -> io::Result<bool> {
    // lzxd 0.2.7 has an E8 zero-target translation defect. Keep those folders
    // on the established console reader rather than publishing altered bytes.
    let mut input = File::open(path)?;
    let mut folder_offset = 36u64;
    let mut folder_reserve = 0;
    let mut data_reserve = 0;
    if u16::from_le_bytes([head[30], head[31]]) & 4 != 0 {
        input.seek(SeekFrom::Start(36))?;
        let mut reserve = [0; 4];
        input.read_exact(&mut reserve)?;
        folder_offset += 4 + u64::from(u16::from_le_bytes([reserve[0], reserve[1]]));
        folder_reserve = u64::from(reserve[2]);
        data_reserve = u64::from(reserve[3]);
    }
    for index in 0..u16::from_le_bytes([head[26], head[27]]) {
        input.seek(SeekFrom::Start(
            folder_offset + u64::from(index) * (8 + folder_reserve),
        ))?;
        let mut folder = [0; 8];
        input.read_exact(&mut folder)?;
        if folder[6] & 15 != 3 || u16::from_le_bytes([folder[4], folder[5]]) == 0 {
            continue;
        }
        input.seek(SeekFrom::Start(
            u64::from(u32::from_le_bytes(folder[..4].try_into().unwrap())) + 8 + data_reserve,
        ))?;
        let mut word = [0; 2];
        input.read_exact(&mut word)?;
        if word[1] & 128 != 0 {
            return Ok(false);
        }
    }
    Ok(true)
}

fn lha_item(header: &delharc::LhaHeader) -> io::Result<Item> {
    // Validate the original components. delharc's display-path helper removes
    // traversal components and percent-escapes names, unsuitable for extraction.
    let mut filename = header.filename.to_vec();
    let mut directory = Vec::new();
    for extra in header.iter_extra() {
        match extra {
            [1, data @ ..] => filename = data.to_vec(),
            [2, data @ ..] => directory = data.to_vec(),
            [0x50, lo, hi, ..] if u16::from_le_bytes([*lo, *hi]) & 0xf000 == 0xa000 => {
                return Err(io::Error::other("LHA-Verknüpfungen werden nicht entpackt."));
            }
            _ => (),
        }
    }
    for byte in &mut directory {
        if *byte == 0xff {
            *byte = b'/';
        }
    }
    if !directory.is_empty() && !directory.ends_with(b"/") {
        directory.push(b'/');
    }
    directory.extend(filename);
    // UTF-8 names are preserved. Legacy bytes use the DOS code page, matching
    // the archive family; no path component is silently removed or renamed.
    let name = match std::str::from_utf8(&directory) {
        Ok(name) => name.to_owned(),
        Err(_) => crate::ace::decode_oem_name(&directory),
    }
    .replace('\\', "/");
    let name = name.trim_end_matches('/');
    let path = archives::checked_archive_path(name)?;
    let modified = match header.parse_last_modified() {
        delharc::header::TimestampResult::Utc(time) => Some(time.into()),
        delharc::header::TimestampResult::Naive(time) => archive_view::local_timestamp(time),
        _ => None,
    };
    Ok(Item {
        path,
        directory: header.is_directory(),
        size: if header.is_directory() {
            0
        } else {
            header.original_size
        },
        encrypted: false,
        modified,
    })
}

fn walk(
    path: &Path,
    cancel: &AtomicBool,
    decode: bool,
    mut visit: impl FnMut(Item, &mut dyn Read) -> io::Result<()>,
) -> io::Result<usize> {
    let original = files::regular_metadata(path)?;
    files::check_cancel(cancel)?;
    let mut count = 0;
    let mut nodes = 0;
    let mut total = 0u64;
    if path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("lha") || ext.eq_ignore_ascii_case("lzh"))
    {
        let mut input = File::open(path)?;
        while input.stream_position()? < original.len() {
            files::check_cancel(cancel)?;
            // Bound allocations for extended headers independently of the
            // (potentially much larger) compressed payload.
            let Some(header) = delharc::LhaHeader::read(&mut (&mut input).take(1024 * 1024))
                .map_err(io::Error::other)?
            else {
                break;
            };
            nodes += 1;
            if nodes > 500_000 {
                return Err(io::Error::other("Zu viele Archiveinträge."));
            }
            let end = input
                .stream_position()?
                .checked_add(header.compressed_size)
                .filter(|end| *end <= original.len())
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::UnexpectedEof, "Abgeschnittener LHA-Eintrag.")
                })?;
            let item = lha_item(&header)?;
            total = total
                .checked_add(item.size)
                .filter(|size| *size <= MAX_EXTRACT)
                .ok_or_else(|| io::Error::other("Archiv überschreitet das Größenlimit."))?;
            if decode && !item.directory {
                let decoder = delharc::decode::DecoderAny::new_from_header(&header, &mut input);
                let mut reader = delharc::LhaDecodeReader::default();
                reader.begin_with_header_and_decoder(header, decoder);
                if !reader.is_decoder_supported() {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "LHA-Kompressionsmethode wird nicht unterstützt.",
                    ));
                }
                visit(item, &mut reader)?;
                reader.crc_check().map_err(io::Error::other)?;
                count += 1;
            } else {
                visit(item, &mut io::empty())?;
            }
            input.seek(SeekFrom::Start(end))?;
        }
    } else {
        let mut cabinet = cab::Cabinet::new(File::open(path)?)?;
        let entries = cabinet
            .folder_entries()
            .flat_map(|folder| folder.file_entries())
            .map(|file| {
                let time = file
                    .datetime()
                    .and_then(|time| {
                        chrono::NaiveDate::from_ymd_opt(
                            time.year(),
                            time.month() as u32,
                            time.day().into(),
                        )?
                        .and_hms_opt(
                            time.hour().into(),
                            time.minute().into(),
                            time.second().into(),
                        )
                    })
                    .and_then(archive_view::local_timestamp);
                let item = Item {
                    path: archives::checked_archive_path(&file.name().replace('\\', "/"))?,
                    directory: false,
                    size: file.uncompressed_size().into(),
                    encrypted: false,
                    modified: time,
                };
                Ok((file.name().to_owned(), item))
            })
            .collect::<io::Result<Vec<_>>>()?;
        if entries.len() > 500_000 {
            return Err(io::Error::other("Zu viele Archiveinträge."));
        }
        for (name, item) in entries {
            files::check_cancel(cancel)?;
            total = total
                .checked_add(item.size)
                .filter(|size| *size <= MAX_EXTRACT)
                .ok_or_else(|| io::Error::other("Archiv überschreitet das Größenlimit."))?;
            if decode {
                visit(item, &mut cabinet.read_file(&name)?)?;
                count += 1;
            } else {
                visit(item, &mut io::empty())?;
            }
        }
    }
    files::ensure_unchanged(path, &original)?;
    Ok(count)
}

pub(crate) fn list(path: &Path, cancel: &AtomicBool) -> io::Result<Vec<Item>> {
    let mut entries = Vec::new();
    walk(path, cancel, false, |item, _| {
        entries.push(item);
        Ok(())
    })?;
    Ok(entries)
}

pub(crate) fn test(path: &Path, cancel: &AtomicBool) -> io::Result<usize> {
    walk(path, cancel, true, |item, reader| {
        if !item.directory
            && tools::copy_cancelled(reader.take(item.size.saturating_add(1)), io::sink(), cancel)?
                != item.size
        {
            return Err(io::Error::other("Ungültige entpackte Dateigröße."));
        }
        Ok(())
    })
}

pub(crate) fn extract(
    path: &Path,
    destination: &Path,
    selection: Option<&Selection>,
    cancel: &AtomicBool,
    policy: crate::archive_zone::ZonePolicy,
) -> io::Result<usize> {
    let original = files::regular_metadata(path)?;
    let entries = archive_view::project(
        list(path, cancel)?,
        selection,
        |item| (&item.path, item.size, item.directory),
        cancel,
    )?;
    let base =
        archive_view::check_destination(destination, &entries, |item| item.directory, cancel)?;
    let zone = crate::archive_zone::ArchiveZone::read_with_policy(path, cancel, policy)?;
    let working = tempfile::tempdir_in(&base)?;
    let mut outputs = Vec::new();
    let requested = entries
        .iter()
        .map(|(item, relative)| (item.path.clone(), relative))
        .collect::<std::collections::BTreeMap<_, _>>();
    walk(path, cancel, true, |item, reader| {
        if item.directory {
            return Ok(());
        }
        if let Some(relative) = requested.get(&item.path) {
            let mut output = tempfile::NamedTempFile::new_in(working.path())?;
            if tools::copy_cancelled(
                reader.take(item.size.saturating_add(1)),
                &mut output,
                cancel,
            )? != item.size
            {
                return Err(io::Error::other("Ungültige entpackte Dateigröße."));
            }
            output.flush()?;
            archive_view::preserve_timestamp(output.as_file(), item.modified)?;
            zone.apply(output.path())?;
            outputs.push((output, (*relative).clone()));
        } else if tools::copy_cancelled(
            reader.take(item.size.saturating_add(1)),
            io::sink(),
            cancel,
        )? != item.size
        {
            return Err(io::Error::other("Ungültige entpackte Dateigröße."));
        }
        Ok(())
    })?;
    files::ensure_unchanged(path, &original)?;
    for (item, relative) in &entries {
        let directory = if item.directory {
            relative.as_path()
        } else {
            relative.parent().unwrap_or(Path::new(""))
        };
        let mut parent = base.clone();
        for component in directory.components() {
            parent.push(component);
            files::create_or_verify_directory(&parent)?;
        }
    }
    let count = outputs.len();
    for (output, relative) in outputs {
        files::check_cancel(cancel)?;
        output
            .persist_noclobber(base.join(relative))
            .map_err(|error| error.error)?;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lha_uses_original_paths_and_never_sanitizes_traversal_into_valid_names() {
        for name in [
            b"../outside".as_slice(),
            b"/absolute",
            b"C:/escape",
            b"dir/../../file",
            b"file:stream",
            b"dir\\..\\escape",
        ] {
            let header = delharc::LhaHeader {
                filename: name.into(),
                ..Default::default()
            };
            assert!(lha_item(&header).is_err(), "{name:?}");
        }
        let header = delharc::LhaHeader {
            filename: b"folder\\caf\x82.txt".as_slice().into(),
            ..Default::default()
        };
        assert_eq!(
            lha_item(&header).unwrap().path,
            Path::new("folder/café.txt")
        );
    }
}
