use super::{App, Event};
use crate::commands::{Dialog, Pending};
use crate::panel::FtpLocation;
use arco_core::ignore::IgnoreList;
use arco_core::sync::LocalRoot;
#[cfg(test)]
use arco_windows::ftp;
use std::io;
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

pub(super) struct Running {
    cancel: Arc<AtomicBool>,
    session: Arc<arco_windows::ftp::Session>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl App {
    pub(crate) fn open_network_watch(&mut self) {
        if self.network_watch.is_some() {
            self.status("Eine Ordnerüberwachung läuft bereits.");
            return;
        }
        let left = self.panels[0].current();
        let right = self.panels[1].current();
        let (local, remote) = if left.is_local() {
            (left, right)
        } else {
            (right, left)
        };
        let Some(remote) = remote.ftp.as_ref().filter(|_| local.is_local()) else {
            self.status("Öffne einen lokalen Ordner neben einem Netzwerk-Dateifenster.");
            return;
        };
        let description = format!(
            "{} → {}\n{}",
            crate::display_path(&local.path),
            remote.location.directory,
            crate::locale::tr(
                "Neue und geänderte Dateien werden veröffentlicht. Die Löschoption gilt nur für bestätigte Dateipaare."
            )
        );
        self.dialog(
            "input",
            Pending::NetworkWatch(local.path.clone(), remote.location.clone()),
            Dialog {
                title: "Lokalen Ordner überwachen",
                description: &description,
                value: "5",
                placeholder: "Prüfintervall in Sekunden",
                options_label: "Lokale Löschungen",
                options: &[
                    crate::locale::tr("Serverdateien behalten"),
                    crate::locale::tr("Löschungen auf dem Server übernehmen"),
                ],
                confirm_label: "Überwachung starten",
                ..Default::default()
            },
        );
    }

    pub(crate) fn start_network_watch(
        &mut self,
        local: PathBuf,
        mut remote: FtpLocation,
        interval: &str,
        delete: bool,
    ) -> io::Result<()> {
        if self.network_watch.is_some() {
            return Err(io::Error::other("Eine Ordnerüberwachung läuft bereits."));
        }
        let interval = arco_app::network_watch::parse_interval(interval)?;
        let root = LocalRoot::open(&local)?;
        let ignore = self.ignore_list();
        let sender = self.sender.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let session = remote.session.clone();
        remote.session = self.job_transfer_options()?.resolve(
            &self.profile.network_transfer_presets,
            &session,
            &remote.directory,
            Some(root.path()),
        )?;
        std::thread::Builder::new()
            .name("arco-network-watch".into())
            .spawn(move || {
                let result = watch(
                    root,
                    remote,
                    ignore.as_deref(),
                    interval,
                    delete,
                    &worker_cancel,
                    |uploaded, deleted| {
                        let _ = sender.send(Event::NetworkPublished(uploaded, deleted));
                    },
                );
                let result = match result {
                    Err(error) if error.kind() != io::ErrorKind::Interrupted => {
                        Err(error.to_string())
                    }
                    _ => Ok(()),
                };
                let _ = sender.send(Event::NetworkWatchEnded(result));
            })?;
        self.network_watch = Some(Running { cancel, session });
        self.status("Ordnerüberwachung wird vorbereitet …");
        Ok(())
    }

    pub(crate) fn stop_network_watch(&self) {
        if let Some(running) = &self.network_watch {
            running.cancel.store(true, Ordering::Relaxed);
            self.status("Ordnerüberwachung wird beendet …");
        } else {
            self.status("Es läuft keine Ordnerüberwachung.");
        }
    }

    pub(super) fn stop_watch_for(&self, session: &Arc<arco_windows::ftp::Session>) {
        if let Some(running) = &self.network_watch
            && Arc::ptr_eq(&running.session, session)
        {
            running.cancel.store(true, Ordering::Relaxed);
        }
    }
}

fn watch(
    root: LocalRoot,
    remote: FtpLocation,
    ignore: Option<&IgnoreList>,
    interval: Duration,
    delete: bool,
    cancel: &AtomicBool,
    mut published: impl FnMut(usize, usize),
) -> io::Result<()> {
    arco_app::network_watch::watch(
        root,
        (remote.session, remote.directory),
        arco_app::network_watch::WatchOptions {
            profile_name: remote.profile.as_deref(),
            ignore,
            interval,
            duration: None,
            delete,
            initial_upload: false,
            masks: "",
        },
        cancel,
        |uploaded, deleted| {
            published(uploaded, deleted);
            Ok(())
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires the writable loopback fixture from scripts/test-sftp-reference.py"]
    fn reference_network_watch() {
        #[cfg(test)]
        use arco_windows::ftp;
        let connection = ftp::Connection::new(
            &std::env::var("ARCO_SFTP_TEST_URL").unwrap(),
            "arco",
            ftp::Security::Sftp,
        )
        .unwrap()
        .with_ssh(&std::env::var("ARCO_SFTP_TEST_FINGERPRINT").unwrap(), None)
        .unwrap();
        let session = Arc::new(connection.authenticate("fixture-only").unwrap());
        let cancel = AtomicBool::new(false);
        let home = session.initial_directory();
        session
            .create_sync_directory(&home, "watch test", &cancel)
            .unwrap();
        let directory = session
            .relative_directory(&home, Path::new("watch test"))
            .unwrap();
        let local = tempfile::tempdir().unwrap();
        std::fs::write(local.path().join("unchanged.txt"), b"initial baseline").unwrap();
        let root = LocalRoot::open(local.path()).unwrap();
        let location = FtpLocation {
            session: session.clone(),
            directory: directory.clone(),
            profile: None,
        };
        let result = std::thread::scope(|scope| {
            let (sender, receiver) = std::sync::mpsc::channel();
            let watch_cancel = &cancel;
            let worker = scope.spawn(move || {
                watch(
                    root,
                    location,
                    None,
                    Duration::from_millis(150),
                    false,
                    watch_cancel,
                    |count, _| {
                        let _ = sender.send(count);
                    },
                )
            });
            let verify = || -> io::Result<Vec<u8>> {
                if receiver
                    .recv_timeout(Duration::from_secs(8))
                    .map_err(io::Error::other)?
                    != 0
                    || !session
                        .list(&directory, &AtomicBool::new(false))?
                        .entries
                        .is_empty()
                {
                    return Err(io::Error::other("Unexpected initial upload"));
                }
                let nested = local.path().join("nested");
                std::fs::create_dir(&nested)?;
                std::fs::write(nested.join("Büro.txt"), b"changed after start")?;
                if receiver
                    .recv_timeout(Duration::from_secs(8))
                    .map_err(io::Error::other)?
                    != 2
                {
                    return Err(io::Error::other(
                        "Expected a directory and a file to be published",
                    ));
                }
                std::fs::remove_file(nested.join("Büro.txt"))?;
                std::thread::sleep(Duration::from_millis(450));
                let folder = session.relative_directory(&directory, Path::new("nested"))?;
                let target = local.path().join("verified");
                session.download(&folder, "Büro.txt", &target, &AtomicBool::new(false))?;
                std::fs::read(target)
            };
            let result = verify();
            cancel.store(true, Ordering::Relaxed);
            let stopped = worker.join().unwrap();
            (result, stopped)
        });
        assert_eq!(result.0.unwrap(), b"changed after start");
        assert_eq!(result.1.unwrap_err().kind(), io::ErrorKind::Interrupted);
    }

    #[test]
    #[ignore = "requires the writable loopback fixture from scripts/test-sftp-reference.py"]
    fn reference_network_watch_deletes_only_confirmed_files() {
        use std::fs;
        let session = Arc::new(
            ftp::Connection::new(
                &std::env::var("ARCO_SFTP_TEST_URL").unwrap(),
                "arco",
                ftp::Security::Sftp,
            )
            .unwrap()
            .with_ssh(&std::env::var("ARCO_SFTP_TEST_FINGERPRINT").unwrap(), None)
            .unwrap()
            .authenticate("fixture-only")
            .unwrap(),
        );
        let local = tempfile::tempdir().unwrap();
        let root = LocalRoot::open(local.path()).unwrap();
        let remote =
            PathBuf::from(std::env::var_os("ARCO_SFTP_TEST_ROOT").unwrap()).join("watch deletions");
        fs::create_dir(&remote).unwrap();
        let directory = session
            .relative_directory(&session.initial_directory(), Path::new("watch deletions"))
            .unwrap();
        let cancel = AtomicBool::new(false);
        for name in ["baseline.txt", "changed.txt"] {
            fs::write(local.path().join(name), b"original").unwrap();
            fs::write(remote.join(name), b"original").unwrap();
        }
        fs::write(remote.join("foreign.txt"), b"remote only").unwrap();
        let changed = remote.join("changed.txt");
        let stamp = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        fs::File::options()
            .write(true)
            .open(&changed)
            .unwrap()
            .set_modified(stamp)
            .unwrap();
        let location = FtpLocation {
            session,
            directory,
            profile: None,
        };
        let (observations, stopped) = std::thread::scope(|scope| {
            let (notice, notices) = std::sync::mpsc::channel();
            let (done, finished) = std::sync::mpsc::channel();
            let cancel_ref = &cancel;
            let worker = scope.spawn(move || {
                let result = watch(
                    root,
                    location,
                    None,
                    Duration::from_millis(150),
                    true,
                    cancel_ref,
                    |uploaded, deleted| {
                        let _ = notice.send((uploaded, deleted));
                    },
                );
                let _ = done.send(());
                result
            });
            let verify = || -> io::Result<()> {
                let next = || {
                    notices
                        .recv_timeout(Duration::from_secs(8))
                        .map_err(io::Error::other)
                };
                if next()? != (0, 0) {
                    return Err(io::Error::other("Unexpected initial transfer"));
                }
                fs::write(local.path().join("new.txt"), b"published after start")?;
                if next()? != (1, 0) {
                    return Err(io::Error::other("Expected the newly published file"));
                }
                fs::remove_file(local.path().join("baseline.txt"))?;
                fs::remove_file(local.path().join("new.txt"))?;
                let first = next()?.1;
                let sum = if first == 1 { first + next()?.1 } else { first };
                if sum != 2
                    || remote.join("baseline.txt").exists()
                    || remote.join("new.txt").exists()
                {
                    return Err(io::Error::other(
                        "Expected both confirmed files to be removed",
                    ));
                }
                fs::write(&changed, b"external")?;
                fs::File::options()
                    .write(true)
                    .open(&changed)?
                    .set_modified(stamp)?;
                fs::remove_file(local.path().join("changed.txt"))?;
                finished
                    .recv_timeout(Duration::from_secs(8))
                    .map_err(io::Error::other)?;
                Ok(())
            };
            let observations = verify();
            cancel.store(true, Ordering::Relaxed);
            (observations, worker.join().unwrap())
        });
        observations.unwrap();
        assert!(stopped.unwrap_err().to_string().contains("wurde geändert"));
        assert_eq!(fs::read(changed).unwrap(), b"external");
        assert_eq!(
            fs::read(remote.join("foreign.txt")).unwrap(),
            b"remote only"
        );
    }
}
