use super::{Interface, pe_header};
use crate::{archives, files, profile, tools};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::AtomicBool;

impl Interface {
    pub fn for_path(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        match extension.strip_suffix("64").unwrap_or(&extension) {
            "wcx" => Some(Self::Wcx),
            "wdx" => Some(Self::Wdx),
            "wfx" => Some(Self::Wfx),
            "wlx" => Some(Self::Wlx),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Wcx => "WCX",
            Self::Wdx => "WDX",
            Self::Wfx => "WFX",
            Self::Wlx => "WLX",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Architecture {
    X86,
    X64,
}

impl Architecture {
    pub fn label(self) -> &'static str {
        match self {
            Self::X86 => "x86",
            Self::X64 => "x64",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Registration {
    pub name: String,
    pub path: PathBuf,
    pub interface: Interface,
    pub architecture: Architecture,
    pub enabled: bool,
    #[serde(default)]
    pub masks: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tc_detection: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<PathBuf>,
}

impl Registration {
    pub fn read(path: &Path, name: &str, masks: &str) -> io::Result<Self> {
        if !path.is_absolute() || !files::regular_metadata(path)?.is_file() {
            return Err(invalid(
                "Wähle eine reguläre Plugin-Datei mit absolutem Pfad.",
            ));
        }
        let interface = Interface::for_path(path).ok_or_else(|| {
            invalid("Erwartet werden WCX-, WFX-, WLX- oder WDX-Dateien für x86 oder x64.")
        })?;
        let path = fs::canonicalize(path)?;
        let header = pe_header(&path)?;
        let attributes = u16::from_le_bytes([header[22], header[23]]);
        if attributes & 0x2000 == 0 {
            return Err(invalid("Die ausgewählte Datei ist keine DLL."));
        }
        let architecture = match u16::from_le_bytes([header[4], header[5]]) {
            0x014c => Architecture::X86,
            0x8664 => Architecture::X64,
            _ => return Err(invalid("Die Plugin-Architektur ist weder x86 noch x64.")),
        };
        let default_name = path.file_stem().unwrap_or_default().to_string_lossy();
        let mut registration = Self {
            name: String::new(),
            path: path.clone(),
            interface,
            architecture,
            enabled: false,
            masks: String::new(),
            tc_detection: None,
            package: None,
        };
        registration.update(
            if name.trim().is_empty() {
                &default_name
            } else {
                name
            },
            masks,
        )?;
        Ok(registration)
    }

    pub fn update(&mut self, name: &str, masks: &str) -> io::Result<()> {
        let name = name.trim();
        let masks = masks.trim();
        if name.is_empty() || name.chars().count() > 160 || name.chars().any(char::is_control) {
            return Err(invalid("Gib einen Plugin-Namen mit 1 bis 160 Zeichen ein."));
        }
        if masks.len() > 2048
            || masks.contains(['/', '\\', ':'])
            || masks.chars().any(char::is_control)
        {
            return Err(invalid("Verwende Dateinamensmasken ohne Verzeichnisteile."));
        }
        files::compile_masks(masks)?;
        self.name = name.into();
        self.masks = masks.into();
        Ok(())
    }

    pub fn matches(&self, interface: Interface, file: &Path) -> bool {
        self.enabled
            && self.interface == interface
            && (interface == Interface::Wfx
                || self.masks.is_empty()
                || file.file_name().is_some_and(|name| {
                    files::compile_masks(&self.masks).is_ok_and(|masks| masks.is_match(name))
                }))
    }

    pub fn same_file(&self, other: &Self) -> bool {
        self.path
            .as_os_str()
            .eq_ignore_ascii_case(other.path.as_os_str())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackagePhase {
    Reading,
    Extracting,
    Checking,
}

pub struct PackageProgress<'a> {
    pub phase: PackagePhase,
    pub path: &'a Path,
    pub completed: u64,
    pub total: Option<u64>,
}

pub struct Package {
    pub registrations: Vec<Registration>,
    pub version: String,
    workspace: tempfile::TempDir,
}

impl Package {
    pub fn read(path: &Path, root: &Path, cancel: &AtomicBool) -> io::Result<Self> {
        Self::read_with_progress(path, root, cancel, |_| {})
    }

    pub fn read_with_progress(
        path: &Path,
        root: &Path,
        cancel: &AtomicBool,
        mut progress: impl FnMut(PackageProgress<'_>),
    ) -> io::Result<Self> {
        progress(PackageProgress {
            phase: PackagePhase::Reading,
            path,
            completed: 0,
            total: None,
        });
        files::check_cancel(cancel)?;
        const MAX_ZIP: u64 = 256 * 1024 * 1024;
        const MAX_OUTPUT: u64 = 512 * 1024 * 1024;
        if !path.is_absolute()
            || !path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
        {
            return Err(invalid(
                "Wähle den absoluten Pfad zu einem ZIP-Plugin-Paket.",
            ));
        }
        let mut source = tempfile::NamedTempFile::new()?;
        let copied =
            tools::copy_cancelled(File::open(path)?.take(MAX_ZIP + 1), &mut source, cancel)?;
        if copied > MAX_ZIP {
            return Err(invalid("Das Plugin-Paket überschreitet 256 MiB."));
        }
        let members = archives::list_zip(source.path(), cancel)?;
        let expanded = members
            .iter()
            .fold(0u64, |bytes, item| bytes.saturating_add(item.size));
        if expanded > MAX_OUTPUT
            || members.len() > 4096
            || members.iter().any(|item| item.encrypted)
        {
            return Err(invalid(
                "Das ZIP ist verschlüsselt oder überschreitet das Installationslimit.",
            ));
        }
        let manifest = members
            .iter()
            .find(|item| {
                !item.directory && item.path.as_os_str().eq_ignore_ascii_case("pluginst.inf")
            })
            .filter(|item| item.size <= 128 * 1024)
            .ok_or_else(|| invalid("Das ZIP benötigt eine pluginst.inf im Hauptverzeichnis."))?;
        fs::create_dir_all(root)?;
        let workspace = tempfile::Builder::new()
            .prefix("package-")
            .tempdir_in(root)?;
        archives::extract_zip_selection_with_progress(
            source.path(),
            workspace.path(),
            None,
            None,
            cancel,
            |p| {
                progress(PackageProgress {
                    phase: PackagePhase::Extracting,
                    path: p.path,
                    completed: p.copied,
                    total: Some(p.total),
                });
            },
        )?;
        progress(PackageProgress {
            phase: PackagePhase::Checking,
            path,
            completed: 0,
            total: None,
        });
        files::check_cancel(cancel)?;
        let text = profile::read_tc_text(&workspace.path().join(&manifest.path))?;
        let values: BTreeMap<_, _> = profile::ini_entries(&text)
            .filter(|(section, _, _)| section == "plugininstall")
            .map(|(_, key, value)| (key, value))
            .collect();
        let value = |key: &str| values.get(key).copied().unwrap_or_default();
        let interface = match value("type").to_ascii_lowercase().as_str() {
            "wcx" => Interface::Wcx,
            "wfx" => Interface::Wfx,
            "wlx" => Interface::Wlx,
            "wdx" => Interface::Wdx,
            _ => {
                return Err(invalid(
                    "Das Manifest nennt keinen unterstützten Plugin-Typ.",
                ));
            }
        };
        let module = archives::checked_archive_path(&value("file").replace('\\', "/"))?;
        if Interface::for_path(&module) != Some(interface) {
            return Err(invalid("Die Plugin-Datei passt nicht zum Typ im Manifest."));
        }
        let extension = interface.label().to_ascii_lowercase();
        let mut registrations = Vec::new();
        let masks = extension_masks(value("defaultextension").split(','));
        let name = value("descriptiondeu").trim();
        let name = if name.is_empty() {
            value("description")
        } else {
            name
        };
        let name: String = name
            .chars()
            .filter(|character| !character.is_control())
            .take(160)
            .collect();
        let package = fs::canonicalize(workspace.path())?;
        for extension in [format!("{extension}64"), extension] {
            let expected = module.with_extension(extension);
            if let Some(item) = members.iter().find(|item| {
                !item.directory
                    && item
                        .path
                        .as_os_str()
                        .eq_ignore_ascii_case(expected.as_os_str())
            }) {
                let mut registration =
                    Registration::read(&package.join(&item.path), &name, &masks)?;
                registration.package = Some(package.clone());
                registrations.push(registration);
            }
        }
        if registrations.is_empty() {
            return Err(invalid("Die Plugin-Datei aus dem Manifest fehlt im ZIP."));
        }
        registrations.sort_by_key(|item| item.architecture == Architecture::X86);
        Ok(Self {
            registrations,
            version: value("version").into(),
            workspace,
        })
    }

    pub fn keep(self) {
        let _ = self.workspace.keep();
    }
}

pub fn remove_package(package: &Path, root: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(package)?;
    if !metadata.is_dir()
        || files::is_link(&metadata)
        || !package
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.starts_with("package-"))
    {
        return Err(invalid("Das Plugin-Paketverzeichnis ist ungültig."));
    }
    let package = fs::canonicalize(package)?;
    if package.parent() != Some(fs::canonicalize(root)?.as_path()) {
        return Err(invalid(
            "Das Paketverzeichnis liegt außerhalb des Plugin-Ordners.",
        ));
    }
    fs::remove_dir_all(package)
}

pub struct TcRegistrations {
    pub plugins: Vec<Registration>,
    pub skipped: usize,
}

pub fn read_tc_plugins(path: &Path, commander_root: Option<&Path>) -> io::Result<TcRegistrations> {
    let text = profile::read_tc_text(path)?;
    let entries: Vec<_> = profile::ini_raw_entries(&text)
        .map(|(section, key, value)| (section, key.to_ascii_lowercase(), value, key))
        .collect();
    let detections: BTreeMap<_, _> = entries
        .iter()
        .filter_map(|(section, key, value, _)| {
            Some(((section.as_str(), key.strip_suffix("_detect")?), *value))
        })
        .collect();
    let mut ordered: Vec<_> = entries.iter().collect();
    ordered
        .sort_by_key(|(section, key, _, _)| (section, key.parse::<usize>().unwrap_or(usize::MAX)));
    let mut plugins: Vec<Registration> = Vec::new();
    let mut skipped = 0;
    for (section, key, value, original_key) in ordered {
        if key == "redirectsection" {
            skipped += usize::from(matches!(
                section.as_str(),
                "packerplugins" | "filesystemplugins" | "listerplugins" | "contentplugins"
            ));
            continue;
        }
        let (interface, module, masks, name) = match section.as_str() {
            "packerplugins" => {
                let Some((_, module)) = value.split_once(',') else {
                    skipped += 1;
                    continue;
                };
                (Interface::Wcx, module, extension_masks([key.as_str()]), "")
            }
            "filesystemplugins" if !key.starts_with('$') => {
                (Interface::Wfx, *value, String::new(), *original_key)
            }
            "listerplugins" if key.parse::<usize>().is_ok() => {
                (Interface::Wlx, *value, String::new(), "")
            }
            "contentplugins" if key.parse::<usize>().is_ok() => {
                (Interface::Wdx, *value, String::new(), "")
            }
            _ => continue,
        };
        let Some(module) = resolve_tc_module(module, commander_root) else {
            skipped += 1;
            continue;
        };
        if Interface::for_path(&module) != Some(interface) {
            skipped += 1;
            continue;
        }
        let detection = detections
            .get(&(section.as_str(), key.as_str()))
            .copied()
            .filter(|value| !value.is_empty())
            .unwrap_or_default();
        let masks = if masks.is_empty() {
            detection_masks(detection)
        } else {
            masks
        };
        let extension = interface.label().to_ascii_lowercase();
        let paired = module
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case(&extension));
        let companion = paired.then(|| module.with_extension(format!("{extension}64")));
        let mut matched = false;
        for path in companion.iter().chain([&module]) {
            if let Ok(mut registration) = Registration::read(path, name, &masks) {
                matched = true;
                registration.tc_detection = (!detection.is_empty()).then(|| detection.into());
                if let Some(existing) = plugins
                    .iter_mut()
                    .find(|item| item.same_file(&registration))
                {
                    if !masks.is_empty() && !existing.masks.split(';').any(|part| part == masks) {
                        let combined = [existing.masks.as_str(), &masks].join(";");
                        let label = existing.name.clone();
                        if existing.update(&label, &combined).is_err() {
                            skipped += 1;
                        }
                    }
                } else {
                    plugins.push(registration);
                }
            }
        }
        skipped += usize::from(!matched);
    }
    Ok(TcRegistrations { plugins, skipped })
}

fn detection_masks(rule: &str) -> String {
    let extensions: Option<Vec<_>> = rule
        .split('|')
        .map(|clause| {
            let (field, value) = clause.split_once('=')?;
            if !field.trim().eq_ignore_ascii_case("EXT") {
                return None;
            }
            let value = value.trim().strip_prefix('"')?.strip_suffix('"')?;
            (!value.is_empty()
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'))
            .then_some(value)
        })
        .collect();
    extensions.map(extension_masks).unwrap_or_default()
}

fn resolve_tc_module(value: &str, root: Option<&Path>) -> Option<PathBuf> {
    let value = value.trim().trim_matches('"');
    if let Some(rest) = value.get(16..)
        && value.get(..16)?.eq_ignore_ascii_case("%COMMANDER_PATH%")
    {
        let relative = Path::new(rest.trim_start_matches(['\\', '/']));
        if relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
        {
            return root.map(|root| root.join(relative));
        }
        return None;
    }
    Some(PathBuf::from(value))
}

fn extension_masks<'a>(extensions: impl IntoIterator<Item = &'a str>) -> String {
    extensions
        .into_iter()
        .map(|extension| extension.trim().trim_start_matches('.'))
        .filter(|extension| {
            !extension.is_empty()
                && extension
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        })
        .map(|extension| format!("*.{extension}"))
        .collect::<Vec<_>>()
        .join(";")
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::{ZipWriter, write::SimpleFileOptions};

    #[test]
    fn package_progress_tracks_extraction_and_cancelled_staging_is_discarded() {
        use std::sync::atomic::Ordering;
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("progress.zip");
        let root = fixture.path().join("plugins");
        let mut zip = ZipWriter::new(File::create(&path).unwrap());
        zip.start_file("pluginst.inf", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"[plugininstall]\ntype=wcx\nfile=demo.wcx64\ndescription=Fixture")
            .unwrap();
        zip.start_file("demo.wcx64", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(&header(0x8664, true)).unwrap();
        zip.finish().unwrap();
        let cancel = AtomicBool::new(false);
        let mut updates = Vec::new();
        let package = Package::read_with_progress(&path, &root, &cancel, |p| {
            updates.push((p.phase, p.completed, p.total));
        })
        .unwrap();
        assert_eq!(package.registrations.len(), 1);
        assert_eq!(updates.first().unwrap().0, PackagePhase::Reading);
        assert!(updates.iter().any(|p| p.0 == PackagePhase::Extracting
            && p.2.is_some_and(|total| total > 0 && p.1 == total)));
        assert_eq!(updates.last().unwrap(), &(PackagePhase::Checking, 0, None));
        drop(package);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        let error = Package::read_with_progress(&path, &root, &cancel, |p| {
            if p.phase == PackagePhase::Extracting {
                cancel.store(true, Ordering::Relaxed);
            }
        })
        .err()
        .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        assert!(path.is_file());
    }

    fn header(machine: u16, library: bool) -> [u8; 88] {
        let mut bytes = [0; 88];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[60] = 64;
        bytes[64..68].copy_from_slice(b"PE\0\0");
        bytes[68..70].copy_from_slice(&machine.to_le_bytes());
        if library {
            bytes[87] = 0x20;
        }
        bytes
    }

    #[test]
    fn package_stages_both_architectures_and_removes_only_managed_directories() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("demo.zip");
        let root = fixture.path().join("plugins");
        let mut zip = ZipWriter::new(File::create(&path).unwrap());
        let manifest = b"[plugininstall]\ntype=wcx\nfile=bin/demo.wcx\ndescription=Demo\ndefaultextension=one,two";
        for (name, contents) in [
            ("pluginst.inf", manifest.as_slice()),
            ("bin/demo.wcx", header(0x014c, true).as_slice()),
            ("bin/demo.wcx64", header(0x8664, true).as_slice()),
        ] {
            zip.start_file(name, SimpleFileOptions::default()).unwrap();
            zip.write_all(contents).unwrap();
        }
        zip.finish().unwrap();
        let package = Package::read(&path, &root, &AtomicBool::new(false)).unwrap();
        assert_eq!(package.registrations.len(), 2);
        assert_eq!(package.registrations[0].architecture, Architecture::X64);
        assert_eq!(package.registrations[1].architecture, Architecture::X86);
        let mut entry = package.registrations[0].clone();
        assert!(!entry.matches(Interface::Wcx, Path::new("sample.ONE")));
        entry.enabled = true;
        assert!(entry.matches(Interface::Wcx, Path::new("sample.ONE")));
        assert!(!entry.matches(Interface::Wcx, Path::new("sample.THREE")));
        assert!(!entry.matches(Interface::Wlx, Path::new("sample.ONE")));
        let managed = entry.package.unwrap();
        package.keep();
        let foreign = fixture.path().join("package-external");
        fs::create_dir(&foreign).unwrap();
        assert!(remove_package(&foreign, &root).is_err());
        assert!(foreign.exists());
        remove_package(&managed, &root).unwrap();
        assert!(!managed.exists());
    }

    #[cfg(windows)]
    #[test]
    fn ini_import_merges_packer_extensions_and_preserves_lister_rule() {
        let fixture = tempfile::tempdir().unwrap();
        fs::write(fixture.path().join("demo.wcx"), header(0x014c, true)).unwrap();
        fs::write(fixture.path().join("demo.wlx64"), header(0x8664, true)).unwrap();
        fs::write(fixture.path().join("not_a_dll.wfx"), header(0x8664, false)).unwrap();
        let path = fixture.path().join("wincmd.ini");
        fs::write(
            &path,
            r#"[PackerPlugins]
one=0,%COMMANDER_PATH%\demo.wcx
two=0,%COMMANDER_PATH%\demo.wcx
[ListerPlugins]
0=%COMMANDER_PATH%\demo.wlx64
0_detect=EXT="ABC"
[FileSystemPlugins]
Fake=%COMMANDER_PATH%\not_a_dll.wfx
"#,
        )
        .unwrap();
        let report = read_tc_plugins(&path, Some(fixture.path())).unwrap();
        assert_eq!(report.plugins.len(), 2);
        assert_eq!(report.skipped, 1);
        assert!(report.plugins.iter().all(|entry| !entry.enabled));
        let packer = report
            .plugins
            .iter()
            .find(|entry| entry.interface == Interface::Wcx)
            .unwrap();
        assert_eq!(packer.masks, "*.one;*.two");
        let lister = report
            .plugins
            .iter()
            .find(|entry| entry.interface == Interface::Wlx)
            .unwrap();
        assert_eq!(lister.tc_detection.as_deref(), Some("EXT=\"ABC\""));
        assert_eq!(lister.masks, "*.ABC");
        assert_eq!(detection_masks("EXT=\"TXT\" | EXT=\"LOG\""), "*.TXT;*.LOG");
        assert!(detection_masks("EXT=\"TXT\" & SIZE<2048").is_empty());
    }
}
