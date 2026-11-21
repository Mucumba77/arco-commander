use crate::panel::FtpLocation;
use arco_windows::ftp::{Security, Session};
use std::io;
use std::sync::atomic::AtomicBool;

#[cfg(windows)]
#[derive(Clone)]
pub(super) struct Target(String);

#[cfg(not(windows))]
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, TryLockError, Weak};
#[cfg(not(windows))]
#[derive(Clone)]
pub(super) struct Target(Arc<Mutex<()>>);

impl Target {
    pub(super) fn for_entry(location: &FtpLocation, name: &str) -> io::Result<Self> {
        let host = location.directory.host_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "Die Arbeitskopie hat kein Serverziel.",
            )
        })?;
        let port = location
            .directory
            .port_or_known_default()
            .unwrap_or_else(|| match location.session.security() {
                Security::Sftp | Security::Scp => 22,
                Security::ImplicitTls => 990,
                _ => 21,
            });
        let folder = Session::directory_path(&location.directory);
        let target = format!("{host}\0{port}\0{folder}\0{name}").to_lowercase();
        Self::new(target)
    }
}

#[cfg(windows)]
impl Target {
    fn new(target: String) -> io::Result<Self> {
        Ok(Self(target))
    }

    pub(super) fn acquire(
        &self,
        cancel: &AtomicBool,
    ) -> io::Result<arco_windows::edit_publication::Publication> {
        arco_windows::edit_publication::Publication::acquire(&self.0, cancel)
    }
}

#[cfg(not(windows))]
impl Target {
    fn new(target: String) -> io::Result<Self> {
        type Publications = std::collections::HashMap<String, Weak<Mutex<()>>>;
        static PUBLICATIONS: OnceLock<Mutex<Publications>> = OnceLock::new();
        let mut locks = PUBLICATIONS
            .get_or_init(Mutex::default)
            .lock()
            .map_err(|_| unavailable())?;
        locks.retain(|_, lock| lock.strong_count() != 0);
        if let Some(lock) = locks.get(&target).and_then(Weak::upgrade) {
            return Ok(Self(lock));
        }
        let lock = Arc::new(Mutex::new(()));
        locks.insert(target, Arc::downgrade(&lock));
        Ok(Self(lock))
    }

    pub(super) fn acquire(&self, cancel: &AtomicBool) -> io::Result<MutexGuard<'_, ()>> {
        loop {
            arco_core::files::check_cancel(cancel)?;
            match self.0.try_lock() {
                Ok(guard) => return Ok(guard),
                Err(TryLockError::WouldBlock) => {
                    std::thread::sleep(std::time::Duration::from_millis(25))
                }
                Err(TryLockError::Poisoned(_)) => return Err(unavailable()),
            }
        }
    }
}

#[cfg(not(windows))]
fn unavailable() -> io::Error {
    io::Error::other("Die Rückübertragung für dieses Serverziel ist vorübergehend nicht verfügbar.")
}
