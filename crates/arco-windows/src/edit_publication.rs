use arco_core::files;
use std::io;
use std::marker::PhantomData;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use windows_sys::Win32::Foundation::{WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};

pub struct Publication {
    handle: OwnedHandle,
    // Ownership must be released by the same Windows thread that acquired it.
    _owning_thread: PhantomData<Rc<()>>,
}

impl Publication {
    pub fn acquire(target: &str, cancel: &AtomicBool) -> io::Result<Self> {
        files::check_cancel(cancel)?;
        let digest = blake3::hash(target.as_bytes());
        let name: Vec<u16> = format!("Local\\ArcoCommander.EditPublication.{digest}")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        // A default Windows security descriptor keeps the existing account permissions.
        let raw = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        loop {
            files::check_cancel(cancel)?;
            match unsafe { WaitForSingleObject(handle.as_raw_handle(), 25) } {
                // Every publication revalidates the remote file before modifying it.
                WAIT_OBJECT_0 | WAIT_ABANDONED => {
                    return Ok(Self {
                        handle,
                        _owning_thread: PhantomData,
                    });
                }
                WAIT_TIMEOUT => (),
                _ => return Err(io::Error::last_os_error()),
            }
        }
    }
}

impl Drop for Publication {
    fn drop(&mut self) {
        unsafe { ReleaseMutex(self.handle.as_raw_handle()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    #[test]
    fn publication_waits_across_processes_and_can_be_cancelled() {
        const TARGET: &str = "ARCO_TEST_PUBLICATION_TARGET";
        const BLOCKED: &str = "ARCO_TEST_PUBLICATION_BLOCKED";
        const TEST: &str =
            "edit_publication::tests::publication_waits_across_processes_and_can_be_cancelled";
        let cancel = AtomicBool::new(false);
        if let Ok(target) = std::env::var(TARGET) {
            if std::env::var_os(BLOCKED).is_some() {
                std::thread::scope(|scope| {
                    scope.spawn(|| {
                        std::thread::sleep(Duration::from_millis(100));
                        cancel.store(true, Ordering::Relaxed);
                    });
                    assert!(
                        matches!(Publication::acquire(&target, &cancel), Err(error) if error.kind() == io::ErrorKind::Interrupted)
                    );
                });
            } else {
                let _publication = Publication::acquire(&target, &cancel).unwrap();
            }
            return;
        }
        let target = format!("test publication {}", std::process::id());
        let child = |blocked| {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args(["--exact", TEST])
                .env(TARGET, &target)
                .env_remove(BLOCKED);
            if blocked {
                command.env(BLOCKED, "1");
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
        };
        let publication = Publication::acquire(&target, &cancel).unwrap();
        child(true);
        drop(publication);
        child(false);
    }
}
