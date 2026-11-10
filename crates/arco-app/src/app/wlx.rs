use super::lister::surface::Surface;
use super::{App, QuickTarget};
use crate::{MainWindow, preview};
use arco_core::plugin::{HostProcess, Interface, Registration};
use slint::winit_030::WinitWindowAccessor;
use slint::{ComponentHandle, Timer};
use std::cell::RefCell;
use std::io;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub(super) enum QuickResult {
    Internal(io::Result<QuickPreview>, Option<String>),
    Embedded(Box<EmbeddedPreview>),
}

pub(super) struct QuickPreview {
    path: PathBuf,
    storage: Option<tempfile::TempDir>,
    content: preview::Content,
    fallback: Option<preview::Content>,
}

pub(super) fn read_quick_preview(
    target: &QuickTarget,
    cancel: &AtomicBool,
) -> io::Result<QuickPreview> {
    let (storage, path) = match &target.archive {
        Some((_, _, true)) => {
            return Err(io::Error::other(
                "Verschlüsselter Eintrag. Verwende F3 für die Passwortabfrage.",
            ));
        }
        Some((archive, _, false)) => {
            let (storage, path) =
                super::archive_actions::stage_archive_preview(archive, &target.path, cancel)?;
            (Some(storage), path)
        }
        None => (None, target.path.clone()),
    };
    arco_core::files::check_cancel(cancel)?;
    let content = preview::read_quick(&path)?;
    let fallback = if matches!(content, preview::Content::Rich(_)) {
        preview::read_raw(&path).ok()
    } else {
        None
    };
    arco_core::files::check_cancel(cancel)?;
    Ok(QuickPreview {
        path,
        storage,
        content,
        fallback,
    })
}

#[derive(Default)]
pub(super) struct QuickWeb {
    generation: u64,
    building: bool,
    pending: Option<RichRequest>,
    child: Option<RichChild>,
}

struct RichRequest {
    path: PathBuf,
    storage: Option<tempfile::TempDir>,
    rich: preview::Rich,
    fallback: Option<preview::Content>,
    light: bool,
}

struct RichChild {
    surface: Surface,
    _storage: Option<tempfile::TempDir>,
    fallback: Option<preview::Content>,
    bounds: Option<[i32; 4]>,
}

pub(super) struct EmbeddedPreview {
    name: String,
    parent: usize,
    window: usize,
    host: HostProcess,
    bounds: Option<[i32; 4]>,
}

impl EmbeddedPreview {
    pub(super) fn new(name: String, parent: usize, window: usize, host: HostProcess) -> Self {
        Self {
            name,
            parent,
            window,
            host,
            bounds: None,
        }
    }

    fn place(&mut self, bounds: Option<[i32; 4]>) -> bool {
        #[cfg(windows)]
        {
            if self.bounds == bounds {
                return true;
            }
            if arco_windows::position_embedded(self.parent, self.window, self.host.id(), bounds) {
                self.bounds = bounds;
                return true;
            }
        }
        false
    }

    fn close(mut self: Box<Self>) {
        #[cfg(windows)]
        arco_windows::close_embedded(self.parent, self.window, self.host.id());
        std::thread::spawn(move || {
            let started = Instant::now();
            while started.elapsed() < Duration::from_millis(400)
                && matches!(self.host.try_wait(), Ok(None))
            {
                std::thread::sleep(Duration::from_millis(20));
            }
        });
    }
}

impl App {
    pub(super) fn stop_quick_plugin(&mut self) {
        if let Some(cancel) = &self.quick_cancel {
            cancel.store(true, Ordering::Relaxed);
        }
        if let Some(preview) = self.quick_embedded.take() {
            preview.close();
        }
        let mut web = self.quick_web.borrow_mut();
        web.generation = web.generation.wrapping_add(1);
        web.pending = None;
        web.child = None;
        self.ui().set_quick_embedded(false);
    }

    pub(super) fn quick_plugin(&self, target: &QuickTarget) -> Option<(Registration, usize)> {
        if self.quick_skip_plugin || target.archive.is_some() {
            return None;
        }
        let entry =
            self.profile.plugins.iter().find(|entry| {
                !entry.masks.is_empty() && entry.matches(Interface::Wlx, &target.path)
            })?;
        Some((entry.clone(), self.native_window_handle()?))
    }

    pub(super) fn native_window_handle(&self) -> Option<usize> {
        self.ui()
            .window()
            .with_winit_window(|window| {
                use slint::winit_030::winit::raw_window_handle::{
                    HasWindowHandle, RawWindowHandle,
                };
                let RawWindowHandle::Win32(handle) = window.window_handle().ok()?.as_raw() else {
                    return None;
                };
                Some(handle.hwnd.get() as usize)
            })
            .flatten()
    }

    pub(super) fn accept_quick_result(&mut self, result: QuickResult) {
        match result {
            QuickResult::Internal(result, error) => {
                if let Some(error) = error {
                    self.status(error);
                }
                match result {
                    Ok(QuickPreview {
                        path,
                        storage,
                        content: preview::Content::Rich(rich),
                        fallback,
                    }) => self.open_quick_web(path, storage, rich, fallback),
                    Ok(preview) => Self::show_quick_result(&self.ui(), Ok(preview.content)),
                    Err(error) => Self::show_quick_result(&self.ui(), Err(error)),
                }
            }
            QuickResult::Embedded(preview) => {
                #[cfg(windows)]
                if !arco_windows::position_embedded(
                    preview.parent,
                    preview.window,
                    preview.host.id(),
                    None,
                ) {
                    preview.close();
                    self.fallback_quick_plugin();
                    return;
                }
                let ui = self.ui();
                ui.set_quick_note(
                    format!("WLX · {} · F3 für die Vollansicht", preview.name).into(),
                );
                ui.set_quick_text("".into());
                ui.set_quick_embedded(true);
                self.quick_embedded = Some(preview);
            }
        }
    }

    fn open_quick_web(
        &self,
        path: PathBuf,
        storage: Option<tempfile::TempDir>,
        rich: preview::Rich,
        fallback: Option<preview::Content>,
    ) {
        let mut state = self.quick_web.borrow_mut();
        state.pending = Some(RichRequest {
            path,
            storage,
            rich,
            fallback,
            light: self.effective_light_theme(),
        });
        if !state.building {
            schedule_quick_web(self.ui().as_weak(), &self.quick_web);
        }
    }

    pub(super) fn position_quick_plugin(&mut self) {
        if self.quick_embedded.is_none() && self.quick_web.borrow().child.is_none() {
            return;
        }
        let ui = self.ui();
        let bounds = ui.get_quick_bounds();
        let rect = if self.context_menu_open
            || ui.get_settings_open()
            || ui.get_drive_side() >= 0
            || !ui.get_dialog_kind().is_empty()
            || ui.get_job_activity_open()
            || bounds.width < 1.0
            || bounds.height < 1.0
        {
            None
        } else {
            let scale = ui.window().scale_factor();
            Some(
                [bounds.x, bounds.y, bounds.width, bounds.height]
                    .map(|value| (value * scale).round() as i32),
            )
        };
        if let Some(preview) = &mut self.quick_embedded
            && (!matches!(preview.host.try_wait(), Ok(None)) || !preview.place(rect))
        {
            self.fallback_quick_plugin();
        }
        let mut web = self.quick_web.borrow_mut();
        if let Some(child) = &mut web.child
            && child.bounds != rect
        {
            if child.surface.place(rect).is_ok() {
                child.bounds = rect;
            } else {
                let fallback = child.fallback.take();
                web.child = None;
                ui.set_quick_embedded(false);
                let note = "Die formatierte Vorschau konnte nicht eingebettet werden.";
                if let Some(fallback) = fallback {
                    Self::show_quick_result(&ui, Ok(fallback));
                    ui.set_quick_note(note.into());
                } else {
                    Self::show_quick_result(&ui, Err(io::Error::other(note)));
                }
            }
        }
    }

    fn fallback_quick_plugin(&mut self) {
        self.stop_quick_plugin();
        self.quick_skip_plugin = true;
        self.quick_ready = false;
        self.ui().set_quick_note("".into());
    }
}

fn schedule_quick_web(window: slint::Weak<MainWindow>, state: &Rc<RefCell<QuickWeb>>) {
    let state = Rc::downgrade(state);
    // WebView2 pumps native messages while creating its controller.
    Timer::single_shot(Duration::ZERO, move || {
        let (Some(ui), Some(state)) = (window.upgrade(), state.upgrade()) else {
            return;
        };
        let (generation, request) = {
            let mut web = state.borrow_mut();
            if web.building || !ui.get_quick_open() {
                return;
            }
            let Some(request) = web.pending.take() else {
                return;
            };
            web.building = true;
            (web.generation, request)
        };
        let RichRequest {
            path,
            storage,
            rich,
            fallback,
            light,
        } = request;
        let label = rich.label();
        let result = Surface::new(ui.window(), &path, rich, light);
        let mut web = state.borrow_mut();
        web.building = false;
        if web.generation == generation && ui.get_quick_open() {
            match result {
                Ok(surface) => {
                    web.child = Some(RichChild {
                        surface,
                        _storage: storage,
                        fallback,
                        bounds: None,
                    });
                    ui.set_quick_note(format!("{label} · F3 für die Vollansicht").into());
                    ui.set_quick_text("".into());
                    ui.set_quick_embedded(true);
                }
                Err(error) => {
                    if let Some(fallback) = fallback {
                        App::show_quick_result(&ui, Ok(fallback));
                        ui.set_quick_note(
                            format!("{label} · Formatierte Vorschau nicht verfügbar").into(),
                        );
                    } else {
                        App::show_quick_result(&ui, Err(error));
                    }
                }
            }
        }
        if web.pending.is_some() {
            schedule_quick_web(window, &state);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quick_view_keeps_formatted_archive_entries_until_the_preview_is_dropped() {
        let fixture = tempfile::tempdir().unwrap();
        let member = PathBuf::from("welcome.html");
        let local = fixture.path().join(&member);
        let zip = fixture.path().join("example.zip");
        let cancel = AtomicBool::new(false);
        std::fs::write(&local, "<h1>Arco</h1>").unwrap();
        arco_core::archives::create_zip(&[local.clone()], fixture.path(), &zip, None, &cancel)
            .unwrap();
        let mut target = QuickTarget {
            path: local,
            modified: None,
            size: None,
            archive: None,
        };
        let preview = read_quick_preview(&target, &cancel).unwrap();
        assert!(matches!(preview.content, preview::Content::Rich(_)));
        assert!(preview.fallback.is_some());
        assert!(preview.storage.is_none());
        target.path = member;
        target.archive = Some((zip, 0, false));
        let preview = read_quick_preview(&target, &cancel).unwrap();
        assert!(matches!(preview.content, preview::Content::Rich(_)));
        let staged = preview.path.clone();
        assert!(staged.is_file());
        drop(preview);
        assert!(!staged.exists());
        cancel.store(true, Ordering::Relaxed);
        assert!(read_quick_preview(&target, &cancel).is_err());
    }
}
