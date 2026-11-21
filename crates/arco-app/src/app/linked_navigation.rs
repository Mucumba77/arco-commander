use super::{App, JobDescription, Navigation, Outcome};
use crate::commands::{Dialog, Pending};
use crate::panel::Location;
use arco_core::{files, profile::TabLock, sync::LocalRoot};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

pub(super) struct Pair {
    roots: [Location; 2],
    tabs: [u64; 2],
    requests: [Option<u64>; 2],
}

pub struct LinkedFolder {
    side: usize,
    requests: [u64; 2],
    tabs: [u64; 2],
    root: Location,
    relative: PathBuf,
    target: Location,
}

impl App {
    fn navigation_tabs(&self) -> [u64; 2] {
        self.panels
            .each_ref()
            .map(|panel| panel.current().navigation_id)
    }

    pub(super) fn navigation_linked(&self) -> bool {
        self.linked_navigation.as_ref().is_some_and(|pair| {
            pair.tabs == self.navigation_tabs()
                && pair
                    .roots
                    .iter()
                    .zip(&self.panels)
                    .all(|(root, panel)| same_scope(root, &panel.current().location()))
        })
    }

    pub(super) fn accepts_linked_load(&self, side: usize, id: u64) -> bool {
        self.navigation_linked()
            && self
                .linked_navigation
                .as_ref()
                .is_some_and(|pair| pair.requests[side] == Some(id))
    }

    pub(super) fn clear_link_on_scope_change(&mut self) {
        if self
            .linked_navigation
            .as_ref()
            .is_some_and(|pair| pair.tabs == self.navigation_tabs())
            && !self.navigation_linked()
        {
            self.linked_navigation = None;
        }
    }

    pub(crate) fn toggle_linked_navigation(&mut self) {
        self.pending_navigation_link = None;
        if self.navigation_linked() {
            self.linked_navigation = None;
            self.ui().set_navigation_linked(false);
            self.status("Navigation entkoppelt.");
            return;
        }
        let roots = self
            .panels
            .each_ref()
            .map(|panel| panel.current().location());
        if !roots
            .iter()
            .all(|root| matches!(root, Location::Local(_) | Location::Ftp(_)))
            || self
                .panels
                .iter()
                .any(|panel| panel.loading || !panel.current().loaded)
        {
            self.status("Öffne zwei lokale oder Netzwerkordner und warte, bis beide geladen sind.");
            return;
        }
        if self
            .panels
            .iter()
            .any(|panel| panel.current().options.lock == TabLock::Fixed)
        {
            self.status("Ein fest gesperrter Tab kann der Navigation nicht folgen.");
            return;
        }
        self.pair_navigation(roots);
        self.status("Navigation gekoppelt.");
    }

    fn pair_navigation(&mut self, roots: [Location; 2]) {
        self.linked_navigation = Some(Pair {
            roots,
            tabs: self.navigation_tabs(),
            requests: [None, None],
        });
        self.ui().set_navigation_linked(true);
    }

    pub(super) fn set_pending_navigation_link(&mut self, linked: bool) {
        self.linked_navigation = None;
        self.pending_navigation_link = linked.then(|| self.navigation_tabs());
        self.ui().set_navigation_linked(false);
    }

    pub(super) fn workspace_link_enabled(&self) -> bool {
        self.navigation_linked() || self.pending_navigation_link == Some(self.navigation_tabs())
    }

    pub(super) fn restore_pending_navigation_link(&mut self) {
        let Some(tabs) = self.pending_navigation_link else {
            return;
        };
        if tabs != self.navigation_tabs() {
            self.pending_navigation_link = None;
            return;
        }
        if self.panels.iter().any(|panel| {
            panel.loading || !panel.current().loaded || panel.current().deferred.is_some()
        }) {
            return;
        }
        self.pending_navigation_link = None;
        let roots = self
            .panels
            .each_ref()
            .map(|panel| panel.current().location());
        if roots
            .iter()
            .all(|root| matches!(root, Location::Local(_) | Location::Ftp(_)))
            && self
                .panels
                .iter()
                .all(|panel| panel.current().options.lock != TabLock::Fixed)
        {
            self.pair_navigation(roots);
        }
    }

    pub(super) fn follow_linked_navigation(&mut self, side: usize, target: &Location) {
        if !self.navigation_linked() {
            return;
        }
        let pair = self.linked_navigation.as_ref().expect("linked tabs");
        let other = 1 - side;
        let panel = &self.panels[other];
        if panel.current().options.lock == TabLock::Fixed
            || (panel.loading && pair.requests[other] != Some(panel.request))
        {
            return;
        }
        let Some(path) = relative(&pair.roots[side], target) else {
            self.status("Der Pfad liegt außerhalb der gekoppelten Startordner.");
            return;
        };
        let Ok(next) = child(&pair.roots[other], &path) else {
            self.status("Der Gegenordner kann auf diesem Dateisystem nicht geöffnet werden.");
            return;
        };
        let same = panel.current().location() == next;
        if !same || panel.loading {
            self.load_location(other, next, Navigation::Linked);
            if let Some(pair) = self.linked_navigation.as_mut() {
                pair.requests[other] = Some(self.panels[other].request);
            }
        }
    }

    pub(super) fn offer_linked_folder(
        &mut self,
        side: usize,
        id: u64,
        target: &Location,
        error: &io::Error,
    ) -> bool {
        if error.kind() != io::ErrorKind::NotFound
            || !self.accepts_linked_load(side, id)
            || self.pending.is_some()
            || self.job.is_some()
        {
            return false;
        }
        let pair = self.linked_navigation.as_ref().expect("linked tabs");
        let Some(path) = relative(&pair.roots[side], target) else {
            return false;
        };
        let request = LinkedFolder {
            side,
            requests: self.panels.each_ref().map(|panel| panel.request),
            tabs: pair.tabs,
            root: pair.roots[side].clone(),
            relative: path,
            target: target.clone(),
        };
        self.dialog(
            "confirm",
            Pending::CreateLinkedFolder(Box::new(request)),
            Dialog {
                title: "Gekoppelten Ordner anlegen",
                description: &target.display(),
                confirm_label: "Ordner anlegen",
                ..Dialog::default()
            },
        );
        true
    }

    fn current_link(&self, request: &LinkedFolder) -> bool {
        self.navigation_linked()
            && request.tabs == self.navigation_tabs()
            && request.requests == self.panels.each_ref().map(|panel| panel.request)
    }

    pub(crate) fn create_linked_folder(&mut self, request: LinkedFolder) -> io::Result<()> {
        if !self.current_link(&request) {
            return Err(io::Error::other(
                "Die gekoppelten Dateifenster wurden inzwischen gewechselt.",
            ));
        }
        let title = "Gekoppelten Ordner anlegen";
        let description = JobDescription::new(
            crate::OperationKind::Task,
            String::new(),
            request.target.display(),
        );
        self.start_operation_job(title, false, description, move |job| {
            create_missing(&request.root, &request.relative, &job.cancel)?;
            Ok(Outcome::LinkedFolder(Box::new(request)))
        });
        Ok(())
    }

    pub(super) fn open_created_linked_folder(&mut self, request: LinkedFolder) {
        if self.current_link(&request) {
            self.load_location(request.side, request.target, Navigation::Linked);
            if let Some(pair) = self.linked_navigation.as_mut() {
                pair.requests[request.side] = Some(self.panels[request.side].request);
            }
        }
    }
}

fn same_scope(root: &Location, target: &Location) -> bool {
    match (root, target) {
        (Location::Local(_), Location::Local(_)) => true,
        (Location::Ftp(a), Location::Ftp(b)) => std::sync::Arc::ptr_eq(&a.session, &b.session),
        _ => false,
    }
}

fn relative(root: &Location, target: &Location) -> Option<PathBuf> {
    if !same_scope(root, target) {
        return None;
    }
    let mut cursor = target.clone();
    let mut parts = Vec::new();
    loop {
        if &cursor == root {
            return Some(parts.iter().rev().collect());
        }
        let key = cursor.row_key();
        let name = match &cursor {
            Location::Local(_) => key.file_name()?,
            Location::Ftp(_) => key.as_os_str(),
            _ => return None,
        };
        parts.push(files::leaf_name(name).ok()?.to_owned());
        cursor = cursor.parent()?;
    }
}

fn child(root: &Location, relative: &Path) -> io::Result<Location> {
    match root {
        Location::Local(path) => Ok(Location::Local(path.join(relative))),
        Location::Ftp(ftp) => Ok(Location::Ftp(
            ftp.at(ftp.session.relative_directory(&ftp.directory, relative)?),
        )),
        _ => Err(io::Error::other(
            "Dieser Ort unterstützt keine gekoppelte Navigation.",
        )),
    }
}

fn create_missing(root: &Location, relative: &Path, cancel: &AtomicBool) -> io::Result<()> {
    match root {
        Location::Local(path) => {
            let root = LocalRoot::open(path)?;
            let mut prefix = PathBuf::new();
            for name in relative {
                files::check_cancel(cancel)?;
                prefix.push(files::leaf_name(name)?);
                let target = root.checked_path(&prefix)?;
                match std::fs::symlink_metadata(&target) {
                    Ok(meta) if meta.is_dir() && !files::is_link(&meta) => (),
                    Ok(_) => {
                        return Err(io::Error::new(
                            io::ErrorKind::AlreadyExists,
                            "Am Ziel liegt ein anderer Eintrag.",
                        ));
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        std::fs::create_dir(target)?
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        Location::Ftp(ftp) => {
            let mut parent = ftp.directory.clone();
            for name in relative {
                files::check_cancel(cancel)?;
                let name = files::leaf_name(name)?
                    .to_str()
                    .ok_or_else(|| io::Error::other("Serverordner benötigen Unicode."))?;
                match ftp
                    .session
                    .sync_entry_at(&parent, Path::new(name), cancel)?
                {
                    Some(entry) if entry.name == name && entry.directory == Some(true) => (),
                    Some(_) => {
                        return Err(io::Error::new(
                            io::ErrorKind::AlreadyExists,
                            "Der Serverordnername ist bereits belegt.",
                        ));
                    }
                    None => ftp.session.create_sync_directory(&parent, name, cancel)?,
                }
                parent = ftp.session.relative_directory(&parent, Path::new(name))?;
            }
        }
        _ => {
            return Err(io::Error::other(
                "Dieser Ort unterstützt keine gekoppelte Navigation.",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchored_paths_follow_each_side_without_leaving_the_selected_roots() {
        let a = Location::Local(PathBuf::from("alpha"));
        let b = Location::Local(PathBuf::from("beta"));
        let folder = Path::new("Ein Ordner").join("Änderungen");
        let source = child(&a, &folder).unwrap();
        let duplicate = child(&b, &relative(&a, &source).unwrap()).unwrap();
        assert_eq!(relative(&b, &duplicate), Some(folder));
        assert_eq!(relative(&a, &a), Some(PathBuf::new()));
        assert_eq!(
            relative(&a, &Location::Local(Path::new("alphabet").join("x"))),
            None
        );
    }

    #[test]
    fn creates_missing_counterparts_without_replacing_a_file() {
        let directory = tempfile::tempdir().unwrap();
        let root = Location::Local(directory.path().to_owned());
        let cancel = AtomicBool::new(false);
        let relative = Path::new("Ein Ordner").join("Änderungen");
        create_missing(&root, &relative, &cancel).unwrap();
        assert!(directory.path().join(&relative).is_dir());
        std::fs::write(directory.path().join("belegt"), b"bestehend").unwrap();
        assert!(create_missing(&root, &Path::new("belegt").join("neu"), &cancel).is_err());
        assert_eq!(
            std::fs::read(directory.path().join("belegt")).unwrap(),
            b"bestehend"
        );
        assert!(create_missing(&root, Path::new("../ungültig"), &cancel).is_err());
    }
}
