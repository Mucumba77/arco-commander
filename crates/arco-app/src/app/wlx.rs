use super::{App, QuickTarget};
use crate::preview;
use arco_core::plugin::{HostProcess, Interface, Registration};
use slint::ComponentHandle;
use slint::winit_030::WinitWindowAccessor;
use std::io;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

pub(super) enum QuickResult {
    Internal(io::Result<preview::Content>, Option<String>),
    Embedded(Box<EmbeddedPreview>),
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
            self.ui().set_quick_embedded(false);
        }
    }

    pub(super) fn quick_plugin(&self, target: &QuickTarget) -> Option<(Registration, usize)> {
        if self.quick_skip_plugin || target.archive.is_some() {
            return None;
        }
        let entry =
            self.profile.plugins.iter().find(|entry| {
                !entry.masks.is_empty() && entry.matches(Interface::Wlx, &target.path)
            })?;
        let parent = self
            .ui()
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
            .flatten()?;
        Some((entry.clone(), parent))
    }

    pub(super) fn accept_quick_result(&mut self, result: QuickResult) {
        match result {
            QuickResult::Internal(result, error) => {
                if let Some(error) = error {
                    self.status(error);
                }
                self.show_quick_result(result);
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

    pub(super) fn position_quick_plugin(&mut self) {
        if self.quick_embedded.is_none() {
            return;
        }
        let ui = self.ui();
        let bounds = ui.get_quick_bounds();
        let rect = if self.context_menu_open
            || ui.get_settings_open()
            || ui.get_drive_side() >= 0
            || !ui.get_dialog_kind().is_empty()
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
        let preview = self.quick_embedded.as_mut().expect("embedded preview");
        if !matches!(preview.host.try_wait(), Ok(None)) || !preview.place(rect) {
            self.fallback_quick_plugin();
        }
    }

    fn fallback_quick_plugin(&mut self) {
        self.stop_quick_plugin();
        self.quick_skip_plugin = true;
        self.quick_ready = false;
        self.ui().set_quick_note("".into());
    }
}
