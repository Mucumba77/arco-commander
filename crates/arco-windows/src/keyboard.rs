use std::cell::Cell;
use std::io;
use std::num::NonZeroIsize;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, VK_APPS, VK_CONTROL, VK_F10, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    WM_KEYDOWN, WM_KEYUP, WM_NCDESTROY, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

const SUBCLASS_ID: usize = 2;

#[derive(Clone, Copy)]
pub struct NativeShortcut {
    pub key: &'static str,
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
    pub repeat: bool,
}

pub struct ShortcutHook {
    window: HWND,
    state: Box<HookState>,
}

struct HookState {
    callback: Box<dyn Fn(NativeShortcut) -> bool>,
    captured: Cell<u8>,
    attached: Cell<bool>,
}

impl ShortcutHook {
    /// Install on the thread that owns the supplied Win32 window.
    pub fn install(
        window: NonZeroIsize,
        callback: impl Fn(NativeShortcut) -> bool + 'static,
    ) -> io::Result<Self> {
        let window = window.get() as HWND;
        let state = Box::new(HookState {
            callback: Box::new(callback),
            captured: Cell::new(0),
            attached: Cell::new(true),
        });
        let reference = (&raw const *state) as usize;
        // The registration is removed before this allocation is freed.
        if unsafe { SetWindowSubclass(window, Some(key_message), SUBCLASS_ID, reference) } == 0 {
            return Err(io::Error::other(
                "Windows-Tastaturfilter konnte nicht installiert werden.",
            ));
        }
        Ok(Self { window, state })
    }
}

impl Drop for ShortcutHook {
    fn drop(&mut self) {
        if self.state.attached.replace(false) {
            unsafe { RemoveWindowSubclass(self.window, Some(key_message), SUBCLASS_ID) };
        }
    }
}

unsafe extern "system" fn key_message(
    window: HWND,
    message: u32,
    wparam: usize,
    lparam: isize,
    _: usize,
    reference: usize,
) -> isize {
    // The subclass stores the address of its live UI-thread guard.
    let state = unsafe { &*(reference as *const HookState) };
    if message == WM_NCDESTROY {
        state.attached.set(false);
        unsafe { RemoveWindowSubclass(window, Some(key_message), SUBCLASS_ID) };
    }
    let key = match wparam as u16 {
        VK_F10 => Some(("F10", 1)),
        VK_APPS => Some(("Menu", 2)),
        _ => None,
    };
    if let Some((key, bit)) = key {
        let captured = state.captured.get();
        if matches!(message, WM_KEYUP | WM_SYSKEYUP) && captured & bit != 0 {
            state.captured.set(captured & !bit);
            return 0;
        }
        if matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN) {
            let repeat = lparam & (1 << 30) != 0;
            if repeat && captured & bit != 0 {
                return 0;
            }
            let down = |key| unsafe { GetKeyState(i32::from(key)) } < 0;
            if !down(VK_LWIN)
                && !down(VK_RWIN)
                && (state.callback)(NativeShortcut {
                    key,
                    control: down(VK_CONTROL),
                    alt: down(VK_MENU),
                    shift: down(VK_SHIFT),
                    repeat,
                })
            {
                state.captured.set(state.captured.get() | bit);
                return 0;
            }
        }
    }
    unsafe { DefSubclassProc(window, message, wparam, lparam) }
}
