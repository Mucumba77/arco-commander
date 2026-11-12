use super::App;
use crate::{MainWindow, ShortcutLabels, ShortcutRow, shortcuts};
use arco_core::profile::QuickFindStart;
use slint::winit_030::winit::event::{ElementState, WindowEvent};
use slint::winit_030::winit::keyboard::{Key, ModifiersState};
use slint::{ModelRc, VecModel};
use std::cell::RefCell;
use std::fmt::Write as _;
use std::rc::{Rc, Weak};

pub(super) struct Editor {
    overrides: shortcuts::Overrides,
    command: Option<&'static str>,
    filter: String,
    candidate: Option<String>,
    recording: bool,
    notice: String,
}

pub(super) fn connect_editor(window: &MainWindow, app: &Rc<RefCell<App>>) {
    let weak = Rc::downgrade(app);
    window.on_shortcut_filter(move |value| {
        let Some(app) = weak.upgrade() else { return };
        let mut app = app.borrow_mut();
        if let Some(editor) = &mut app.shortcut_editor {
            editor.filter = value.to_string();
            editor.command = editor.command.filter(|id| editor.matches(id)).or_else(|| {
                shortcuts::commands()
                    .map(|(_, id)| id)
                    .find(|id| editor.matches(id))
            });
            editor.update_selection();
        }
        app.sync_editor(true);
    });
    let weak = Rc::downgrade(app);
    window.on_select_shortcut_command(move |id| {
        let Some(app) = weak.upgrade() else { return };
        let mut app = app.borrow_mut();
        if let Some(editor) = &mut app.shortcut_editor {
            editor.command = shortcuts::commands()
                .map(|(_, command)| command)
                .find(|command| *command == id.as_str());
            editor.update_selection();
        }
        app.sync_editor(false);
    });
    let weak = Rc::downgrade(app);
    window.on_edit_shortcut(move |operation, key| {
        let Some(app) = weak.upgrade() else { return };
        let mut app = app.borrow_mut();
        if let Some(editor) = &mut app.shortcut_editor {
            editor.edit(operation.as_str(), key.as_str());
        }
        app.sync_editor(matches!(operation.as_str(), "assign" | "remove" | "reset"));
    });
}

impl Editor {
    fn matches(&self, id: &str) -> bool {
        let filter = self.filter.to_lowercase();
        id.contains(&filter)
            || shortcuts::label(id)
                .is_some_and(|label| crate::locale::tr(label).to_lowercase().contains(&filter))
    }

    fn update_selection(&mut self) {
        self.recording = false;
        self.candidate = None;
        self.notice.clear();
    }

    fn edit(&mut self, operation: &str, key: &str) {
        match operation {
            "record" => {
                if self.command.is_some() {
                    self.update_selection();
                    self.recording = true;
                }
            }
            "stop" => self.recording = false,
            "assign" => {
                if let (Some(key), Some(command)) = (self.candidate.take(), self.command) {
                    shortcuts::change(&mut self.overrides, key, Some(command));
                    self.notice = "Zugewiesen. Anwenden speichert die Änderung.".into();
                }
            }
            "remove" => {
                if shortcuts::effective(&self.overrides).get(key).copied() == self.command
                    && self.command.is_some()
                {
                    shortcuts::change(&mut self.overrides, key.to_owned(), None);
                    self.notice = "Entfernt. Anwenden speichert die Änderung.".into();
                }
            }
            "reset" => {
                self.overrides.clear();
                self.update_selection();
                self.notice = "Standardbelegung vorgemerkt.".into();
            }
            _ => (),
        }
    }

    fn record(&mut self, chord: Option<String>) {
        if let Some(chord) = chord {
            if chord == "Escape" {
                self.recording = false;
                self.notice.clear();
                return;
            }
            if shortcuts::valid_chord(&chord) {
                self.recording = false;
                self.candidate = Some(chord);
                self.notice.clear();
                return;
            }
        }
        self.notice = "Diese Taste ist reserviert oder kann hier nicht belegt werden.".into();
    }
}

impl App {
    pub(crate) fn open_settings(&mut self, page: super::SettingsPage) {
        self.sync_ui_settings();
        self.shortcut_editor = Some(Editor {
            overrides: self.profile.shortcuts.clone(),
            command: Some("search"),
            filter: String::new(),
            candidate: None,
            recording: false,
            notice: String::new(),
        });
        self.sync_editor(true);
        let ui = self.ui();
        ui.set_settings_error("".into());
        ui.set_settings_error_page(-1);
        ui.set_settings_status("".into());
        ui.set_settings_page(page as i32);
        ui.set_settings_languages(super::strings(
            crate::locale::LANGUAGES.map(crate::locale::label),
        ));
        ui.set_settings_open(true);
    }

    pub(crate) fn close_settings(&mut self) {
        self.shortcut_editor = None;
        let ui = self.ui();
        ui.set_shortcut_recording(false);
        ui.set_settings_open(false);
        ui.invoke_focus_panels();
    }

    pub(super) fn draft_shortcuts(&self) -> Option<&shortcuts::Overrides> {
        self.shortcut_editor
            .as_ref()
            .map(|editor| &editor.overrides)
    }

    pub(crate) fn reload_shortcuts(&mut self) {
        self.bindings = shortcuts::effective(&self.profile.shortcuts);
        self.sync_shortcut_labels();
    }

    pub(crate) fn sync_shortcut_labels(&self) {
        let key = |id| {
            shortcuts::keys_for(&self.bindings, id)
                .first()
                .map_or_else(String::new, |key| shortcuts::display(key))
        };
        self.ui().set_shortcut_labels(ShortcutLabels {
            refresh: key("refresh").into(),
            view: key("view").into(),
            edit: key("edit").into(),
            copy: key("copy").into(),
            move_file: key("move").into(),
            mkdir: key("mkdir").into(),
            delete: key("delete").into(),
            palette: key("palette").into(),
            job_details: key("job-details").into(),
        });
        self.sync_command_menu();
    }

    pub(crate) fn keyboard_help(&self) -> String {
        let mut result = String::new();
        for (label, id) in shortcuts::commands() {
            let keys = shortcuts::keys_for(&self.bindings, id);
            if !keys.is_empty() {
                let keys = keys
                    .into_iter()
                    .map(shortcuts::display)
                    .collect::<Vec<_>>()
                    .join(" / ");
                writeln!(result, "{keys:27} {label}").unwrap();
            }
        }
        result
    }

    pub(super) fn sync_editor(&self, rebuild_list: bool) {
        let Some(editor) = &self.shortcut_editor else {
            return;
        };
        self.ui()
            .set_shortcut_dirty(editor.overrides != self.profile.shortcuts);
        let bindings = shortcuts::effective(&editor.overrides);
        let ui = self.ui();
        if rebuild_list {
            ui.set_shortcut_query(editor.filter.as_str().into());
            let mut rows: Vec<_> = shortcuts::commands()
                .filter(|(_, id)| editor.matches(id))
                .map(|(label, id)| ShortcutRow {
                    key: id.into(),
                    label: crate::locale::tr(label).into(),
                })
                .collect();
            rows.sort_by_cached_key(|row| row.label.to_lowercase());
            ui.set_shortcut_commands(ModelRc::new(VecModel::from(rows)));
        }
        let selected = editor.command.unwrap_or_default();
        ui.set_shortcut_command(selected.into());
        ui.set_shortcut_title(
            shortcuts::label(selected)
                .map(crate::locale::tr)
                .unwrap_or("Kein Befehl gewählt")
                .into(),
        );
        ui.set_shortcut_assigned(ModelRc::new(VecModel::from(
            shortcuts::keys_for(&bindings, selected)
                .into_iter()
                .map(|key| ShortcutRow {
                    key: key.into(),
                    label: shortcuts::display(key).into(),
                })
                .collect::<Vec<_>>(),
        )));
        ui.set_shortcut_recording(editor.recording);
        ui.set_shortcut_candidate(
            editor
                .candidate
                .as_deref()
                .map(shortcuts::display)
                .unwrap_or_default()
                .into(),
        );
        let occupied = editor
            .candidate
            .as_ref()
            .and_then(|key| bindings.get(key))
            .filter(|id| **id != selected)
            .and_then(|id| shortcuts::label(id));
        ui.set_shortcut_conflict(occupied.is_some());
        ui.set_shortcut_notice(
            occupied
                .map_or_else(
                    || editor.notice.clone(),
                    |label| format!("Belegt durch „{label}“."),
                )
                .into(),
        );
    }
}

pub(super) fn window_key(
    app: &Weak<RefCell<App>>,
    event: &WindowEvent,
    modifiers: &mut ModifiersState,
) -> bool {
    if let WindowEvent::ModifiersChanged(value) = event {
        *modifiers = value.state();
        return false;
    }
    if let WindowEvent::Focused(false) = event {
        *modifiers = ModifiersState::empty();
    }
    let WindowEvent::KeyboardInput {
        event,
        is_synthetic: false,
        ..
    } = event
    else {
        return false;
    };
    if event.state != ElementState::Pressed {
        return false;
    }
    let Some(app) = app.upgrade() else {
        return false;
    };
    if shortcuts::is_modifier(event) {
        return false;
    }
    let chord = shortcuts::from_event(event, *modifiers);
    if route(&app, chord, event.repeat) {
        return true;
    }
    let Key::Character(text) = &event.logical_key else {
        return false;
    };
    let Ok(mut state) = app.try_borrow_mut() else {
        return false;
    };
    if event.repeat || !quick_find_starts(state.profile.layout.quick_find_start, *modifiers, text) {
        return false;
    }
    let ui = state.ui();
    if state.context_menu_open
        || ui.get_settings_open()
        || !ui.get_dialog_kind().is_empty()
        || ui.get_drive_side() >= 0
        || !ui.invoke_file_list_focused()
        || state.panels[state.active].loading
    {
        return false;
    }
    state.quick_find_with_text(text);
    true
}

fn quick_find_starts(trigger: QuickFindStart, modifiers: ModifiersState, text: &str) -> bool {
    if text.chars().count() != 1 || !text.chars().all(char::is_alphanumeric) {
        return false;
    }
    let modifiers = modifiers - ModifiersState::SHIFT;
    match trigger {
        QuickFindStart::Shortcut => false,
        QuickFindStart::Typing => modifiers.is_empty(),
        QuickFindStart::Alt => modifiers == ModifiersState::ALT,
        QuickFindStart::CtrlAlt => modifiers == (ModifiersState::CONTROL | ModifiersState::ALT),
    }
}

#[cfg(test)]
#[test]
fn quick_find_only_accepts_printable_letters_and_the_configured_modifiers() {
    use ModifiersState as Mods;
    assert!(quick_find_starts(QuickFindStart::Typing, Mods::SHIFT, "Ä"));
    assert!(quick_find_starts(QuickFindStart::Alt, Mods::ALT, "1"));
    assert!(quick_find_starts(
        QuickFindStart::CtrlAlt,
        Mods::CONTROL | Mods::ALT,
        "z"
    ));
    assert!(!quick_find_starts(
        QuickFindStart::Typing,
        Mods::CONTROL,
        "a"
    ));
    assert!(!quick_find_starts(
        QuickFindStart::Shortcut,
        Mods::empty(),
        "a"
    ));
    assert!(!quick_find_starts(
        QuickFindStart::Typing,
        Mods::empty(),
        "\t"
    ));
}

fn route(app: &Rc<RefCell<App>>, chord: Option<String>, repeat: bool) -> bool {
    let command = {
        let Ok(mut state) = app.try_borrow_mut() else {
            return false;
        };
        let ui = state.ui();
        if let Some(editor) = state
            .shortcut_editor
            .as_mut()
            .filter(|editor| editor.recording)
        {
            if !repeat {
                editor.record(chord);
                state.sync_editor(false);
            }
            return true;
        }
        if state.context_menu_open
            || ui.get_settings_open()
            || !ui.get_dialog_kind().is_empty()
            || ui.get_drive_side() >= 0
            || ui.get_job_activity_open()
        {
            return false;
        }
        let Some(key) = chord else { return false };
        let Some(&command) = state.bindings.get(&key) else {
            return false;
        };
        if !ui.invoke_file_list_focused() && !shortcuts::available_during_edit(&key, command) {
            return false;
        }
        command
    };
    if repeat && !shortcuts::repeatable(command) {
        return true;
    }
    activate(app, command);
    true
}

pub(super) fn activate(app: &Rc<RefCell<App>>, command: &str) {
    let ui = app.borrow().ui();
    match command {
        "context-menu" => ui.invoke_open_keyboard_context(),
        "drive-left" => app.borrow().show_drives(0),
        "drive-right" => app.borrow().show_drives(1),
        "focus-path" => ui.invoke_focus_active_path(),
        "focus-filter" => ui.invoke_focus_active_filter(),
        "focus-command-line" => ui.invoke_focus_command_line(),
        _ => app.borrow_mut().action(command),
    }
}

#[cfg(windows)]
pub(super) fn ensure_native_hook(window: &slint::Window, app: &Weak<RefCell<App>>) {
    use slint::winit_030::WinitWindowAccessor;
    use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Some(app) = app.upgrade() else { return };
    let Ok(mut state) = app.try_borrow_mut() else {
        return;
    };
    if state.shortcut_hook.is_some() {
        return;
    }
    // Slint may defer creation of the HWND until the winit event loop starts.
    let handle = window
        .with_winit_window(|native| {
            let handle = native.window_handle().ok()?;
            let RawWindowHandle::Win32(handle) = handle.as_raw() else {
                return None;
            };
            Some(handle.hwnd)
        })
        .flatten();
    let Some(handle) = handle else {
        return;
    };
    let weak = Rc::downgrade(&app);
    match arco_windows::ShortcutHook::install(handle, move |event| {
        let Some(app) = weak.upgrade() else {
            return false;
        };
        let chord = shortcuts::chord(event.key, event.control, event.alt, event.shift);
        let plain_f10 = chord == "F10";
        route(&app, Some(chord), event.repeat)
            || (!plain_f10
                && app
                    .try_borrow()
                    .is_ok_and(|state| state.ui().invoke_file_list_focused()))
    }) {
        Ok(hook) => state.shortcut_hook = Some(hook),
        Err(error) => state.status(error.to_string()),
    }
}
