use super::*;
use arco_core::archive_view::{self, Catalog};
use std::collections::{BTreeMap, BTreeSet};

/// Each member retains a real archive-relative path and a separate container identity.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct Scope {
    pub file: PathBuf,
    pub roots: Vec<PathBuf>,
}

pub(super) fn entry_path(entry: &Entry) -> String {
    entry.archive_file.as_ref().map_or_else(
        || display_path(&entry.path),
        |file| format!("{}::{}", display_path(file), display_path(&entry.path)),
    )
}

pub(super) fn refine(setup: &mut Setup, entries: &[Entry]) {
    let mut archives = BTreeMap::<PathBuf, Vec<PathBuf>>::new();
    setup.selected.clear();
    for entry in entries {
        if let Some(file) = &entry.archive_file {
            archives
                .entry(file.as_ref().clone())
                .or_default()
                .push(entry.path.clone());
        } else {
            setup.selected.push(entry.path.clone());
        }
    }
    setup.archive_scopes = archives
        .into_iter()
        .map(|(file, roots)| Scope { file, roots })
        .collect();
    setup.refine = true;
}

impl SearchTask {
    pub(super) fn run_global_archives(
        mut self,
        cancel: &AtomicBool,
        progress: &mut impl FnMut(Update),
    ) -> io::Result<Found> {
        let discover = self.options.archives && !self.query.searches_only_roots();
        let indexed = self.indexed.is_some();
        self.options.archives = false;
        let mut scopes = std::mem::take(&mut self.archive_scopes)
            .into_iter()
            .map(|scope| (scope.file, scope.roots))
            .collect::<BTreeMap<_, _>>();
        let mut examined = 0usize;
        let show_hidden = self.show_hidden;
        let ignore = self.ignore.clone();
        let mut observe = |update: &Update| {
            examined = examined.max(update.examined);
            if discover {
                let path = Path::new(&update.path);
                if archive_view::supports_text_search(path)
                    && let Ok(entry) = Entry::read(
                        path.to_path_buf(),
                        path.file_name().unwrap_or_default().to_owned(),
                    )
                    && entry.kind == Kind::File
                    && (show_hidden || !entry.hidden)
                    && ignore
                        .as_ref()
                        .is_none_or(|ignore| !ignore.local(path, false))
                {
                    scopes
                        .entry(path.to_path_buf())
                        .or_insert_with(|| vec![PathBuf::new()]);
                }
            }
        };
        // With Everything the progress stream contains no filesystem candidates. Discover
        // containers in the selected local search roots separately, without reading content.
        if discover && indexed {
            let scan_options = Options {
                names: fallback_names(
                    self.options
                        .everything()
                        .map_or("", |(_, expression)| expression),
                )
                .unwrap_or_default(),
                depth: self.options.depth.clone(),
                ..Default::default()
            };
            let scan = advanced_search::search_with_progress(
                &self.roots,
                self.options.selected_only,
                &scan_options.compile()?,
                self.show_hidden,
                self.ignore.as_deref(),
                cancel,
                |value| {
                    let update = Update {
                        path: value.path.as_os_str().to_owned(),
                        examined: value.examined,
                        matched: None,
                        errors: value.errors,
                        phase: "Archive ermitteln …".into(),
                    };
                    observe(&update);
                    progress(update);
                },
            );
            if scan.cancelled {
                return Ok(scan);
            }
        }
        let roots = self.roots.clone();
        let query = self.query.clone();
        let wdx = self.wdx.take();
        let ignore = self.ignore.clone();
        let mut local = SearchTask {
            query: query.clone(),
            roots: roots.clone(),
            options: self.options.clone(),
            indexed: self.indexed.take(),
            show_hidden: self.show_hidden,
            ignore,
            executable: self.executable.take(),
            archive: None,
            archive_scopes: Vec::new(),
            password: self.password.clone(),
            wdx,
        };
        let mut found = local.run_local(cancel, &mut |update| {
            observe(&update);
            progress(update);
        })?;
        drop(observe);
        if let Some(wdx) = &local.wdx {
            if found.cancelled {
                found.directory.entries.clear();
            } else {
                wdx.filter(&mut found.directory.entries, cancel)?;
            }
        }
        self.wdx = local.wdx.take();
        for (file, roots) in scopes {
            if cancel.load(Ordering::Relaxed) {
                found.cancelled = true;
                break;
            }
            progress(Update {
                path: file.as_os_str().to_owned(),
                examined,
                matched: Some(found.directory.entries.len()),
                errors: found.directory.errors.len(),
                phase: "Archiv öffnen …".into(),
            });
            let password = self.password.clone();
            let result = (|| {
                let catalog = Catalog::read_with_password(
                    &file,
                    password.as_deref().map(|password| password.as_str()),
                    cancel,
                )?;
                let archive = ArchiveTab {
                    file: file.clone(),
                    folder: PathBuf::new(),
                    catalog: Arc::new(catalog),
                    password: self.password.clone(),
                };
                self.roots = roots;
                let offset = examined;
                self.run_archive(&archive, cancel, &mut |mut update| {
                    examined = examined.max(offset.saturating_add(update.examined));
                    update.examined = examined;
                    update.matched = update
                        .matched
                        .map(|count| count + found.directory.entries.len());
                    update.errors += found.directory.errors.len();
                    progress(update);
                })
            })();
            match result {
                Ok(mut result) => {
                    let origin = Arc::new(file.clone());
                    for entry in &mut result.directory.entries {
                        entry.archive_file = Some(origin.clone());
                    }
                    found.directory.entries.extend(result.directory.entries);
                    found.directory.errors.extend(
                        result
                            .directory
                            .errors
                            .into_iter()
                            .map(|error| format!("{}: {error}", display_path(&file))),
                    );
                    found.cancelled |= result.cancelled;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                    found.cancelled = true;
                    break;
                }
                Err(error) => found
                    .directory
                    .errors
                    .push(format!("{}: {error}", display_path(&file))),
            }
        }
        found.cancelled |= cancel.load(Ordering::Relaxed);
        progress(Update {
            path: Default::default(),
            examined,
            matched: Some(found.directory.entries.len()),
            errors: found.directory.errors.len(),
            phase: if found.cancelled {
                "Suche abgebrochen"
            } else {
                "Suche abgeschlossen"
            }
            .into(),
        });
        Ok(found)
    }
}

/// Match TC's documented feed-to-listbox behavior. No archive member may be treated
/// as a local pathname by copy, delete, rename, or open operations.
pub(super) fn prepare_feed(
    mut review: Review,
    cancel: &AtomicBool,
) -> io::Result<(Review, Option<ArchiveTab>)> {
    let origins = review
        .found
        .directory
        .entries
        .iter()
        .filter_map(|entry| entry.archive_file.as_deref().cloned())
        .collect::<BTreeSet<_>>();
    let only_archive = origins.len() == 1
        && review
            .found
            .directory
            .entries
            .iter()
            .all(|entry| entry.archive_file.is_some());
    if only_archive {
        let file = origins.into_iter().next().unwrap();
        let catalog = Catalog::read_with_password(
            &file,
            review
                .setup
                .search_password
                .as_deref()
                .map(|password| password.as_str()),
            cancel,
        )?;
        let archive = ArchiveTab {
            file,
            folder: PathBuf::new(),
            catalog: Arc::new(catalog),
            password: review.setup.search_password.clone(),
        };
        for entry in &mut review.found.directory.entries {
            entry.archive_file = None;
        }
        Ok((review, Some(archive)))
    } else {
        let mut seen = BTreeSet::new();
        let mut entries = Vec::new();
        for entry in review.found.directory.entries {
            files::check_cancel(cancel)?;
            let path = entry.archive_file.as_deref().unwrap_or(&entry.path);
            if !seen.insert(path.clone()) {
                continue;
            }
            if entry.archive_file.is_some() {
                match Entry::read(
                    path.clone(),
                    path.file_name().unwrap_or_default().to_owned(),
                ) {
                    Ok(container) if container.kind == Kind::File => entries.push(container),
                    Ok(_) => review
                        .found
                        .directory
                        .errors
                        .push(format!("{}: Archiv wurde ersetzt.", display_path(path))),
                    Err(error) => review
                        .found
                        .directory
                        .errors
                        .push(format!("{}: {error}", display_path(path))),
                }
            } else {
                entries.push(entry);
            }
        }
        review.found.directory.entries = entries;
        Ok((review, None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(root: &Path, options: Options) -> SearchTask {
        SearchTask {
            query: options.compile().unwrap(),
            roots: vec![root.to_path_buf()],
            options,
            indexed: None,
            show_hidden: true,
            ignore: None,
            executable: None,
            archive: None,
            archive_scopes: Vec::new(),
            password: None,
            wdx: None,
        }
    }
    fn setup(root: &Path) -> Setup {
        Setup {
            side: 0,
            request: 7,
            directory: root.to_path_buf(),
            selected: Vec::new(),
            archive: None,
            refine: false,
            archive_scopes: Vec::new(),
            search_password: None,
        }
    }
    #[test]
    fn global_archive_search_keeps_origins_refines_members_and_feeds_safe_operational_paths() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("search");
        let source = fixture.path().join("docs");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("Größe.txt"), b"needle").unwrap();
        std::fs::write(source.join("other.txt"), b"other").unwrap();
        std::fs::write(root.join("local.txt"), b"needle").unwrap();
        let cancel = AtomicBool::new(false);
        for name in ["one.zip", "two.zip"] {
            arco_core::archives::create_zip(
                std::slice::from_ref(&source),
                fixture.path(),
                &root.join(name),
                Some("test"),
                &cancel,
            )
            .unwrap();
        }
        let options = Options {
            names: "*.txt".into(),
            content: "needle".into(),
            content_mode: 1,
            archives: true,
            ..Default::default()
        };
        let password: ArchivePassword = Arc::new(zeroize::Zeroizing::new("test".to_owned()));
        let mut search = task(&root, options.clone());
        search.password = Some(password.clone());
        let found = search.run(&cancel).unwrap();
        assert!(
            found.directory.errors.is_empty(),
            "{:?}",
            found.directory.errors
        );
        assert_eq!(found.directory.entries.len(), 3);
        assert_eq!(
            found
                .directory
                .entries
                .iter()
                .filter(|entry| entry.archive_file.is_some())
                .count(),
            2
        );
        let mut setup = setup(&root);
        setup.search_password = Some(password.clone());
        refine(&mut setup, &found.directory.entries);
        assert_eq!(setup.selected, vec![root.join("local.txt")]);
        assert_eq!(setup.archive_scopes.len(), 2);
        assert!(
            setup
                .archive_scopes
                .iter()
                .all(|scope| scope.roots == vec![PathBuf::from("docs/Größe.txt")])
        );
        let mut narrow = task(
            &root,
            Options {
                names: "not-matched".into(),
                depth: "0".into(),
                ..Default::default()
            },
        );
        narrow.query = narrow.query.only_roots();
        narrow.roots = setup.selected.clone();
        narrow.archive_scopes = setup.archive_scopes.clone();
        narrow.password = Some(password);
        assert_eq!(narrow.run(&cancel).unwrap().directory.entries.len(), 3);

        let review = Review {
            setup,
            found,
            duplicate_groups: None,
            criteria: Some(options),
        };
        let (feed, archive) = prepare_feed(review.clone(), &cancel).unwrap();
        assert!(archive.is_none());
        assert_eq!(feed.found.directory.entries.len(), 3);
        assert!(
            feed.found
                .directory
                .entries
                .iter()
                .all(|entry| entry.archive_file.is_none()
                    && entry.path.is_absolute()
                    && entry.path.is_file())
        );
        let mut single = review.clone();
        single.found.directory.entries.retain(|entry| {
            entry
                .archive_file
                .as_ref()
                .is_some_and(|file| file.ends_with("one.zip"))
        });
        let (feed, archive) = prepare_feed(single, &cancel).unwrap();
        assert!(archive.unwrap().file.ends_with("one.zip"));
        assert_eq!(
            feed.found.directory.entries[0].path,
            PathBuf::from("docs/Größe.txt")
        );
        assert!(feed.found.directory.entries[0].archive_file.is_none());

        let mut ignored = task(&root, review.criteria.unwrap());
        ignored.ignore = Some(Arc::new(
            arco_core::ignore::IgnoreList::parse("*.zip").unwrap(),
        ));
        assert_eq!(ignored.run(&cancel).unwrap().directory.entries.len(), 1);
        let aborted = task(
            &root,
            Options {
                archives: true,
                ..Default::default()
            },
        )
        .run(&AtomicBool::new(true))
        .unwrap();
        assert!(aborted.cancelled);
        assert!(aborted.directory.entries.is_empty());
    }
}
