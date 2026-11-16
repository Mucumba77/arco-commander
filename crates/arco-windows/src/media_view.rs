//! Optional Media Foundation playback for formats the WebView cannot decode.
use serde::Serialize;
use std::cell::Cell;
use std::ffi::c_void;
use std::io;
use std::marker::PhantomData;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr::{null, null_mut};
use std::rc::Rc;
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
};
use windows::Win32::Foundation::SIZE;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows_core::{HRESULT, Interface, implement};
use windows_sys::Win32::Foundation::{GetLastError, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BLACK_BRUSH, BeginPaint, EndPaint, FillRect, GetStockObject, PAINTSTRUCT,
};
use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows_sys::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{SetFocus, VK_ESCAPE, VK_TAB};
use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

#[derive(Clone, Debug, Serialize)]
pub struct MediaState {
    pub ready: bool,
    pub playing: bool,
    pub position: f64,
    pub duration: f64,
    pub volume: f32,
    pub muted: bool,
    pub video: bool,
    pub error: Option<String>,
}

#[derive(Default)]
struct Events {
    ready: AtomicBool,
    error: Mutex<Option<String>>,
}

#[implement(IMFPMediaPlayerCallback)]
struct Callback {
    events: Arc<Events>,
}

impl IMFPMediaPlayerCallback_Impl for Callback_Impl {
    fn OnMediaPlayerEvent(&self, header: *const MFP_EVENT_HEADER) {
        let Some(header) = (unsafe { header.as_ref() }) else {
            return;
        };
        if header.hrEvent.is_err() || header.eEventType == MFP_EVENT_TYPE_ERROR {
            self.events.ready.store(false, Ordering::Relaxed);
            if let Ok(mut error) = self.events.error.lock() {
                *error = Some(format!(
                    "Windows kann dieses Medienformat nicht wiedergeben. Prüfe den installierten Codec bzw. das Media Feature Pack. {}",
                    windows_core::Error::from_hresult(header.hrEvent)
                ));
            }
        } else if header.eEventType == MFP_EVENT_TYPE_MEDIAITEM_SET {
            self.events.ready.store(true, Ordering::Relaxed);
        }
    }
}

struct Apartment;
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}
struct Child(HWND);
impl Drop for Child {
    fn drop(&mut self) {
        unsafe { DestroyWindow(self.0) };
    }
}
struct Context {
    player: IMFPMediaPlayer,
}

pub struct MediaView {
    window: Child,
    context: Box<Context>,
    events: Arc<Events>,
    rectangle: Cell<Option<[i32; 4]>>,
    placed: Cell<Option<[i32; 4]>>,
    _thread: PhantomData<Rc<()>>,
    _apartment: Apartment,
}

impl MediaView {
    pub fn new(parent: usize, path: &Path) -> io::Result<Self> {
        if parent == 0 || !path.is_absolute() {
            return Err(io::Error::other(
                "Die Medienvorschau benötigt eine lokale Datei mit absolutem Pfad.",
            ));
        }
        let initialized = unsafe { CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32) };
        if initialized < 0 {
            return Err(io::Error::other(windows_core::Error::from_hresult(
                HRESULT(initialized),
            )));
        }
        let apartment = Apartment;
        type Create = unsafe extern "system" fn(
            *const u16,
            i32,
            u32,
            *mut c_void,
            HWND,
            *mut *mut c_void,
        ) -> HRESULT;
        // Keep the optional DLL loaded while any COM vtable can reference it.
        // Dynamic loading also permits Windows N without the Media Feature Pack.
        static LIBRARY: OnceLock<Result<usize, u32>> = OnceLock::new();
        let module = *LIBRARY.get_or_init(|| {
            let name = "mfplay.dll\0".encode_utf16().collect::<Vec<_>>();
            let library = unsafe { LoadLibraryExW(name.as_ptr(), null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32) };
            if library.is_null() { Err(unsafe { GetLastError() }) } else { Ok(library as usize) }
        }).as_ref().map_err(|_| io::Error::other("Windows-Medienkomponenten fehlen. Installiere bei Windows N das Media Feature Pack oder verwende ein WLX-Plugin."))?;
        let address =
            unsafe { GetProcAddress(module as _, c"MFPCreateMediaPlayer".as_ptr().cast()) }
                .ok_or_else(|| io::Error::other("Der Windows-Medienplayer ist nicht verfügbar."))?;
        let create: Create = unsafe { std::mem::transmute(address) };
        let class = "STATIC\0".encode_utf16().collect::<Vec<_>>();
        let child = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                class.as_ptr(),
                WS_CHILD | WS_CLIPSIBLINGS,
                0,
                0,
                1,
                1,
                parent as HWND,
                null_mut(),
                null_mut(),
                null(),
            )
        };
        if child.is_null() {
            return Err(io::Error::last_os_error());
        }
        let window = Child(child);
        let events = Arc::<Events>::default();
        let callback: IMFPMediaPlayerCallback = Callback {
            events: events.clone(),
        }
        .into();
        let filename = path
            .as_os_str()
            .encode_wide()
            .chain([0])
            .collect::<Vec<_>>();
        let mut pointer = null_mut();
        unsafe {
            create(
                filename.as_ptr(),
                0,
                0,
                callback.as_raw(),
                window.0,
                &mut pointer,
            )
        }
        .ok()
        .map_err(io::Error::other)?;
        if pointer.is_null() {
            return Err(io::Error::other("Windows meldet keinen Medienplayer."));
        }
        let player = unsafe { IMFPMediaPlayer::from_raw(pointer) };
        let mut viewer = Self {
            window,
            context: Box::new(Context { player }),
            events,
            rectangle: Cell::new(None),
            placed: Cell::new(None),
            _thread: PhantomData,
            _apartment: apartment,
        };
        if unsafe {
            SetWindowSubclass(
                viewer.window.0,
                Some(video_paint),
                1,
                (&raw mut *viewer.context) as usize,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(viewer)
    }

    pub fn place(&self, rectangle: Option<[i32; 4]>) -> io::Result<()> {
        self.rectangle.set(rectangle);
        let frame = self.state();
        self.position(frame.video)
    }

    fn position(&self, video: bool) -> io::Result<()> {
        let rectangle = if video { self.rectangle.get() } else { None };
        let rectangle = rectangle.map(|[x, y, width, height]| {
            let dpi = unsafe { GetDpiForWindow(self.window.0) }.max(96) as i32;
            [x, y, width, (height - 118 * dpi / 96).max(1)]
        });
        if self.placed.get() != rectangle {
            if let Some([x, y, width, height]) = rectangle {
                if unsafe {
                    SetWindowPos(
                        self.window.0,
                        HWND_TOP,
                        x,
                        y,
                        width.max(1),
                        height.max(1),
                        SWP_NOACTIVATE,
                    )
                } == 0
                {
                    return Err(io::Error::last_os_error());
                }
            }
            unsafe {
                ShowWindow(
                    self.window.0,
                    if rectangle.is_some() {
                        SW_SHOWNA
                    } else {
                        SW_HIDE
                    },
                )
            };
            self.placed.set(rectangle);
        }
        Ok(())
    }

    pub fn state(&self) -> MediaState {
        let player = &self.context.player;
        let ready = self.events.ready.load(Ordering::Relaxed);
        let seconds = |value: windows_core::Result<PROPVARIANT>| {
            value
                .ok()
                .and_then(|value| i64::try_from(&value).ok())
                .map(|value| value.max(0) as f64 / 10_000_000.0)
                .unwrap_or(0.0)
        };
        let mut size = SIZE::default();
        let video = ready
            && unsafe { player.GetNativeVideoSize(Some(&raw mut size), None) }.is_ok()
            && size.cx > 0
            && size.cy > 0;
        let volume = unsafe { player.GetVolume() }.unwrap_or(1.0);
        MediaState {
            ready,
            video,
            playing: unsafe { player.GetState() }
                .is_ok_and(|state| state == MFP_MEDIAPLAYER_STATE_PLAYING),
            position: seconds(unsafe { player.GetPosition(&MFP_POSITIONTYPE_100NS) }),
            duration: seconds(unsafe { player.GetDuration(&MFP_POSITIONTYPE_100NS) }),
            volume: if volume.is_finite() {
                volume.clamp(0.0, 1.0)
            } else {
                1.0
            },
            muted: unsafe { player.GetMute() }.is_ok_and(|muted| muted.as_bool()),
            error: self
                .events
                .error
                .lock()
                .ok()
                .and_then(|error| error.clone()),
        }
    }

    pub fn refresh(&self) -> MediaState {
        let state = self.state();
        let _ = self.position(state.video);
        state
    }

    pub fn control(&self, command: &str, value: f64) -> io::Result<()> {
        if !value.is_finite() {
            return Err(io::Error::other("Ungültiger Medienwert."));
        }
        let player = &self.context.player;
        let result = match command {
            "toggle" => {
                if self.state().playing {
                    unsafe { player.Pause() }
                } else {
                    unsafe { player.Play() }
                }
            }
            "stop" => unsafe { player.Stop() },
            "seek" if (0.0..=1.0).contains(&value) => {
                let ticks = (self.state().duration * value * 10_000_000.0)
                    .round()
                    .clamp(0.0, i64::MAX as f64) as i64;
                unsafe { player.SetPosition(&MFP_POSITIONTYPE_100NS, &PROPVARIANT::from(ticks)) }
            }
            "volume" if (0.0..=1.0).contains(&value) => unsafe { player.SetVolume(value as f32) },
            "mute" => unsafe { player.SetMute(value != 0.0) },
            "leave" | "escape" => {
                let parent = unsafe { GetParent(self.window.0) };
                unsafe {
                    SetFocus(parent);
                    PostMessageW(
                        parent,
                        WM_KEYDOWN,
                        if command == "escape" {
                            VK_ESCAPE as usize
                        } else {
                            VK_TAB as usize
                        },
                        0,
                    );
                }
                Ok(())
            }
            _ => {
                return Err(io::Error::other(
                    "Unbekannte Medienaktion oder ungültiger Wert.",
                ));
            }
        };
        result.map_err(io::Error::other)
    }

    pub fn notice(&self, error: String) {
        if let Ok(mut current) = self.events.error.lock() {
            *current = Some(error);
        }
    }
}

impl Drop for MediaView {
    fn drop(&mut self) {
        unsafe {
            RemoveWindowSubclass(self.window.0, Some(video_paint), 1);
            let _ = self.context.player.Shutdown();
        }
    }
}

unsafe extern "system" fn video_paint(
    window: HWND,
    message: u32,
    key: WPARAM,
    value: LPARAM,
    _: usize,
    context: usize,
) -> LRESULT {
    if message == WM_ERASEBKGND {
        return 1;
    }
    if message == WM_PAINT {
        let mut paint = PAINTSTRUCT::default();
        let dc = unsafe { BeginPaint(window, &raw mut paint) };
        unsafe {
            FillRect(
                dc,
                &raw const paint.rcPaint,
                GetStockObject(BLACK_BRUSH) as _,
            );
        }
        if let Some(context) = unsafe { (context as *const Context).as_ref() } {
            let _ = unsafe { context.player.UpdateVideo() };
        }
        unsafe { EndPaint(window, &raw const paint) };
        return 0;
    }
    unsafe { DefSubclassProc(window, message, key, value) }
}
