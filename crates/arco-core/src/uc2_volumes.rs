use crate::{archive_view, archives, files, legacy_archives, tools};
use std::{
    fs::{self, File, Metadata},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

const SAS_MAGIC: [u8; 4] = [0x45, 0x14, 0x02, 0x0f];
const SAS_HEADER: u32 = 14;
const SEA_MAGIC: &[u8] = b"UC2SFX Header\0";

enum Set {
    Sas {
        prefix: char,
        total: u32,
        selected: usize,
    },
    Sea {
        first: PathBuf,
        parts: usize,
    },
}

fn word(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().expect("4 bytes"))
}

fn short(bytes: &[u8]) -> u16 {
    u16::from_le_bytes(bytes.try_into().expect("2 bytes"))
}

fn sas_number(extension: &str) -> Option<(char, usize)> {
    let mut chars = extension.chars();
    let prefix = chars.next()?;
    let rest = chars.as_str();
    let number: usize = rest.parse().ok()?;
    if !matches!(prefix, 'p' | 'P')
        || !(1..=999).contains(&number)
        || rest != format!("{number:02}")
    {
        return None;
    }
    Some((prefix, number))
}

fn read_sas(path: &Path) -> io::Result<(u32, u32, usize)> {
    let metadata = files::regular_metadata(path)?;
    let mut header = [0; SAS_HEADER as usize];
    File::open(path)?.read_exact(&mut header)?;
    let total = word(&header[4..8]);
    let physical = word(&header[8..12]);
    if header[..4] != SAS_MAGIC
        || total == 0
        || physical <= SAS_HEADER
        || u64::from(physical) != metadata.len()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Ungültiger UC2-SAS-Teil: {}", path.display()),
        ));
    }
    Ok((
        total,
        physical - SAS_HEADER,
        usize::from(short(&header[12..14])),
    ))
}

fn sea_parts(path: &Path) -> Option<usize> {
    if !files::regular_metadata(path).ok()?.is_file() {
        return None;
    }
    let mut file = File::open(path).ok()?;
    let mut mz = [0; 32];
    file.read_exact(&mut mz).ok()?;
    let pages = u64::from(short(&mz[4..6]));
    let tail = u64::from(short(&mz[2..4]));
    if &mz[..2] != b"MZ" || pages == 0 || tail > 511 {
        return None;
    }
    let stub = (pages - 1) * 512 + if tail == 0 { 512 } else { tail };
    if !(32..=1024 * 1024).contains(&stub) {
        return None;
    }
    file.seek(SeekFrom::Start(stub)).ok()?;
    let mut config = Vec::new();
    file.take(65_536 + 37).read_to_end(&mut config).ok()?;
    config.windows(37).enumerate().find_map(|(banner, header)| {
        let parts = usize::from(short(&header[19..21]));
        (header.starts_with(SEA_MAGIC)
            && u64::from(word(&header[15..19])) == stub + banner as u64 + 37
            && usize::from(short(&header[35..37])) == banner
            && word(&header[29..33]) == 0xac28_3746
            && parts <= 999
            && word(&header[21..25]) > 0)
            .then_some(parts)
    })
}

fn detect(path: &Path) -> Option<Set> {
    let ext = path.extension()?.to_str()?;
    if let Some((prefix, selected)) = sas_number(ext) {
        let first = path.with_extension(format!("{prefix}01"));
        let (total, _, _) = read_sas(path).or_else(|_| read_sas(&first)).ok()?;
        return Some(Set::Sas {
            prefix,
            total,
            selected,
        });
    }
    let first = if ext.eq_ignore_ascii_case("exe") {
        path.to_path_buf()
    } else if ext.len() == 3 && ext.bytes().all(|byte| byte.is_ascii_digit()) {
        let upper = path.with_extension("EXE");
        if upper.is_file() {
            upper
        } else {
            path.with_extension("exe")
        }
    } else {
        return None;
    };
    let parts = sea_parts(&first)?;
    Some(Set::Sea { first, parts })
}

pub(crate) fn is_volume(path: &Path) -> bool {
    detect(path).is_some()
}

pub(crate) fn snapshot(path: &Path) -> io::Result<Vec<(PathBuf, Metadata)>> {
    match detect(path) {
        None => Ok(Vec::new()),
        Some(Set::Sas {
            prefix,
            total,
            selected,
        }) => {
            let mut remaining = total;
            let mut volumes = Vec::new();
            for index in 1..=999 {
                let volume = path.with_extension(format!("{prefix}{index:02}"));
                let metadata = files::regular_metadata(&volume)?;
                let (expected, size, number) = read_sas(&volume)?;
                if expected != total || size > remaining || number != index {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("UC2-SAS-Teil passt nicht zum Satz: {}", volume.display()),
                    ));
                }
                volumes.push((volume, metadata));
                remaining -= size;
                if remaining == 0 && selected <= index {
                    return Ok(volumes);
                }
            }
            Err(io::Error::other("Der UC2-SAS-Satz ist unvollständig."))
        }
        Some(Set::Sea { first, parts }) => std::iter::once(first.clone())
            .chain((1..=parts).map(|index| first.with_extension(format!("{index:03}"))))
            .map(|volume| {
                let metadata = files::regular_metadata(&volume)?;
                if !metadata.is_file() {
                    return Err(io::Error::other("Ein UC2SEA-Teil ist keine Datei."));
                }
                Ok((volume, metadata))
            })
            .collect(),
    }
}

pub(crate) fn unchanged(snapshot: &[(PathBuf, Metadata)]) -> io::Result<()> {
    for (path, metadata) in snapshot {
        files::ensure_unchanged(path, metadata)?;
    }
    Ok(())
}

pub fn pack_sea(
    sources: &[PathBuf],
    base: &Path,
    destination: &Path,
    volume_size: u64,
    cancel: &AtomicBool,
) -> io::Result<usize> {
    let stem = destination
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if !destination
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
        || !(1..=8).contains(&stem.len())
        || !stem
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
    {
        return Err(io::Error::other(
            "DOS-UC2SEA benötigt einen EXE-Namen mit höchstens acht ASCII-Zeichen.",
        ));
    }
    if !(32 * 1024..=512 * 1024 * 1024).contains(&volume_size) || volume_size % 1024 != 0 {
        return Err(io::Error::other(
            "DOS-UC2SEA benötigt Teilgrößen zwischen 32 KiB und 512 MiB.",
        ));
    }
    let parent = fs::canonicalize(
        destination
            .parent()
            .ok_or_else(|| io::Error::other("Zielordner fehlt."))?,
    )?;
    files::leaf_name(destination.file_name().expect("validated EXE name"))?;
    for entry in fs::read_dir(&parent)? {
        files::check_cancel(cancel)?;
        let path = entry?.path();
        if path
            .file_stem()
            .is_some_and(|name| name.eq_ignore_ascii_case(stem))
            && path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| {
                    ext.eq_ignore_ascii_case("exe")
                        || (ext.len() == 3 && ext.bytes().all(|byte| byte.is_ascii_digit()))
                })
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("DOS-UC2SEA-Ziel existiert bereits: {}", path.display()),
            ));
        }
    }
    for path in sources {
        if parent.starts_with(fs::canonicalize(path)?) {
            return Err(io::Error::other("Das Zielarchiv liegt im Quellordner."));
        }
    }
    let dosbox = tool_path("ARCO_DOSBOX_X", false)?;
    let tool = tool_path("ARCO_UC2SEA_DIR", true)?;
    let working = tempfile::tempdir_in(parent)?;
    let format = legacy_archives::Format::Uc2;
    let manifest = stage_sea_sources(sources, base, working.path(), cancel)?;
    for name in ["UC.EXE", "UC2SEA.EXE", "DOS.SEA"] {
        let source = tool.join(name);
        let metadata = files::regular_metadata(&source)?;
        if !metadata.is_file() {
            return Err(io::Error::other(format!("UC2SEA-Werkzeug fehlt: {name}")));
        }
        fs::copy(source, working.path().join(name))?;
    }
    let config = working.path().join("arco.conf");
    let volume_argument = volume_size / 1024;
    fs::write(
        &config,
        format!(
            "[sdl]\r\noutput=none\r\n[dosbox]\r\nmemsize=16\r\n[cpu]\r\ncycles=max\r\n[autoexec]\r\nmount c .\r\nc:\r\ncd data\r\nc:\\uc.exe as c:\\payload\r\ncd \\\r\nif exist payload.uc2 uc2sea payload -v{volume_argument}\r\nexit\r\n"
        ),
    )?;
    run_dosbox(&dosbox, working.path(), Duration::from_secs(900), cancel)?;
    let first = working.path().join("PAYLOAD.EXE");
    let count = sea_parts(&first)
        .ok_or_else(|| io::Error::other("UC2SEA hat keinen gültigen DOS-Satz erzeugt."))?;
    format.compare_sources(&first, &manifest, cancel)?;
    format.test(&first, cancel)?;
    let parts = std::iter::once((first.clone(), destination.to_path_buf()))
        .chain((1..=count).map(|index| {
            let extension = format!("{index:03}");
            (
                first.with_extension(&extension),
                destination.with_extension(extension),
            )
        }))
        .collect::<Vec<_>>();
    for (part, _) in &parts {
        if fs::metadata(part)?.len() > volume_size {
            return Err(io::Error::other(
                "Das DOS-Werkzeug hat die gewählte Teilgröße überschritten.",
            ));
        }
        File::options().write(true).open(part)?.sync_all()?;
    }
    tools::publish_parts(parts, cancel)
}

fn stage_sea_sources(
    sources: &[PathBuf],
    base: &Path,
    working: &Path,
    cancel: &AtomicBool,
) -> io::Result<PathBuf> {
    let plan = archives::plan_sources(sources, base, Path::new(""), cancel)?;
    let data = working.join("DATA");
    fs::create_dir(&data)?;
    let mut entries = Vec::with_capacity(plan.len());
    let mut total = 0u64;
    for (source, name, metadata) in plan {
        files::check_cancel(cancel)?;
        let relative = archives::checked_archive_path(&name)?;
        if relative.components().count() == 1
            && [
                "SEASTART.BAT",
                "SEASTART.CMD",
                "SEASTART.EXE",
                "SEASTART.COM",
            ]
            .iter()
            .any(|name| relative.as_os_str().eq_ignore_ascii_case(name))
        {
            return Err(io::Error::other(
                "DOS-UC2SEA unterstützt hier keine automatisch gestarteten SEASTART-Dateien.",
            ));
        }
        let target = data.join(relative);
        files::ensure_unchanged(&source, &metadata)?;
        if metadata.is_dir() {
            fs::create_dir_all(&target)?;
        } else {
            total = total
                .checked_add(metadata.len())
                .filter(|&size| size <= 512 * 1024 * 1024)
                .ok_or_else(|| {
                    io::Error::other(
                        "Die Quelldateien überschreiten das DOS-UC2SEA-Limit von 512 MiB.",
                    )
                })?;
            let mut file = File::options().write(true).create_new(true).open(&target)?;
            let size = tools::copy_cancelled(
                File::open(&source)?.take(metadata.len() + 1),
                &mut file,
                cancel,
            )?;
            if size != metadata.len() {
                return Err(io::Error::other(
                    "Eine DOS-UC2SEA-Quelldatei hat ihre Größe geändert.",
                ));
            }
            archive_view::preserve_timestamp(&file, metadata.modified().ok())?;
        }
        files::ensure_unchanged(&source, &metadata)?;
        entries.push((name, target, metadata.is_dir()));
    }
    let manifest = working.join("sources.json");
    serde_json::to_writer(File::create(&manifest)?, &entries).map_err(io::Error::other)?;
    Ok(manifest)
}

fn run_dosbox(
    dosbox: &Path,
    folder: &Path,
    timeout: Duration,
    cancel: &AtomicBool,
) -> io::Result<()> {
    files::check_cancel(cancel)?;
    let mut command = Command::new(dosbox);
    command
        .current_dir(folder)
        .args(["-conf", "arco.conf", "-nopromptfolder", "-silent"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command.spawn()?;
    let start = Instant::now();
    let result = (|| {
        loop {
            files::check_cancel(cancel)?;
            if let Some(status) = child.try_wait()? {
                return if status.success() {
                    Ok(())
                } else {
                    Err(io::Error::other(format!("DOSBox-X beendet mit {status}.")))
                };
            }
            if start.elapsed() >= timeout {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "DOSBox-X antwortet nicht mehr.",
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

fn tool_path(variable: &str, directory: bool) -> io::Result<PathBuf> {
    std::env::var_os(variable)
        .map(PathBuf::from)
        .filter(|path| {
            path.is_absolute()
                && if directory {
                    path.is_dir()
                } else {
                    path.is_file()
                }
        })
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("Für DOS-UC2SEA muss {variable} auf einen absoluten Pfad zeigen."),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires DOSBox-X, original DOS UC2SEA, and the Python UC2 adapter"]
    fn original_dos_encoder_creates_readable_volumes() {
        if std::env::var_os("ARCO_DOSBOX_X").is_none()
            || std::env::var_os("ARCO_UC2SEA_DIR").is_none()
            || std::env::var_os("ARCO_ARCHIVE_PYTHON").is_none()
        {
            return;
        }
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        let source = root.join("PROBE.DAT");
        let mut state = 1234u32;
        let data: Vec<_> = (0..128_000)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state as u8
            })
            .collect();
        fs::write(&source, &data).unwrap();
        let target = root.join("RESULT.EXE");
        let cancel = AtomicBool::new(false);
        let paths = std::slice::from_ref(&source);
        let count = pack_sea(paths, root, &target, 32 * 1024, &cancel).unwrap();
        assert!(count >= 3);
        let second = target.with_extension("001");
        let output = tempfile::tempdir().unwrap();
        let format = legacy_archives::Format::Uc2;
        assert_eq!(
            format
                .extract(&second, output.path(), None, &cancel)
                .unwrap(),
            1
        );
        assert_eq!(
            blake3::hash(&fs::read(output.path().join("PROBE.DAT")).unwrap()),
            blake3::hash(&data)
        );
        let dosbox = tool_path("ARCO_DOSBOX_X", false).unwrap();
        fs::write(
            root.join("arco.conf"),
            "[sdl]\r\noutput=none\r\n[dosbox]\r\nmemsize=16\r\n[cpu]\r\ncycles=max\r\n[autoexec]\r\nmount c .\r\nc:\r\nresult OUT\r\nexit\r\n",
        ).unwrap();
        run_dosbox(&dosbox, root, Duration::from_secs(30), &cancel).unwrap();
        assert_eq!(
            blake3::hash(&fs::read(root.join("OUT/PROBE.DAT")).unwrap()),
            blake3::hash(&data)
        );
        assert!(pack_sea(paths, root, &target, 32 * 1024, &cancel).is_err());
        let larger = root.join("LARGER.EXE");
        let count = pack_sea(paths, root, &larger, 96 * 1024, &cancel).unwrap();
        assert!(count >= 2);
        assert!(fs::metadata(larger).unwrap().len() <= 96 * 1024);
    }
}

pub fn pack_sas(
    sources: &[PathBuf],
    base: &Path,
    destination: &Path,
    volume_size: u64,
    cancel: &AtomicBool,
) -> io::Result<usize> {
    let part_size = u32::try_from(volume_size)
        .ok()
        .filter(|&size| size > SAS_HEADER)
        .ok_or_else(|| {
            io::Error::other("Die SAS-Teilgröße muss zwischen 15 Byte und 4 GiB liegen.")
        })?;
    let parent = fs::canonicalize(
        destination
            .parent()
            .ok_or_else(|| io::Error::other("Zielordner fehlt."))?,
    )?;
    let leaf = destination
        .file_name()
        .ok_or_else(|| io::Error::other("SAS-Archivname fehlt."))?;
    files::leaf_name(leaf)?;
    for source in sources {
        if parent.starts_with(fs::canonicalize(source)?) {
            return Err(io::Error::other("Das Zielarchiv liegt im Quellordner."));
        }
    }
    let working = tempfile::tempdir_in(&parent)?;
    let archive = working.path().join("payload.uc2");
    legacy_archives::pack_uc2(sources, base, &archive, cancel)?;
    let mut input = File::open(&archive)?;
    let total = u32::try_from(input.metadata()?.len())
        .map_err(|_| io::Error::other("Der UC2-SAS-Satz ist größer als 4 GiB."))?;
    let payload_size = part_size - SAS_HEADER;
    let count = total.div_ceil(payload_size) as usize;
    if count == 0 || count > 999 {
        return Err(io::Error::other("SAS unterstützt bis zu 999 Teile."));
    }
    let mut parts = Vec::with_capacity(count);
    for index in 1..=count {
        let mut name = leaf.to_os_string();
        name.push(format!(".p{index:02}"));
        let target = parent.join(&name);
        if target.try_exists()? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("SAS-Teil existiert bereits: {}", target.display()),
            ));
        }
        parts.push((working.path().join(name), target));
    }
    let mut remaining = total;
    for (index, (staged, _)) in parts.iter().enumerate() {
        files::check_cancel(cancel)?;
        let size = remaining.min(payload_size);
        let mut part = File::create(staged)?;
        part.write_all(&SAS_MAGIC)?;
        part.write_all(&total.to_le_bytes())?;
        part.write_all(&(size + SAS_HEADER).to_le_bytes())?;
        part.write_all(&((index + 1) as u16).to_le_bytes())?;
        let actual = tools::copy_cancelled((&mut input).take(u64::from(size)), &mut part, cancel)?;
        if actual != u64::from(size) {
            return Err(io::Error::other("UC2-Daten wurden abgeschnitten."));
        }
        part.sync_all()?;
        remaining -= size;
    }
    legacy_archives::Format::Uc2.test(&parts[0].0, cancel)?;
    tools::publish_parts(parts, cancel)
}
