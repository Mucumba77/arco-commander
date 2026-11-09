use super::{Library, ansi, required, symbol, wide};
use arco_core::files;
use arco_core::plugin::{Row, WLX_BITMAP_BATCH, WLX_BITMAP_HEIGHT, WLX_BITMAP_WIDTH, WlxBitmap};
use std::cell::Cell;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, COLOR_WINDOW, CreateCompatibleDC, DIB_RGB_COLORS,
    DeleteDC, DeleteObject, GetDIBits, GetObjectW, GetSysColorBrush, HBITMAP, HDC,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{GetWindowDpiAwarenessContext, SetThreadDpiAwarenessContext};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{SetFocus, VK_ESCAPE};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

type Load<T> = unsafe extern "system" fn(HWND, *mut T, i32) -> HWND;
type Close = unsafe extern "system" fn(HWND);
type SetDefaults = unsafe extern "system" fn(*mut Defaults);
type GetBitmap<T> = unsafe extern "system" fn(*mut T, i32, i32, *mut u8, i32) -> HBITMAP;

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
    set_defaults(library)?;
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

fn set_defaults(library: &Library) -> io::Result<()> {
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
    Ok(())
}

pub fn thumbnails(library: &Library, files: &[PathBuf]) -> io::Result<Vec<Row>> {
    if !(1..=WLX_BITMAP_BATCH).contains(&files.len())
        || files.iter().any(|file| !file.is_absolute())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Ungültige WLX-Miniaturanfrage.",
        ));
    }
    let wide_export = symbol::<GetBitmap<u16>>(library, b"ListGetPreviewBitmapW");
    let ansi_export = symbol::<GetBitmap<u8>>(library, b"ListGetPreviewBitmap");
    if wide_export.is_none() && ansi_export.is_none() {
        return Ok(Vec::new());
    }
    set_defaults(library)?;
    let dc = DeviceContext(unsafe { CreateCompatibleDC(null_mut()) });
    if dc.0.is_null() {
        return Err(io::Error::last_os_error());
    }
    let mut rows = Vec::new();
    for (index, file) in files.iter().enumerate() {
        if super::cancelled() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let image = (|| {
            let before = files::regular_metadata(file)
                .ok()
                .filter(|meta| meta.is_file())?;
            let mut content = Vec::new();
            File::open(file)
                .ok()?
                .take(8192)
                .read_to_end(&mut content)
                .ok()?;
            files::ensure_unchanged(file, &before).ok()?;
            let (w, h) = (WLX_BITMAP_WIDTH as i32, WLX_BITMAP_HEIGHT as i32);
            let len = content.len() as i32;
            let bitmap = if let Some(preview) = wide_export {
                let mut name = wide(file.as_os_str());
                unsafe { preview(name.as_mut_ptr(), w, h, content.as_mut_ptr(), len) }
            } else {
                let mut name = ansi(file.as_os_str()).ok()?;
                let preview = ansi_export?;
                unsafe { preview(name.as_mut_ptr(), w, h, content.as_mut_ptr(), len) }
            };
            let bitmap = Bitmap(bitmap);
            files::ensure_unchanged(file, &before).ok()?;
            bitmap.rgba(&dc)
        })();
        if let Some(image) = image {
            rows.push(image.into_row(index));
        }
    }
    Ok(rows)
}

struct DeviceContext(HDC);
impl Drop for DeviceContext {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { DeleteDC(self.0) };
        }
    }
}

struct Bitmap(HBITMAP);
impl Drop for Bitmap {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { DeleteObject(self.0) };
        }
    }
}

impl Bitmap {
    fn rgba(&self, dc: &DeviceContext) -> Option<WlxBitmap> {
        if self.0.is_null() {
            return None;
        }
        let mut bitmap = BITMAP::default();
        if unsafe {
            GetObjectW(
                self.0,
                std::mem::size_of::<BITMAP>() as i32,
                (&raw mut bitmap).cast(),
            )
        } == 0
        {
            return None;
        }
        let width = u32::try_from(bitmap.bmWidth).ok()?;
        let height = u32::try_from(bitmap.bmHeight).ok()?;
        let mut rgba = vec![0; WlxBitmap::rgba_size(width, height)?];
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..Default::default()
            },
            ..Default::default()
        };
        let lines = unsafe {
            GetDIBits(
                dc.0,
                self.0,
                0,
                height,
                rgba.as_mut_ptr().cast(),
                &mut info,
                DIB_RGB_COLORS,
            )
        };
        if lines != height as i32 {
            return None;
        }
        for pixel in rgba.chunks_exact_mut(4) {
            pixel.swap(0, 2);
            pixel[3] = 255;
        }
        Some(WlxBitmap {
            width,
            height,
            rgba,
        })
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
