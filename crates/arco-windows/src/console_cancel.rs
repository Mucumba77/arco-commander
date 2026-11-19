use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler};

static ACTIVE: AtomicBool = AtomicBool::new(false);
static CANCELLED: AtomicBool = AtomicBool::new(false);

unsafe extern "system" fn interrupt(event: u32) -> i32 {
    if matches!(event, CTRL_C_EVENT | CTRL_BREAK_EVENT) {
        CANCELLED.store(true, Ordering::Relaxed);
        1
    } else {
        0
    }
}

pub fn run<T>(work: impl FnOnce(&AtomicBool) -> io::Result<T>) -> io::Result<T> {
    if ACTIVE.swap(true, Ordering::Acquire) {
        return Err(io::Error::other(
            "Die Konsolenüberwachung ist bereits registriert.",
        ));
    }
    struct Registration(bool);
    impl Drop for Registration {
        fn drop(&mut self) {
            if self.0 {
                unsafe { SetConsoleCtrlHandler(Some(interrupt), 0) };
            }
            ACTIVE.store(false, Ordering::Release);
        }
    }
    let mut registration = Registration(false);
    CANCELLED.store(false, Ordering::Relaxed);
    if unsafe { SetConsoleCtrlHandler(Some(interrupt), 1) } == 0 {
        return Err(io::Error::last_os_error());
    }
    registration.0 = true;
    work(&CANCELLED)
}
