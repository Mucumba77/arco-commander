//! Read-only system RTF renderer. Windows owns document layout, tables and lists.
use std::io;
use std::marker::PhantomData;
use std::ptr::{null, null_mut};
use std::rc::Rc;
use std::sync::OnceLock;
use windows::Win32::UI::Controls::RichEdit::{
    CFM_FACE, CFM_SIZE, CHARFORMATW, EDITSTREAM, EM_AUTOURLDETECT, EM_EXLIMITTEXT,
    EM_SETCHARFORMAT, EM_STREAMIN, MSFTEDIT_CLASS, SCF_DEFAULT, SF_RTF,
};
use windows_sys::Win32::Foundation::{GetLastError, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::{LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{SetFocus, VK_ESCAPE, VK_TAB};
use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

pub struct RichTextView {
    window: HWND,
    _thread: PhantomData<Rc<()>>,
}

impl RichTextView {
    /// `document` must already have external field instructions and OLE
    /// destinations removed by the caller's RTF filter.
    pub fn new(parent: usize, document: &[u8]) -> io::Result<Self> {
        if parent == 0 || document.len() > 2 * 1024 * 1024 {
            return Err(io::Error::other(
                "Ungültiges RTF-Dokument oder Vorschaufenster.",
            ));
        }
        // The DLL's window class stays registered until process termination.
        // Keep its module loaded for the same lifetime, across all previews.
        static LIBRARY: OnceLock<Result<usize, u32>> = OnceLock::new();
        let library = LIBRARY
            .get_or_init(|| {
                let name = "Msftedit.dll\0".encode_utf16().collect::<Vec<_>>();
                let module = unsafe {
                    LoadLibraryExW(name.as_ptr(), null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32)
                };
                if module.is_null() {
                    Err(unsafe { GetLastError() })
                } else {
                    Ok(module as usize)
                }
            })
            .as_ref()
            .map_err(|error| io::Error::from_raw_os_error(*error as i32))?;
        let title = "RTF-Vorschau\0".encode_utf16().collect::<Vec<_>>();
        let window = unsafe {
            CreateWindowExW(
                0,
                MSFTEDIT_CLASS.0,
                title.as_ptr(),
                WS_CHILD
                    | WS_TABSTOP
                    | WS_VSCROLL
                    | WS_HSCROLL
                    | (ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL) as u32,
                0,
                0,
                1,
                1,
                parent as HWND,
                null_mut(),
                *library as _,
                null(),
            )
        };
        if window.is_null() {
            return Err(io::Error::last_os_error());
        }
        let viewer = Self {
            window,
            _thread: PhantomData,
        };
        if unsafe { SetWindowSubclass(window, Some(preview_keys), 1, parent) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut format = CHARFORMATW {
            cbSize: std::mem::size_of::<CHARFORMATW>() as u32,
            dwMask: CFM_FACE | CFM_SIZE,
            yHeight: 220,
            ..Default::default()
        };
        for (destination, source) in format.szFaceName.iter_mut().zip("Segoe UI".encode_utf16()) {
            *destination = source;
        }
        unsafe {
            SendMessageW(
                window,
                EM_SETCHARFORMAT,
                SCF_DEFAULT as usize,
                (&raw const format) as LPARAM,
            );
            SendMessageW(window, EM_EXLIMITTEXT, 0, 8 * 1024 * 1024);
            SendMessageW(window, EM_AUTOURLDETECT, 0, 0);
        }
        let mut input = Input {
            bytes: document,
            offset: 0,
        };
        let mut stream = EDITSTREAM {
            dwCookie: (&raw mut input) as usize,
            dwError: 0,
            pfnCallback: Some(read_document),
        };
        unsafe {
            SendMessageW(
                window,
                EM_STREAMIN,
                SF_RTF as usize,
                (&raw mut stream) as LPARAM,
            )
        };
        if stream.dwError != 0 {
            return Err(io::Error::from_raw_os_error(stream.dwError as i32));
        }
        Ok(viewer)
    }

    pub fn place(&self, rectangle: Option<[i32; 4]>) -> io::Result<()> {
        if let Some([x, y, width, height]) = rectangle {
            if unsafe {
                SetWindowPos(
                    self.window,
                    null_mut(),
                    x,
                    y,
                    width.max(1),
                    height.max(1),
                    SWP_NOACTIVATE | SWP_NOZORDER,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        unsafe {
            ShowWindow(
                self.window,
                if rectangle.is_some() {
                    SW_SHOWNA
                } else {
                    SW_HIDE
                },
            )
        };
        Ok(())
    }
}

impl Drop for RichTextView {
    fn drop(&mut self) {
        unsafe {
            RemoveWindowSubclass(self.window, Some(preview_keys), 1);
            DestroyWindow(self.window);
        }
    }
}

struct Input<'a> {
    bytes: &'a [u8],
    offset: usize,
}

unsafe extern "system" fn read_document(
    cookie: usize,
    buffer: *mut u8,
    length: i32,
    read: *mut i32,
) -> u32 {
    if cookie == 0 || read.is_null() || length < 0 || (length > 0 && buffer.is_null()) {
        return 1;
    }
    let input = unsafe { &mut *(cookie as *mut Input<'_>) };
    let count = (length as usize).min(input.bytes.len().saturating_sub(input.offset));
    if count > 0 {
        unsafe {
            std::ptr::copy_nonoverlapping(input.bytes.as_ptr().add(input.offset), buffer, count)
        };
    }
    input.offset += count;
    unsafe { *read = count as i32 };
    0
}

unsafe extern "system" fn preview_keys(
    window: HWND,
    message: u32,
    key: WPARAM,
    value: LPARAM,
    _: usize,
    parent: usize,
) -> LRESULT {
    if message == WM_KEYDOWN && (key == VK_TAB as usize || key == VK_ESCAPE as usize) {
        unsafe {
            SetFocus(parent as HWND);
            PostMessageW(parent as HWND, message, key, value);
        }
        return 0;
    }
    unsafe { DefSubclassProc(window, message, key, value) }
}
