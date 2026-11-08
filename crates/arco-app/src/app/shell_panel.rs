use super::{App, LoadedDirectory, Navigation, Outcome};
use crate::panel::{Location, ShellLocation};
use arco_core::files::{self, Directory, Entry, Kind};
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

pub(super) fn read_panel(
    location: &mut ShellLocation,
    hidden: bool,
    cancel: &AtomicBool,
) -> io::Result<LoadedDirectory> {
    let listing = arco_windows::shell_listing(location.address(), hidden, cancel)?;
    let mut directory = Directory {
        entries: Vec::with_capacity(listing.entries.len()),
        errors: Vec::new(),
    };
    let mut shell = BTreeMap::new();
    for item in listing.entries {
        files::check_cancel(cancel)?;
        let key = PathBuf::from(&item.parsing_name);
        if shell.contains_key(&key) {
            continue;
        }
        let kind = if item.directory {
            Kind::Directory
        } else {
            Kind::File
        };
        let metadata = item
            .path
            .as_deref()
            .and_then(|path| files::regular_metadata(path).ok());
        let size = metadata
            .as_ref()
            .filter(|metadata| metadata.is_file())
            .map(|metadata| metadata.len());
        let modified = metadata.and_then(|metadata| metadata.modified().ok());
        directory.entries.push(
            Entry::from_remote(key.clone(), kind, size, modified)
                .with_display_name(item.name.as_str().into()),
        );
        shell.insert(key, item);
    }
    *location = ShellLocation(listing.ancestors);
    Ok(LoadedDirectory {
        directory,
        space: None,
        archive: None,
        ftp: None,
        shell,
    })
}

impl App {
    pub(crate) fn open_shell_panel(&mut self, side: usize, address: &str) {
        if cfg!(windows) {
            self.load_location(
                side,
                Location::Shell(ShellLocation::from_address(address)),
                Navigation::Jump,
            );
        } else {
            self.status("Windows-Shell-Ordner sind unter Windows verfügbar.");
        }
    }

    pub(super) fn open_shell_panel_row(&mut self, row: usize) {
        let tab = self.current();
        let Some(item) = tab
            .display_row(row)
            .and_then(|entry| tab.shell_entries.get(&entry.path))
            .cloned()
        else {
            return;
        };
        if let Some(path) = item.path.filter(|path| path.is_absolute()) {
            self.open_path(&path);
        } else if item.directory {
            self.open_shell_panel(self.active, &item.parsing_name);
        } else {
            self.start_job("Öffne Shell-Eintrag …", false, move |_| {
                arco_windows::open_shell_item(&item.parsing_name)?;
                Ok(Outcome::Notice("Shell-Eintrag geöffnet.".into()))
            });
        }
    }

    pub(crate) fn current_shell_file(&self) -> Option<&Path> {
        let tab = self.current();
        let entry = tab.display_row(tab.cursor)?;
        let item = tab.shell_entries.get(&entry.path)?;
        (!item.directory)
            .then(|| item.path.as_deref())
            .flatten()
            .filter(|path| path.is_absolute())
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn shell_listing_exposes_computer_and_its_filesystem_drives() {
        let cancel = AtomicBool::new(false);
        let mut desktop = ShellLocation::default();
        let root = read_panel(&mut desktop, false, &cancel).unwrap();
        let computer = root.shell.values().find(|item| item.computer).unwrap();
        assert!(computer.directory);
        let mut location = ShellLocation::from_address(&computer.parsing_name);
        let drives = read_panel(&mut location, false, &cancel).unwrap();
        assert_eq!(
            Location::Shell(location).parent(),
            Some(Location::Shell(desktop))
        );
        assert!(drives.shell.values().any(|item| {
            item.directory && item.path.as_ref().is_some_and(|path| path.is_absolute())
        }));
    }

    #[test]
    fn shell_tabs_keep_the_local_fallback_and_shell_lock_on_restore() {
        use crate::panel::{Panel, Tab};
        use arco_core::profile::TabLock;
        let local = tempfile::tempdir().unwrap();
        let mut panel = Panel::from_saved(Default::default(), local.path());
        let shell = Location::Shell(ShellLocation::from_address(arco_windows::SHELL_COMPUTER));
        let mut tab = Tab::new(local.path().to_owned());
        tab.change_location(&shell, None);
        tab.set_lock(TabLock::ReturnToBase);
        panel.tabs = vec![tab];
        let restored = Panel::from_saved(panel.saved(), local.path());
        assert_eq!(restored.current().path, local.path());
        assert_eq!(restored.current().location(), shell);
        assert_eq!(restored.current().lock_base, Some(shell));
    }
}
