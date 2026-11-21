use super::invalid;
use arco_app::network_sync::{MAX_ENTRIES, copy_names, scan_for_delete};
use arco_core::{files::check_cancel, sync::Scope};
use arco_windows::ftp::{Entry, Session, Url};
use regex::Regex;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::sync::{Arc, atomic::AtomicBool};

pub(super) struct Remove<'a> {
    paths: Vec<&'a str>,
    directories_only: bool,
    permanent: bool,
}

struct Removal {
    parent: Url,
    entry: Entry,
    recycle: bool,
    children: Vec<(Url, Entry)>,
}

impl<'a> Remove<'a> {
    pub(super) fn parse(verb: &str, arguments: &[&'a str]) -> io::Result<Self> {
        let (permanent, paths) = match arguments {
            ["-permanent", paths @ ..] => (true, paths),
            paths => (false, paths),
        };
        let paths = paths.strip_prefix(&["--"]).unwrap_or(paths);
        if paths.is_empty() {
            return Err(invalid(
                "rm und rmdir benötigen mindestens einen Serverpfad.",
            ));
        }
        Ok(Self {
            paths: paths.to_vec(),
            directories_only: verb == "rmdir",
            permanent,
        })
    }

    pub(super) fn run(
        self,
        session: &Arc<Session>,
        directory: &Url,
        fail_on_no_match: bool,
        cancel: &AtomicBool,
    ) -> io::Result<Value> {
        let (selected, exact) = self.select(session, directory, fail_on_no_match, cancel)?;
        let mut folders = BTreeSet::new();
        for (parent, entry) in &selected {
            if session.protects_recycle_directory(parent, entry, cancel)? {
                return Err(invalid(
                    "Öffne den Server-Papierkorb, um seine Inhalte zu entfernen.",
                ));
            }
            if !self.directories_only && entry.directory == Some(true) {
                folders.insert(
                    session
                        .child_directory(parent, &entry.name)?
                        .path()
                        .to_owned(),
                );
            }
        }

        let mut plan = Vec::new();
        let mut count = 0;
        for (parent, entry) in selected {
            check_cancel(cancel)?;
            if parent
                .path()
                .match_indices('/')
                .any(|(index, _)| folders.contains(&parent.path()[..=index]))
            {
                continue;
            }
            let recycle = !self.permanent && session.will_recycle(&parent);
            let mut children = Vec::new();
            if !recycle && entry.directory == Some(true) {
                let folder = session.child_directory(&parent, &entry.name)?;
                if self.directories_only {
                    if !session.list_sync(&folder, cancel)?.entries.is_empty() {
                        return Err(io::Error::new(
                            io::ErrorKind::DirectoryNotEmpty,
                            "Der Serverordner ist nicht leer.",
                        ));
                    }
                } else {
                    // Limit the traversal to the exact server path, even on case-insensitive clients.
                    let endpoint = (Arc::clone(session), folder);
                    let scope = Scope::new(false, &[])?;
                    let subtree = scan_for_delete(&endpoint, &scope, cancel)?;
                    if count + subtree.len() >= MAX_ENTRIES {
                        return Err(invalid("Die Löschliste enthält zu viele Einträge."));
                    }
                    for (path, child) in subtree.into_iter().rev() {
                        children.push((copy_names::remote_parent(&endpoint, &path, true)?, child));
                    }
                }
            }
            count += 1 + children.len();
            if count > MAX_ENTRIES {
                return Err(invalid("Die Löschliste enthält zu viele Einträge."));
            }
            plan.push(Removal {
                parent,
                entry,
                recycle,
                children,
            });
        }

        let mut completed = Vec::new();
        for action in plan {
            let record = action.apply(session, cancel).map_err(|error| {
                if exact {
                    return error;
                }
                io::Error::new(
                    error.kind(),
                    format!(
                        "{} nach {} abgeschlossenen Zielen: {error}",
                        action.entry.name,
                        completed.len()
                    ),
                )
            })?;
            completed.push(record);
        }
        if exact && completed.len() == 1 {
            return Ok(completed.pop().expect("one result"));
        }
        Ok(json!({"entries": completed, "completed": completed.len()}))
    }

    fn select(
        &self,
        session: &Session,
        directory: &Url,
        fail_on_no_match: bool,
        cancel: &AtomicBool,
    ) -> io::Result<(Vec<(Url, Entry)>, bool)> {
        let mut listings = BTreeMap::new();
        let mut selected = BTreeMap::new();
        let mut exact = self.paths.len() == 1;
        for requested in &self.paths {
            check_cancel(cancel)?;
            let path = requested
                .strip_prefix("./")
                .unwrap_or(requested)
                .trim_end_matches('/');
            if path.contains("://") {
                return Err(invalid("Verwende einen Serverpfad für rm oder rmdir."));
            }
            let (parent, name) = session.download_location(directory, path)?;
            let mask = (!self.directories_only && name.contains(['*', '?']))
                .then(|| wildcard(&name))
                .transpose()?;
            exact &= mask.is_none();
            let listing = match listings.entry(parent.clone()) {
                std::collections::btree_map::Entry::Occupied(slot) => slot.into_mut(),
                std::collections::btree_map::Entry::Vacant(slot) => {
                    slot.insert(session.list_sync(&parent, cancel)?)
                }
            };
            let mut found = false;
            for entry in &listing.entries {
                if !mask
                    .as_ref()
                    .map_or(entry.name == name, |mask| mask.is_match(&entry.name))
                {
                    continue;
                }
                if if self.directories_only {
                    entry.directory != Some(true)
                } else {
                    entry.directory.is_none() && !entry.symlink
                } {
                    return Err(invalid(
                        "Der Eintrag hat einen anderen oder unbekannten Dateityp.",
                    ));
                }
                found = true;
                selected.insert((parent.clone(), entry.name.clone()), entry.clone());
                if selected.len() > MAX_ENTRIES {
                    return Err(invalid("Die Löschliste enthält zu viele Einträge."));
                }
            }
            if !found && (mask.is_none() || fail_on_no_match) {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("Keine Treffer für „{requested}“."),
                ));
            }
        }
        Ok((
            selected
                .into_iter()
                .map(|((parent, _), entry)| (parent, entry))
                .collect(),
            exact,
        ))
    }
}

impl Removal {
    fn apply(&self, session: &Session, cancel: &AtomicBool) -> io::Result<Value> {
        check_cancel(cancel)?;
        if self.recycle {
            let slot = session.recycle_entry(&self.parent, &self.entry, cancel)?;
            return Ok(json!({"recycled": self.entry.name, "slot": slot}));
        }
        for (parent, child) in &self.children {
            session.delete_sync_entry(parent, child, cancel)?;
        }
        session.delete_sync_entry(&self.parent, &self.entry, cancel)?;
        Ok(json!({"removed": self.entry.name}))
    }
}

fn wildcard(name: &str) -> io::Result<Regex> {
    let mut expression = String::from(r"\A");
    for part in name.split_inclusive(['*', '?']) {
        if let Some(literal) = part.strip_suffix('*') {
            expression.push_str(&regex::escape(literal));
            expression.push_str(".*");
        } else if let Some(literal) = part.strip_suffix('?') {
            expression.push_str(&regex::escape(literal));
            expression.push('.');
        } else {
            expression.push_str(&regex::escape(part));
        }
    }
    expression.push_str(r"\z");
    Regex::new(&expression).map_err(io::Error::other)
}
