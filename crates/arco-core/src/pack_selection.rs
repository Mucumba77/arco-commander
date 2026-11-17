//! Shared selection semantics for every packer, independent of its format.
use crate::{archives, files, tools};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs::{self, File, Metadata},
    io,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Options {
    pub paths: bool,
    pub recursive: bool,
    pub separate: bool,
    pub omit_base: bool,
    pub move_sources: bool,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            paths: true,
            recursive: true,
            separate: false,
            omit_base: false,
            move_sources: false,
        }
    }
}

pub struct Unit {
    pub sources: Vec<PathBuf>,
    pub target: PathBuf,
}
pub struct Batch {
    pub units: Vec<Unit>,
    base: PathBuf,
    options: Options,
    snapshot: Vec<(PathBuf, Metadata)>,
}
pub struct Prepared {
    _storage: Option<tempfile::TempDir>,
    pub base: PathBuf,
    pub sources: Vec<PathBuf>,
}

impl Batch {
    pub fn new(
        sources: &[PathBuf],
        base: &Path,
        target: &Path,
        options: Options,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        if sources.is_empty() {
            return Err(io::Error::other("Wähle mindestens eine Archivquelle."));
        }
        if options.move_sources && !options.recursive {
            return Err(io::Error::other(
                "Beim Verschieben ins Archiv müssen Unterordner vollständig mitgepackt werden.",
            ));
        }
        let parent = fs::canonicalize(
            target
                .parent()
                .ok_or_else(|| io::Error::other("Archivziel fehlt."))?,
        )?;
        let mut selected = Vec::new();
        for source in sources {
            files::regular_metadata(source)?;
            let source = fs::canonicalize(source)?;
            if parent.starts_with(&source) {
                return Err(io::Error::other("Das Zielarchiv liegt im Quellordner."));
            }
            if !selected.contains(&source) {
                selected.push(source);
            }
        }
        selected.sort_by_key(|source| source.components().count());
        let mut roots: Vec<PathBuf> = Vec::new();
        for source in selected {
            if !roots.iter().any(|root| source.starts_with(root)) {
                roots.push(source);
            }
        }
        let base = fs::canonicalize(base)?;
        let plan = archives::plan_sources(&roots, &base, Path::new(""), cancel)?;
        let snapshot = plan
            .into_iter()
            .map(|(path, _, metadata)| (path, metadata))
            .collect();
        let mut units = Vec::new();
        if options.separate {
            let extension = target.extension().and_then(|ext| ext.to_str());
            let mut names = BTreeSet::new();
            for source in roots {
                let leaf = source
                    .file_name()
                    .ok_or_else(|| io::Error::other("Quellname fehlt."))?
                    .to_string_lossy();
                let name = extension.map_or_else(
                    || leaf.to_string(),
                    |extension| format!("{leaf}.{extension}"),
                );
                let name = tools::output_name(&name)?;
                if !names.insert(name.to_string_lossy().to_lowercase()) {
                    return Err(io::Error::other(
                        "Mehrere Quellen würden denselben Archivnamen erhalten.",
                    ));
                }
                units.push(Unit {
                    sources: vec![source],
                    target: parent.join(name),
                });
            }
        } else {
            units.push(Unit {
                sources: roots,
                target: target.to_path_buf(),
            });
        }
        for unit in &units {
            files::check_cancel(cancel)?;
            if unit.target.try_exists()? {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("Zielarchiv existiert bereits: {}", unit.target.display()),
                ));
            }
        }
        Ok(Self {
            units,
            base,
            options,
            snapshot,
        })
    }

    pub fn prepare(&self, unit: &Unit, cancel: &AtomicBool) -> io::Result<Prepared> {
        files::check_cancel(cancel)?;
        if self.options.paths && self.options.recursive && !self.options.omit_base {
            return Ok(Prepared {
                _storage: None,
                base: self.base.clone(),
                sources: unit.sources.clone(),
            });
        }
        let storage = tempfile::tempdir()?;
        let plan = archives::plan_sources(&unit.sources, &self.base, Path::new(""), cancel)?;
        let mut names = BTreeSet::new();
        for (source, name, metadata) in plan {
            files::check_cancel(cancel)?;
            let root = unit
                .sources
                .iter()
                .find(|root| source.starts_with(root))
                .ok_or_else(|| io::Error::other("Archivquelle liegt außerhalb der Auswahl."))?;
            let relative = source.strip_prefix(root).map_err(io::Error::other)?;
            let depth = relative.components().count();
            if !self.options.recursive && (depth > 1 || depth == 1 && metadata.is_dir()) {
                continue;
            }
            let name = if !self.options.paths {
                PathBuf::from(
                    source
                        .file_name()
                        .ok_or_else(|| io::Error::other("Quellname fehlt."))?,
                )
            } else if self.options.omit_base && metadata.is_dir() && source == *root {
                continue;
            } else if self.options.omit_base && root.is_dir() {
                relative.to_path_buf()
            } else {
                PathBuf::from(name)
            };
            // With paths disabled, only empty directories need an entry.
            if !self.options.paths && metadata.is_dir() && fs::read_dir(&source)?.next().is_some() {
                continue;
            }
            if !names.insert(name.to_string_lossy().to_lowercase()) {
                return Err(io::Error::other(
                    "Die Auswahl enthält kollidierende Namen ohne Pfad. Aktiviere Pfadnamen.",
                ));
            }
            let target = storage.path().join(&name);
            if metadata.is_dir() {
                fs::create_dir_all(&target)?;
            } else {
                fs::create_dir_all(
                    target
                        .parent()
                        .ok_or_else(|| io::Error::other("Zielordner fehlt."))?,
                )?;
                let mut output = File::options().create_new(true).write(true).open(&target)?;
                files::ensure_unchanged(&source, &metadata)?;
                let size = tools::copy_cancelled(File::open(&source)?, &mut output, cancel)?;
                files::ensure_unchanged(&source, &metadata)?;
                if size != metadata.len() {
                    return Err(io::Error::other("Archivquelle hat ihre Größe geändert."));
                }
                crate::archive_view::preserve_timestamp(&output, metadata.modified().ok())?;
                output.sync_all()?;
            }
        }
        let base = storage.path().to_path_buf();
        let mut sources = fs::read_dir(&base)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<io::Result<Vec<_>>>()?;
        sources.sort();
        Ok(Prepared {
            _storage: Some(storage),
            base,
            sources,
        })
    }

    pub fn check_sources(&self, cancel: &AtomicBool) -> io::Result<()> {
        for (path, metadata) in &self.snapshot {
            files::check_cancel(cancel)?;
            files::ensure_unchanged(path, metadata)?;
        }
        Ok(())
    }

    /// Call only after every unit has been created and verified successfully.
    pub fn finish(
        &self,
        outputs: &[PathBuf],
        password: Option<&str>,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        self.check_sources(cancel)?;
        if !self.options.move_sources {
            return Ok(());
        }
        if outputs.len() != self.units.len() {
            return Err(io::Error::other(
                "Nicht alle Archive wurden fertiggestellt. Die Quellen bleiben erhalten.",
            ));
        }
        for target in outputs {
            crate::archive_view::test(target, password, cancel)?;
        }
        self.check_sources(cancel)?;
        let mut entries: Vec<_> = self.snapshot.iter().collect();
        entries.sort_by_key(|(path, _)| std::cmp::Reverse(path.components().count()));
        for (path, metadata) in entries {
            files::check_cancel(cancel)?;
            if metadata.is_dir() {
                fs::remove_dir(path)?;
            } else {
                files::ensure_unchanged(path, metadata)?;
                fs::remove_file(path)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn separate_archives_recursion_and_base_omission_have_consistent_names() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("Projekt");
        fs::create_dir(&source).unwrap();
        fs::create_dir(source.join("tiefer")).unwrap();
        fs::write(source.join("direkt.txt"), b"direct").unwrap();
        fs::write(source.join("tiefer/datei.txt"), b"deep").unwrap();
        let cancel = AtomicBool::new(false);
        let batch = Batch::new(
            std::slice::from_ref(&source),
            root.path(),
            &root.path().join("target.zip"),
            Options {
                separate: true,
                omit_base: true,
                ..Default::default()
            },
            &cancel,
        )
        .unwrap();
        assert_eq!(batch.units[0].target.file_name().unwrap(), "Projekt.zip");
        let prepared = batch.prepare(&batch.units[0], &cancel).unwrap();
        assert!(prepared.base.join("direkt.txt").is_file());
        assert!(prepared.base.join("tiefer/datei.txt").is_file());
        assert!(!prepared.base.join("Projekt").exists());
        let shallow = Batch::new(
            std::slice::from_ref(&source),
            root.path(),
            &root.path().join("shallow.zip"),
            Options {
                recursive: false,
                ..Default::default()
            },
            &cancel,
        )
        .unwrap();
        let prepared = shallow.prepare(&shallow.units[0], &cancel).unwrap();
        assert!(prepared.base.join("Projekt/direkt.txt").is_file());
        assert!(!prepared.base.join("Projekt/tiefer").exists());
        assert!(
            Batch::new(
                &[source],
                root.path(),
                &root.path().join("bad.zip"),
                Options {
                    recursive: false,
                    move_sources: true,
                    ..Default::default()
                },
                &cancel
            )
            .is_err()
        );
    }

    #[test]
    fn flattened_name_collisions_and_cancellation_never_change_sources() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("Projekt");
        fs::create_dir(&source).unwrap();
        fs::create_dir(source.join("sub")).unwrap();
        fs::write(source.join("same.txt"), b"first").unwrap();
        fs::write(source.join("sub/same.txt"), b"second").unwrap();
        let cancel = AtomicBool::new(false);
        let batch = Batch::new(
            std::slice::from_ref(&source),
            root.path(),
            &root.path().join("out.zip"),
            Options {
                paths: false,
                ..Default::default()
            },
            &cancel,
        )
        .unwrap();
        assert!(batch.prepare(&batch.units[0], &cancel).is_err());
        assert_eq!(fs::read(source.join("same.txt")).unwrap(), b"first");
        assert_eq!(fs::read(source.join("sub/same.txt")).unwrap(), b"second");
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            batch
                .prepare(&batch.units[0], &cancel)
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::Interrupted
        );
    }

    #[test]
    fn moving_into_an_archive_requires_complete_verified_outputs_and_unchanged_sources() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("sample.txt");
        fs::write(&source, b"original").unwrap();
        let target = root.path().join("out.zip");
        let cancel = AtomicBool::new(false);
        let batch = Batch::new(
            std::slice::from_ref(&source),
            root.path(),
            &target,
            Options {
                move_sources: true,
                ..Default::default()
            },
            &cancel,
        )
        .unwrap();
        assert!(batch.finish(&[], None, &cancel).is_err());
        assert!(source.exists());
        fs::write(&target, b"not an archive").unwrap();
        assert!(
            batch
                .finish(std::slice::from_ref(&target), None, &cancel)
                .is_err()
        );
        assert!(source.exists());
        fs::remove_file(&target).unwrap();
        archives::create_zip(
            std::slice::from_ref(&source),
            root.path(),
            &target,
            Some("secret"),
            &cancel,
        )
        .unwrap();
        assert!(
            batch
                .finish(std::slice::from_ref(&target), Some("wrong"), &cancel)
                .is_err()
        );
        assert!(source.exists());
        batch
            .finish(std::slice::from_ref(&target), Some("secret"), &cancel)
            .unwrap();
        assert!(!source.exists());
        crate::archive_view::test(&target, Some("secret"), &cancel).unwrap();
        fs::write(&source, b"original").unwrap();
        let changed = Batch::new(
            std::slice::from_ref(&source),
            root.path(),
            &root.path().join("another.zip"),
            Options {
                move_sources: true,
                ..Default::default()
            },
            &cancel,
        )
        .unwrap();
        fs::write(&source, b"edited after planning").unwrap();
        assert!(changed.finish(&[target], Some("secret"), &cancel).is_err());
        assert_eq!(fs::read(&source).unwrap(), b"edited after planning");
    }
}
