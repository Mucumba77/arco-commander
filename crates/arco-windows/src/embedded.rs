use std::ptr::null_mut;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetParent, GetWindowThreadProcessId, PostMessageW, SW_HIDE, SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE,
    SWP_NOZORDER, SWP_SHOWWINDOW, SetWindowPos, ShowWindowAsync, WM_CLOSE,
};

fn child_window(parent: usize, window: usize, process: u32) -> Option<HWND> {
    let window = window as HWND;
    let mut owner = 0;
    // Check both the owning process and the parent before acting on an HWND from IPC.
    unsafe {
        if GetWindowThreadProcessId(window, &mut owner) == 0
            || owner != process
            || GetParent(window) != parent as HWND
        {
            return None;
        }
    }
    Some(window)
}

pub fn position_embedded(
    parent: usize,
    window: usize,
    process: u32,
    rect: Option<[i32; 4]>,
) -> bool {
    let Some(window) = child_window(parent, window, process) else {
        return false;
    };
    unsafe {
        if let Some([x, y, width, height]) = rect {
            return SetWindowPos(
                window,
                null_mut(),
                x,
                y,
                width,
                height,
                SWP_ASYNCWINDOWPOS | SWP_NOACTIVATE | SWP_NOZORDER | SWP_SHOWWINDOW,
            ) != 0;
        }
        ShowWindowAsync(window, SW_HIDE);
    }
    true
}

pub fn close_embedded(parent: usize, window: usize, process: u32) {
    if let Some(window) = child_window(parent, window, process) {
        unsafe {
            ShowWindowAsync(window, SW_HIDE);
            PostMessageW(window, WM_CLOSE, 0, 0);
        }
    }
}
