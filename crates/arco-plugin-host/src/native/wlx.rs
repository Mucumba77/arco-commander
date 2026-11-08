use super::{Library, ansi, required, symbol, wide};
use std::cell::Cell;
use std::ffi::OsStr;
use std::io;
use std::path::Path;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{COLOR_WINDOW, GetSysColorBrush};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{GetWindowDpiAwarenessContext, SetThreadDpiAwarenessContext};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{SetFocus, VK_ESCAPE};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

type Load<T> = unsafe extern "system" fn(HWND, *mut T, i32) -> HWND;
type Close = unsafe extern "system" fn(HWND);
type SetDefaults = unsafe extern "system" fn(*mut Defaults);

const FORCE_SHOW: i32 = 16;

#[repr(C)]
struct Defaults {
    size: i32,
    version_low: u32,
    version_high: u32,
    ini: [u8; 260],
}

#[derive(Clone, Copy)]
struct Preview {
    child: HWND,
    close: Option<Close>,
}

thread_local! {
    static PREVIEW: Cell<Option<Preview>> = const { Cell::new(None) };
}

pub fn view(
    library: &Library,
    file: &Path,
    embedded: Option<usize>,
    ready: impl FnOnce(Option<usize>) -> io::Result<()>,
) -> io::Result<()> {
    if !file.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "Datei für die WLX-Vorschau fehlt",
        ));
    }
    if let Some(set) = symbol::<SetDefaults>(library, b"ListSetDefaultParams") {
        let path = std::env::current_dir()?.join("lsplugin.ini");
        let name = ansi(path.as_os_str())?;
        let mut defaults = Defaults {
            size: std::mem::size_of::<Defaults>() as i32,
            version_low: 0,
            version_high: 2,
            ini: [0; 260],
        };
        let buffer = defaults
            .ini
            .get_mut(..name.len())
            .ok_or_else(|| io::Error::other("Pfad für WLX-Einstellungen ist zu lang"))?;
        buffer.copy_from_slice(&name);
        unsafe { set(&mut defaults) };
    }
    let parent = Window::new(file, embedded)?;
    let child = if let Some(load) = symbol::<Load<u16>>(library, b"ListLoadW") {
        let mut filename = wide(file.as_os_str());
        unsafe { load(parent.0, filename.as_mut_ptr(), FORCE_SHOW) }
    } else {
        let load: Load<u8> = required(library, b"ListLoad")?;
        let mut filename = ansi(file.as_os_str())?;
        unsafe { load(parent.0, filename.as_mut_ptr(), FORCE_SHOW) }
    };
    if child.is_null() || unsafe { IsChild(parent.0, child) } == 0 {
        return Err(io::Error::other(
            "WLX-Plugin konnte kein Vorschaufenster öffnen",
        ));
    }
    PREVIEW.set(Some(Preview {
        child,
        close: symbol(library, b"ListCloseWindow"),
    }));
    resize(parent.0, child);
    unsafe {
        ShowWindow(child, SW_SHOW);
        if embedded.is_none() {
            ShowWindow(parent.0, SW_SHOW);
            SetFocus(child);
        }
    }
    ready(embedded.map(|_| parent.0 as usize))?;
    let mut message = MSG::default();
    loop {
        match unsafe { GetMessageW(&mut message, null_mut(), 0, 0) } {
            -1 => return Err(io::Error::last_os_error()),
            0 => return Ok(()),
            _ if embedded.is_some()
                && message.message == WM_KEYDOWN
                && message.wParam == usize::from(VK_ESCAPE) =>
            {
                return Ok(());
            }
            _ => unsafe {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            },
        }
    }
}

struct Window(HWND);

impl Window {
    fn new(file: &Path, embedded: Option<usize>) -> io::Result<Self> {
        let class = wide(OsStr::new("ArcoWlxPreview"));
        let filename = file
            .file_name()
            .unwrap_or(file.as_os_str())
            .to_string_lossy();
        let title = wide(OsStr::new(&format!("Arco Vorschau · {filename}")));
        let instance = unsafe { GetModuleHandleW(null()) };
        let window_class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            hCursor: unsafe { LoadCursorW(null_mut(), IDC_ARROW) },
            hbrBackground: unsafe { GetSysColorBrush(COLOR_WINDOW) },
            lpszClassName: class.as_ptr(),
            ..Default::default()
        };
        if unsafe { RegisterClassW(&window_class) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let (style, x, y, width, height, parent) = if let Some(parent) = embedded {
            let parent = parent as HWND;
            if unsafe { IsWindow(parent) } == 0 {
                return Err(io::Error::other(
                    "Das Arco-Vorschaufenster existiert nicht mehr.",
                ));
            }
            let dpi = unsafe { GetWindowDpiAwarenessContext(parent) };
            if dpi.is_null() || unsafe { SetThreadDpiAwarenessContext(dpi) }.is_null() {
                return Err(io::Error::other(
                    "DPI-Kontext des Vorschaufensters konnte nicht übernommen werden.",
                ));
            }
            (
                WS_CHILD | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
                0,
                0,
                32,
                32,
                parent,
            )
        } else {
            (
                WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                900,
                620,
                null_mut(),
            )
        };
        let window = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                title.as_ptr(),
                style,
                x,
                y,
                width,
                height,
                parent,
                null_mut(),
                instance,
                null(),
            )
        };
        if window.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(window))
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        if unsafe { IsWindow(self.0) } != 0 {
            unsafe { SendMessageW(self.0, WM_CLOSE, 0, 0) };
        }
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_SIZE => {
            if let Some(preview) = PREVIEW.get() {
                resize(window, preview.child);
            }
        }
        WM_SETFOCUS => {
            if let Some(preview) = PREVIEW.get() {
                unsafe { SetFocus(preview.child) };
            }
        }
        WM_CLOSE => unsafe {
            if let Some(preview) = PREVIEW.take() {
                if let Some(close) = preview.close {
                    close(preview.child);
                } else {
                    DestroyWindow(preview.child);
                }
            }
            DestroyWindow(window);
        },
        WM_DESTROY => unsafe { PostQuitMessage(0) },
        _ => return unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
    0
}

fn resize(window: HWND, child: HWND) {
    let mut rect = RECT::default();
    if unsafe { GetClientRect(window, &mut rect) } != 0 {
        unsafe { MoveWindow(child, 0, 0, rect.right, rect.bottom, 1) };
    }
}
