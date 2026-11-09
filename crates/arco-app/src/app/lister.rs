use super::{App, StagedPreview, Viewer, strings};
use crate::{Colors, ListerWindow, display_path, preview};
use arco_core::files;
use arco_core::tools::{self, Preview, PreviewEncoding, PreviewMode};
use slint::{ComponentHandle, Timer, TimerMode};
use std::cell::RefCell;
use std::io;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;
use tempfile::TempDir;

pub(super) mod surface;

pub(crate) const VIEWER_ENCODINGS: &[PreviewEncoding] = &[
    PreviewEncoding::Auto,
    PreviewEncoding::Utf8,
    PreviewEncoding::Utf16Le,
    PreviewEncoding::Utf16Be,
    PreviewEncoding::Windows1252,
];

pub(crate) struct ViewerRequest {
    viewer: Viewer,
    offset: u64,
    find: Option<(String, bool)>,
}

impl Viewer {
    pub(crate) fn new(path: PathBuf, mode: PreviewMode) -> Self {
        Self {
            path,
            mode,
            offset: 0,
            next: 0,
            previous: Vec::new(),
            length: 0,
            encoding: PreviewEncoding::Auto,
            last_hit: None,
        }
    }

    pub(crate) fn description(&self, page: &Preview) -> String {
        let hit = self
            .last_hit
            .as_ref()
            .map_or_else(String::new, |(_, offset)| {
                format!(" · Treffer bei Byte {offset}")
            });
        format!("Bytes {}–{} / {}{hit}", page.offset, page.next, page.length)
    }

    pub(crate) fn update_page(&mut self, page: &Preview) {
        self.offset = page.offset;
        self.next = page.next;
        self.length = page.length;
    }

    pub(crate) fn encoding_index(&self) -> i32 {
        VIEWER_ENCODINGS
            .iter()
            .position(|encoding| *encoding == self.encoding)
            .unwrap_or(0) as i32
    }

    pub(crate) fn navigate(mut self, operation: &str) -> Option<ViewerRequest> {
        self.last_hit = None;
        let offset = match operation {
            "viewer-back" => self.previous.pop()?,
            "viewer-next" if self.next < self.length => {
                self.previous.push(self.offset);
                self.next
            }
            "viewer-toggle" => {
                self.previous.clear();
                self.mode = match self.mode {
                    PreviewMode::Text => PreviewMode::Hex,
                    PreviewMode::Hex => PreviewMode::Text,
                };
                self.offset
            }
            _ => return None,
        };
        Some(ViewerRequest {
            viewer: self,
            offset,
            find: None,
        })
    }

    pub(crate) fn control(
        mut self,
        operation: i32,
        value: &str,
        option: i32,
    ) -> Result<Option<ViewerRequest>, &'static str> {
        let mut offset = self.offset;
        let mut find = None;
        match operation {
            0 => {
                let Some(&encoding) = VIEWER_ENCODINGS.get(option.max(0) as usize) else {
                    return Ok(None);
                };
                if self.encoding == encoding {
                    return Ok(None);
                }
                self.encoding = encoding;
                self.previous.clear();
                self.last_hit = None;
            }
            1 => {
                let value = value.trim();
                let position = if let Some(hex) = value
                    .strip_prefix("0x")
                    .or_else(|| value.strip_prefix("0X"))
                {
                    u64::from_str_radix(hex, 16)
                } else {
                    value.parse::<u64>()
                };
                offset = position
                    .ok()
                    .filter(|position| *position <= self.length)
                    .ok_or("Gib einen Byte-Offset innerhalb der Datei ein, dezimal oder mit 0x.")?;
                if offset != self.offset {
                    self.previous.push(self.offset);
                }
                self.last_hit = None;
            }
            2 | 3 => find = Some((value.to_owned(), operation == 3)),
            _ => return Ok(None),
        }
        Ok(Some(ViewerRequest {
            viewer: self,
            offset,
            find,
        }))
    }
}

impl ViewerRequest {
    pub(crate) fn path(&self) -> &std::path::Path {
        &self.viewer.path
    }

    pub(crate) fn label(&self) -> &'static str {
        if self.find.is_some() {
            "Durchsuche Datei …"
        } else {
            "Lese Vorschau …"
        }
    }

    pub(crate) fn load(self, cancel: &AtomicBool) -> io::Result<Option<(Viewer, Preview)>> {
        let Self {
            mut viewer,
            offset,
            find,
        } = self;
        files::check_cancel(cancel)?;
        let metadata = files::regular_metadata(&viewer.path)?;
        let offset = if let Some((query, from_start)) = find {
            let start = if from_start {
                0
            } else {
                viewer
                    .last_hit
                    .as_ref()
                    .filter(|(last, _)| *last == query)
                    .map_or(viewer.offset, |(_, position)| position.saturating_add(1))
            };
            let position = tools::find_preview(
                &viewer.path,
                start,
                viewer.mode,
                viewer.encoding,
                &query,
                cancel,
            )?;
            let Some(position) = position else {
                files::ensure_unchanged(&viewer.path, &metadata)?;
                return Ok(None);
            };
            if position != viewer.offset {
                viewer.previous.push(viewer.offset);
            }
            viewer.last_hit = Some((query, position));
            position
        } else {
            offset
        };
        files::check_cancel(cancel)?;
        let page =
            tools::preview_with_encoding(&viewer.path, offset, viewer.mode, viewer.encoding)?;
        files::ensure_unchanged(&viewer.path, &metadata)?;
        Ok(Some((viewer, page)))
    }
}

enum Loaded {
    Page(Viewer, Preview),
    Image(image::RgbaImage),
    Rich(PathBuf, preview::Rich),
    NoMatch,
}

impl Loaded {
    fn initial(path: PathBuf, content: preview::Content) -> Self {
        match content {
            preview::Content::Page(mode, page) => Self::Page(Viewer::new(path, mode), page),
            preview::Content::Image(image) => Self::Image(image),
            preview::Content::Rich(content) => Self::Rich(path, content),
        }
    }
}

enum Source {
    File(PathBuf),
    Loaded(Loaded),
    Restore(Viewer),
}

struct Worker {
    result: Receiver<io::Result<Loaded>>,
    cancel: Arc<AtomicBool>,
}

struct State {
    storage: Option<Arc<TempDir>>,
    viewer: Option<Viewer>,
    timer: Timer,
    worker: Option<Worker>,
    surface: Option<surface::Surface>,
    raw_path: Option<PathBuf>,
}

impl State {
    fn start(
        &mut self,
        window: &ListerWindow,
        label: &str,
        load: impl FnOnce(&AtomicBool) -> io::Result<Loaded> + Send + 'static,
    ) {
        let (send, result) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let token = cancel.clone();
        let storage = self.storage.clone();
        let thread = std::thread::Builder::new()
            .name("arco-lister".into())
            .spawn(move || {
                let _storage = storage;
                let _ = send.send(load(&token));
            });
        if let Err(error) = thread {
            window.set_status(error.to_string().into());
            return;
        }
        self.worker = Some(Worker { result, cancel });
        window.set_busy(true);
        window.set_status(label.into());
        self.timer.restart();
    }

    fn finish(&mut self, window: &ListerWindow) {
        let Some(worker) = &self.worker else {
            return;
        };
        let result = match worker.result.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err(io::Error::other("Vorschau wurde abgebrochen.")),
        };
        self.worker = None;
        self.timer.stop();
        window.set_busy(false);
        self.present(window, result);
    }

    fn present(&mut self, window: &ListerWindow, result: io::Result<Loaded>) {
        match result {
            Ok(Loaded::Page(mut viewer, page)) => {
                self.surface = None;
                self.raw_path = None;
                window.set_has_rich(false);
                window.set_status(viewer.description(&page).into());
                viewer.update_page(&page);
                window.set_preview_text(page.text.into());
                window.set_has_page(true);
                window.set_has_image(false);
                window.set_can_back(!viewer.previous.is_empty());
                window.set_can_next(viewer.next < viewer.length);
                window.set_hex(viewer.mode == PreviewMode::Hex);
                window.set_encoding(viewer.encoding_index());
                self.viewer = Some(viewer);
            }
            Ok(Loaded::Image(image)) => {
                window.set_preview_image(preview::to_slint(&image));
                window.set_has_image(true);
                window.set_status("Bildvorschau".into());
            }
            Ok(Loaded::Rich(path, content)) => {
                let label = content.label();
                self.raw_path = Some(path.clone());
                window.set_has_rich(true);
                match surface::Surface::new(
                    window.window(),
                    &path,
                    content,
                    window.get_light(),
                    false,
                ) {
                    Ok(surface) => {
                        self.surface = Some(surface);
                        window.set_status(format!("{label} · Formatierte Vorschau").into());
                    }
                    Err(error) => window.set_status(error.to_string().into()),
                }
            }
            Ok(Loaded::NoMatch) => window.set_status("Kein weiterer Treffer.".into()),
            Err(error) => window.set_status(error.to_string().into()),
        }
    }

    fn request(&mut self, window: &ListerWindow, request: ViewerRequest) {
        let label = request.label();
        self.start(window, label, move |cancel| {
            Ok(match request.load(cancel)? {
                Some((viewer, page)) => Loaded::Page(viewer, page),
                None => Loaded::NoMatch,
            })
        });
    }

    fn cancel(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.cancel.store(true, Ordering::Relaxed);
        }
        self.timer.stop();
        self.surface = None;
    }
}

impl App {
    pub(crate) fn open_lister(&mut self, path: PathBuf) {
        self.open_lister_window(display_path(&path), None, Source::File(path));
    }

    pub(super) fn open_rich_lister(&mut self, path: PathBuf, content: preview::Rich) {
        self.open_lister_window(
            display_path(&path),
            None,
            Source::Loaded(Loaded::Rich(path, content)),
        );
    }

    pub(super) fn open_staged_lister(&mut self, file: StagedPreview, content: preview::Content) {
        self.open_lister_window(
            file.title,
            Some(file.storage),
            Source::Loaded(Loaded::initial(file.path, content)),
        );
    }

    pub(crate) fn detach_lister(&mut self) {
        let ui = self.ui();
        let viewer = self.viewer.clone();
        let path = match ui.get_dialog_kind().as_str() {
            "viewer" => viewer.as_ref().map(|viewer| viewer.path.clone()),
            "image" => self
                .staged_preview
                .as_ref()
                .map(|file| &file.path)
                .or(self.preview_image_path.as_ref())
                .cloned(),
            _ => None,
        };
        let Some(path) = path else {
            return;
        };
        let storage = self
            .staged_preview
            .as_ref()
            .filter(|file| file.path == path)
            .map(|file| file.storage.clone());
        let source = viewer.map_or_else(|| Source::File(path), Source::Restore);
        if self.open_lister_window(ui.get_dialog_title().into(), storage, source) {
            self.close_dialog();
        }
    }

    fn open_lister_window(
        &mut self,
        title: String,
        storage: Option<Arc<TempDir>>,
        source: Source,
    ) -> bool {
        let window = match ListerWindow::new() {
            Ok(window) => window,
            Err(error) => {
                self.status(format!(
                    "Listerfenster konnte nicht geöffnet werden. {error}"
                ));
                return false;
            }
        };
        window.set_lister_title(format!("Arco Lister – {title}").into());
        window.set_light(self.ui().global::<Colors>().get_light());
        window.set_encodings(strings([
            "Automatisch",
            "UTF-8",
            "UTF-16 LE",
            "UTF-16 BE",
            "Windows-1252",
        ]));
        let state = Rc::new(RefCell::new(State {
            storage,
            viewer: None,
            timer: Timer::default(),
            worker: None,
            surface: None,
            raw_path: None,
        }));
        let weak_state = Rc::downgrade(&state);
        let weak = window.as_weak();
        state
            .borrow()
            .timer
            .start(TimerMode::Repeated, Duration::from_millis(50), move || {
                if let (Some(window), Some(state)) = (weak.upgrade(), weak_state.upgrade()) {
                    state.borrow_mut().finish(&window);
                }
            });
        state.borrow().timer.stop();
        let weak = window.as_weak();
        let controls = state.clone();
        window.on_navigate(move |operation| {
            let Some(window) = weak.upgrade() else { return };
            let mut state = controls.borrow_mut();
            if state.worker.is_none()
                && let Some(request) = state
                    .viewer
                    .clone()
                    .and_then(|viewer| viewer.navigate(&operation))
            {
                state.request(&window, request);
            }
        });
        let weak = window.as_weak();
        let controls = state.clone();
        window.on_control(move |operation, value, option| {
            let Some(window) = weak.upgrade() else { return };
            let mut state = controls.borrow_mut();
            if state.worker.is_some() {
                return;
            }
            if let Some(viewer) = state.viewer.clone() {
                match viewer.control(operation, &value, option) {
                    Ok(Some(request)) => state.request(&window, request),
                    Ok(None) => (),
                    Err(error) => window.set_status(error.into()),
                }
            }
        });
        let weak = window.as_weak();
        let controls = state.clone();
        window.on_view_raw(move || {
            let Some(window) = weak.upgrade() else { return };
            let Ok(mut state) = controls.try_borrow_mut() else {
                return;
            };
            if state.worker.is_none()
                && let Some(path) = state.raw_path.clone()
            {
                state.start(&window, "Lese Quelldaten …", move |cancel| {
                    files::check_cancel(cancel)?;
                    let content = preview::read_raw(&path)?;
                    Ok(Loaded::initial(path, content))
                });
            }
        });
        #[cfg(windows)]
        {
            use slint::winit_030::{EventResult, WinitWindowAccessor, winit::event::WindowEvent};
            let controls = Rc::downgrade(&state);
            window.window().on_winit_window_event(move |window, event| {
                if matches!(
                    event,
                    WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. }
                ) && let Some(state) = controls.upgrade()
                    && let Ok(state) = state.try_borrow()
                    && let Some(surface) = &state.surface
                {
                    surface.resize(window);
                }
                EventResult::Propagate
            });
        }
        let weak = window.as_weak();
        let fullscreen = window.as_weak();
        window.on_toggle_fullscreen(move || {
            if let Some(window) = fullscreen.upgrade() {
                let window = window.window();
                window.set_fullscreen(!window.is_fullscreen());
            }
        });
        let controls = state.clone();
        window.on_close_lister(move || {
            if let Ok(mut controls) = controls.try_borrow_mut() {
                controls.cancel();
            }
            if let Some(window) = weak.upgrade() {
                let _ = window.hide();
            }
        });
        let controls = state.clone();
        window.window().on_close_requested(move || {
            if let Ok(mut controls) = controls.try_borrow_mut() {
                controls.cancel();
                slint::CloseRequestResponse::HideWindow
            } else {
                slint::CloseRequestResponse::KeepWindowShown
            }
        });
        if let Err(error) = window.show() {
            self.status(format!(
                "Listerfenster konnte nicht geöffnet werden. {error}"
            ));
            return false;
        }
        match source {
            Source::Restore(viewer) => state.borrow_mut().request(
                &window,
                ViewerRequest {
                    offset: viewer.offset,
                    viewer,
                    find: None,
                },
            ),
            Source::Loaded(loaded) => {
                let weak = window.as_weak();
                Timer::single_shot(Duration::ZERO, move || {
                    if let Some(window) = weak.upgrade()
                        && window.window().is_visible()
                    {
                        state.borrow_mut().present(&window, Ok(loaded));
                    }
                });
            }
            Source::File(path) => {
                state
                    .borrow_mut()
                    .start(&window, "Lese Vorschau …", move |cancel| {
                        files::check_cancel(cancel)?;
                        let content = preview::read(&path)?;
                        files::check_cancel(cancel)?;
                        Ok(Loaded::initial(path, content))
                    })
            }
        }
        self.listers.push(window);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewer_requests_keep_independent_history_and_search_position() {
        let fixture = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(fixture.path(), "eins Fund zwei Fund").unwrap();
        let cancel = AtomicBool::new(false);
        let first = Viewer::new(fixture.path().to_owned(), PreviewMode::Text);
        let second = first.clone();
        let (mut first, page) = first
            .control(2, "Fund", 0)
            .unwrap()
            .unwrap()
            .load(&cancel)
            .unwrap()
            .unwrap();
        first.update_page(&page);
        assert_eq!(first.offset, 5);
        let (mut first, page) = first
            .control(2, "Fund", 0)
            .unwrap()
            .unwrap()
            .load(&cancel)
            .unwrap()
            .unwrap();
        first.update_page(&page);
        assert_eq!(first.offset, 15);
        assert_eq!(first.previous, [0, 5]);
        let (second, page) = second
            .control(2, "Fund", 0)
            .unwrap()
            .unwrap()
            .load(&cancel)
            .unwrap()
            .unwrap();
        assert_eq!(page.offset, 5);
        assert_eq!(second.previous, [0]);
        let (_, page) = first
            .navigate("viewer-back")
            .unwrap()
            .load(&cancel)
            .unwrap()
            .unwrap();
        assert_eq!(page.offset, 5);
    }
}
