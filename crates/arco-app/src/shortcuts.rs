use crate::commands::COMMANDS;
use slint::winit_030::winit::event::KeyEvent;
use slint::winit_030::winit::keyboard::{Key, KeyCode, ModifiersState, PhysicalKey};
use slint::winit_030::winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
use std::collections::BTreeMap;

pub type Overrides = BTreeMap<String, Option<String>>;
pub type Bindings = BTreeMap<String, &'static str>;

const DEFAULTS: &[(&str, &str)] = &[
    ("F1", "help"),
    ("F2", "refresh"),
    ("F3", "view"),
    ("F4", "edit"),
    ("F5", "copy"),
    ("F6", "move"),
    ("F7", "mkdir"),
    ("F8", "delete"),
    ("Alt+F1", "drive-left"),
    ("Alt+F2", "drive-right"),
    ("Alt+F5", "zip-pack"),
    ("Alt+F6", "zip-extract"),
    ("Alt+F7", "search"),
    ("Alt+Shift+F7", "search-separate"),
    ("Alt+F9", "zip-extract"),
    ("Alt+Shift+F9", "zip-test"),
    ("Shift+F4", "new-file"),
    ("Shift+F2", "compare-dirs"),
    ("Shift+F6", "rename"),
    ("Shift+F8", "delete-permanent"),
    ("Shift+F10", "context-menu"),
    ("Menu", "context-menu"),
    ("Ctrl+F1", "short-view"),
    ("Ctrl+Shift+F1", "thumbnail-view"),
    ("Shift+F1", "column-sets"),
    ("Ctrl+F2", "long-view"),
    ("Ctrl+Shift+F2", "comment-view"),
    ("Ctrl+Z", "edit-comment"),
    ("Ctrl+F3", "sort-name"),
    ("Ctrl+F4", "sort-extension"),
    ("Ctrl+F5", "sort-time"),
    ("Ctrl+F6", "sort-size"),
    ("Ctrl+F8", "directory-tree"),
    ("Ctrl+Shift+F8", "separate-tree-cycle"),
    ("Ctrl+F9", "print-file"),
    ("Ctrl+F10", "clear-filter"),
    ("Ctrl+F11", "only-programs"),
    ("Alt+Enter", "properties"),
    ("Alt+D", "focus-path"),
    ("Alt+Left", "back"),
    ("Alt+Right", "forward"),
    ("Ctrl+A", "select-all"),
    ("Ctrl+Shift+A", "tab-list"),
    ("Ctrl+B", "branch"),
    ("Ctrl+Shift+B", "branch-selected"),
    ("Ctrl+C", "clipboard-copy"),
    ("Ctrl+D", "favorites"),
    ("Ctrl+F", "ftp-profiles"),
    ("Ctrl+J", "job-details"),
    ("Ctrl+K", "palette"),
    ("Ctrl+L", "space-usage"),
    ("Ctrl+Shift+L", "focus-command-line"),
    ("Alt+Shift+Enter", "folder-sizes"),
    ("Ctrl+M", "multi-rename"),
    ("Ctrl+N", "ftp-connect"),
    ("Ctrl+Q", "quick-view"),
    ("Ctrl+R", "refresh"),
    ("Ctrl+S", "focus-filter"),
    ("Ctrl+Shift+S", "quick-find"),
    ("Ctrl+T", "new-tab"),
    ("Ctrl+U", "swap"),
    ("Ctrl+V", "clipboard-paste"),
    ("Ctrl+W", "close-tab"),
    ("Ctrl+X", "clipboard-cut"),
    ("Ctrl+,", "settings"),
    ("Tab", "switch-panel"),
    ("Ctrl+Tab", "next-tab"),
    ("Ctrl+Shift+Tab", "previous-tab"),
    ("Up", "cursor-up"),
    ("Down", "cursor-down"),
    ("Shift+Up", "select-up"),
    ("Shift+Down", "select-down"),
    ("Left", "tree-collapse"),
    ("Right", "tree-expand"),
    ("PageUp", "page-up"),
    ("PageDown", "page-down"),
    ("Ctrl+PageUp", "up"),
    ("Ctrl+PageDown", "open"),
    ("Home", "cursor-first"),
    ("End", "cursor-last"),
    ("Insert", "mark"),
    ("Space", "mark"),
    ("Delete", "delete"),
    ("Shift+Delete", "delete-permanent"),
    ("Enter", "open"),
    ("NumpadEnter", "open"),
    ("Backspace", "up"),
    ("NumpadAdd", "mask-add"),
    ("Shift+NumpadAdd", "mask-add-folders"),
    ("NumpadSubtract", "mask-remove"),
    ("Shift+NumpadSubtract", "mask-remove-files"),
    ("NumpadMultiply", "invert-numberpad"),
    ("Shift+NumpadMultiply", "invert"),
    ("NumpadDivide", "restore-selection"),
    ("Ctrl+NumpadAdd", "select-all-numberpad"),
    ("Ctrl+Shift+NumpadAdd", "select-all"),
    ("Ctrl+NumpadSubtract", "clear-selection"),
    ("Alt+NumpadAdd", "select-extension"),
    ("Alt+NumpadSubtract", "unselect-extension"),
];

const EXTRA_COMMANDS: &[(&str, &str)] = &[
    ("Ast im Ordnerbaum schließen", "tree-collapse"),
    ("Ast im Ordnerbaum öffnen", "tree-expand"),
    ("Aktiven Eintrag öffnen", "open"),
    ("Aktuelles Fenster neu laden", "refresh"),
    ("Anzeigen", "view"),
    ("Bearbeiten", "edit"),
    ("Kopieren", "copy"),
    ("Verschieben", "move"),
    ("Umbenennen", "rename"),
    ("Neuer Ordner", "mkdir"),
    ("Neue Datei", "new-file"),
    ("In den Papierkorb", "delete"),
    ("Endgültig löschen", "delete-permanent"),
    ("Windows-Kontextmenü", "context-menu"),
    ("Laufwerk links wählen", "drive-left"),
    ("Laufwerk rechts wählen", "drive-right"),
    ("Pfad bearbeiten", "focus-path"),
    ("Befehlszeile fokussieren", "focus-command-line"),
    ("Schnellfilter fokussieren", "focus-filter"),
    ("Kurzansicht", "short-view"),
    ("Detailansicht", "long-view"),
    ("Nach Name sortieren", "sort-name"),
    ("Nach Erweiterung sortieren", "sort-extension"),
    ("Nach Datum sortieren", "sort-time"),
    ("Nach Größe sortieren", "sort-size"),
    ("Nach Attributen sortieren", "sort-attributes"),
    ("Fenster wechseln", "switch-panel"),
    ("Neuer Ordner-Tab", "new-tab"),
    ("Ordner-Tab schließen", "close-tab"),
    ("Nächster Tab", "next-tab"),
    ("Vorheriger Tab", "previous-tab"),
    ("Cursor hoch", "cursor-up"),
    ("Cursor runter", "cursor-down"),
    ("Markieren und hoch", "select-up"),
    ("Markieren und runter", "select-down"),
    ("Seite hoch", "page-up"),
    ("Seite runter", "page-down"),
    ("Erster Eintrag", "cursor-first"),
    ("Letzter Eintrag", "cursor-last"),
    ("Eintrag markieren", "mark"),
    ("Alles markieren", "select-all"),
    (
        "Alles markieren nach Ziffernblock-Einstellung",
        "select-all-numberpad",
    ),
    ("Markierung aufheben", "clear-selection"),
    ("Auswahl erweitern nach Maske", "mask-add"),
    (
        "Auswahl mit Ordnern erweitern nach Maske",
        "mask-add-folders",
    ),
    ("Auswahl verkleinern nach Maske", "mask-remove"),
    ("Dateiauswahl verkleinern nach Maske", "mask-remove-files"),
    (
        "Auswahl umkehren nach Ziffernblock-Einstellung",
        "invert-numberpad",
    ),
    ("Gleiche Erweiterung markieren", "select-extension"),
    (
        "Markierung gleicher Erweiterung aufheben",
        "unselect-extension",
    ),
    ("Elternordner", "up"),
    ("Zum Laufwerksstamm", "root"),
    ("Im Verlauf zurück", "back"),
    ("Im Verlauf vor", "forward"),
    ("In Zwischenablage kopieren", "clipboard-copy"),
    ("In Zwischenablage ausschneiden", "clipboard-cut"),
    ("Aus Zwischenablage einfügen", "clipboard-paste"),
];

pub fn commands() -> impl Iterator<Item = (&'static str, &'static str)> {
    let mut seen = std::collections::HashSet::new();
    COMMANDS
        .iter()
        .chain(EXTRA_COMMANDS)
        .copied()
        .chain(crate::app::menu::actions())
        .filter(move |(_, key)| seen.insert(*key))
}

pub fn label(id: &str) -> Option<&'static str> {
    commands()
        .find(|(_, command)| *command == id)
        .map(|(label, _)| label)
}

pub fn effective(overrides: &Overrides) -> Bindings {
    let mut bindings = DEFAULTS
        .iter()
        .map(|(key, id)| (key.to_string(), *id))
        .collect::<Bindings>();
    for (key, command) in overrides {
        if !valid_chord(key) {
            continue;
        }
        match command {
            None => {
                bindings.remove(key);
            }
            Some(id) => {
                if let Some((_, command)) = commands().find(|(_, command)| *command == id) {
                    bindings.insert(key.clone(), command);
                }
            }
        }
    }
    bindings
}

pub fn change(overrides: &mut Overrides, key: String, command: Option<&str>) {
    let default = DEFAULTS
        .iter()
        .find(|(chord, _)| *chord == key)
        .map(|(_, id)| *id);
    if default == command {
        overrides.remove(&key);
    } else {
        overrides.insert(key, command.map(str::to_owned));
    }
}

pub fn keys_for<'a>(bindings: &'a Bindings, id: &str) -> Vec<&'a str> {
    let mut keys: Vec<_> = bindings
        .iter()
        .filter(|(_, command)| **command == id)
        .map(|(key, _)| key.as_str())
        .collect();
    keys.sort_by_key(|key| {
        // Prefer an explicitly reassigned chord over an unchanged alternative.
        let position = DEFAULTS
            .iter()
            .position(|(default, command)| default == key && *command == id);
        position.map_or(0, |position| position + 1)
    });
    keys
}

pub fn display(chord: &str) -> String {
    chord
        .split('+')
        .map(|part| match part {
            "Ctrl" => "Strg",
            "Shift" => "Umschalt",
            "Enter" => "Enter",
            "Backspace" => "Rücktaste",
            "PageUp" => "Bild↑",
            "PageDown" => "Bild↓",
            "Home" => "Pos1",
            "End" => "Ende",
            "Insert" => "Einfg",
            "Delete" => "Entf",
            "Space" => "Leertaste",
            "Menu" => "Menütaste",
            "Left" => "←",
            "Right" => "→",
            "Up" => "↑",
            "Down" => "↓",
            "Plus" => "+",
            "NumpadAdd" => "Num +",
            "NumpadSubtract" => "Num −",
            "NumpadMultiply" => "Num *",
            "NumpadDivide" => "Num /",
            "NumpadDecimal" => "Num Dezimal",
            "NumpadEnter" => "Num Enter",
            "NumpadEqual" => "Num =",
            "NumpadComma" => "Num ,",
            _ => part,
        })
        .map(crate::locale::tr)
        .collect::<Vec<_>>()
        .join("+")
}

pub fn chord(key: &str, control: bool, alt: bool, shift: bool) -> String {
    format!(
        "{}{}{}{}",
        if control { "Ctrl+" } else { "" },
        if alt { "Alt+" } else { "" },
        if shift { "Shift+" } else { "" },
        key
    )
}

pub fn from_event(event: &KeyEvent, modifiers: ModifiersState) -> Option<String> {
    let physical = match event.physical_key {
        PhysicalKey::Code(code) => format!("{code:?}"),
        _ => String::new(),
    };
    let key = if physical.starts_with("Numpad") && valid_key(&physical) {
        physical
    } else {
        match event.key_without_modifiers() {
            Key::Named(key) => match format!("{key:?}").as_str() {
                "ArrowLeft" => "Left".into(),
                "ArrowRight" => "Right".into(),
                "ArrowUp" => "Up".into(),
                "ArrowDown" => "Down".into(),
                "ContextMenu" => "Menu".into(),
                name => name.to_owned(),
            },
            Key::Character(text) if text == "+" => "Plus".into(),
            Key::Character(text) if text == " " => "Space".into(),
            Key::Character(text) if text.chars().count() == 1 => {
                let upper = text.to_uppercase();
                if upper.chars().count() == 1 {
                    upper
                } else {
                    text.to_string()
                }
            }
            _ => return None,
        }
    };
    valid_key(&key).then(|| {
        let chord = chord(
            &key,
            modifiers.control_key(),
            modifiers.alt_key(),
            modifiers.shift_key(),
        );
        if modifiers.super_key() {
            format!("Win+{chord}")
        } else {
            chord
        }
    })
}

pub fn is_modifier(event: &KeyEvent) -> bool {
    matches!(
        event.physical_key,
        PhysicalKey::Code(
            KeyCode::ControlLeft
                | KeyCode::ControlRight
                | KeyCode::AltLeft
                | KeyCode::AltRight
                | KeyCode::ShiftLeft
                | KeyCode::ShiftRight
                | KeyCode::SuperLeft
                | KeyCode::SuperRight
        )
    )
}

pub fn valid_chord(value: &str) -> bool {
    let mut key = value.strip_prefix("Win+").unwrap_or(value);
    for prefix in ["Ctrl+", "Alt+", "Shift+"] {
        key = key.strip_prefix(prefix).unwrap_or(key);
    }
    valid_key(key)
        && !matches!(
            value,
            "F10" | "Alt+F4" | "Alt+Tab" | "Alt+Shift+Tab" | "Alt+Space" | "Ctrl+Alt+Delete"
        )
        && key != "Escape"
}

pub fn tc_chord(value: &str) -> Option<String> {
    let (windows, value) = value
        .strip_prefix("WIN+")
        .map_or((false, value), |value| (true, value));
    let (mods, key) = value.split_once('+').unwrap_or(("", value));
    let mut seen = 0u8;
    for modifier in mods.bytes() {
        let bit = match modifier {
            b'C' => 1,
            b'A' => 2,
            b'S' => 4,
            _ => return None,
        };
        if seen & bit != 0 {
            return None;
        }
        seen |= bit;
    }
    let key = match key {
        "UP" => "Up",
        "DOWN" => "Down",
        "LEFT" => "Left",
        "RIGHT" => "Right",
        "ENTER" | "RETURN" => "Enter",
        "BACKSPACE" | "BACK" => "Backspace",
        "DEL" | "DELETE" => "Delete",
        "INS" | "INSERT" => "Insert",
        "HOME" => "Home",
        "END" => "End",
        "PGUP" | "PAGEUP" => "PageUp",
        "PGDN" | "PAGEDOWN" => "PageDown",
        "SPACE" => "Space",
        "TAB" => "Tab",
        "ADD" | "NUM+" => "NumpadAdd",
        "SUBTRACT" | "NUM-" => "NumpadSubtract",
        "MULTIPLY" | "NUM*" => "NumpadMultiply",
        "DIVIDE" | "NUM/" => "NumpadDivide",
        "PLUS" | "+" => "Plus",
        key => key,
    };
    let chord = chord(key, seen & 1 != 0, seen & 2 != 0, seen & 4 != 0);
    let chord = if windows {
        format!("Win+{chord}")
    } else {
        chord
    };
    valid_chord(&chord).then_some(chord)
}

fn valid_key(key: &str) -> bool {
    matches!(
        key,
        "Up" | "Down"
            | "Left"
            | "Right"
            | "Enter"
            | "Backspace"
            | "Delete"
            | "Insert"
            | "Home"
            | "End"
            | "PageUp"
            | "PageDown"
            | "Space"
            | "Tab"
            | "Menu"
            | "Plus"
            | "Escape"
            | "NumpadAdd"
            | "NumpadSubtract"
            | "NumpadMultiply"
            | "NumpadDivide"
            | "NumpadDecimal"
            | "NumpadEnter"
            | "NumpadEqual"
            | "NumpadComma"
    ) || key
        .strip_prefix('F')
        .and_then(|value| value.parse::<u8>().ok())
        .is_some_and(|n| (1..=24).contains(&n))
        || key
            .strip_prefix("Numpad")
            .is_some_and(|digit| digit.len() == 1 && digit.as_bytes()[0].is_ascii_digit())
        || {
            let mut chars = key.chars();
            chars
                .next()
                .is_some_and(|c| c != '+' && !c.is_control() && !c.is_whitespace())
                && chars.next().is_none()
        }
}

pub fn available_during_edit(chord: &str, command: &str) -> bool {
    matches!(
        command,
        "drive-left"
            | "drive-right"
            | "focus-filter"
            | "focus-path"
            | "focus-command-line"
            | "clear-filter"
            | "job-details"
            | "palette"
            | "settings"
            | "help"
            | "next-tab"
            | "previous-tab"
    ) && !matches!(
        chord,
        "Ctrl+A" | "Ctrl+C" | "Ctrl+V" | "Ctrl+X" | "Ctrl+Z" | "Ctrl+Y"
    ) && (chord.contains('+') || chord == "F1")
}

pub fn repeatable(id: &str) -> bool {
    matches!(
        id,
        "cursor-up" | "cursor-down" | "select-up" | "select-down" | "page-up" | "page-down" | "up"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn total_commander_chords_normalize_modifiers_and_keep_system_keys_reserved() {
        assert_eq!(tc_chord("CAS+F2").as_deref(), Some("Ctrl+Alt+Shift+F2"));
        assert_eq!(tc_chord("S+F10").as_deref(), Some("Shift+F10"));
        assert_eq!(tc_chord("A+UP").as_deref(), Some("Alt+Up"));
        assert_eq!(tc_chord("C+NUM+").as_deref(), Some("Ctrl+NumpadAdd"));
        assert_eq!(tc_chord("WIN+CS+F2").as_deref(), Some("Win+Ctrl+Shift+F2"));
        assert_eq!(tc_chord("WIN+C").as_deref(), Some("Win+C"));
        assert!(!valid_chord("Win+Win+F1"));
        assert_eq!(tc_chord("CC+F2"), None);
        assert_eq!(tc_chord("A+F4"), None);
        assert_eq!(tc_chord("F10"), None);
    }

    #[test]
    fn remapping_replaces_only_the_chosen_chord_and_can_be_reset() {
        let mut overrides = Overrides::new();
        change(&mut overrides, "F5".into(), Some("search"));
        change(&mut overrides, "Shift+F10".into(), None);
        let bindings = effective(&overrides);
        assert_eq!(bindings.get("F5"), Some(&"search"));
        assert_eq!(bindings.get("Alt+F7"), Some(&"search"));
        assert!(!bindings.contains_key("Shift+F10"));
        assert_eq!(bindings.get("Menu"), Some(&"context-menu"));
        change(&mut overrides, "F5".into(), Some("copy"));
        change(&mut overrides, "Shift+F10".into(), Some("context-menu"));
        assert!(overrides.is_empty());
    }

    #[test]
    fn invalid_saved_bindings_leave_default_behavior_intact() {
        let overrides = Overrides::from([
            ("F5".into(), Some("unknown-command".into())),
            ("Alt+F4".into(), Some("delete".into())),
            ("Ctrl+NumpadAdd".into(), Some("mask".into())),
        ]);
        let bindings = effective(&overrides);
        assert_eq!(bindings.get("F5"), Some(&"copy"));
        assert!(!bindings.contains_key("Alt+F4"));
        assert_eq!(bindings.get("Ctrl+NumpadAdd"), Some(&"mask"));
    }

    #[test]
    fn all_default_bindings_have_a_unique_key_and_a_registered_command() {
        let bindings = effective(&Overrides::new());
        assert_eq!(bindings.len(), DEFAULTS.len());
        for &(key, command) in DEFAULTS {
            assert!(valid_chord(key), "{key}");
            assert!(label(command).is_some(), "{command}");
        }
    }

    #[test]
    fn profile_preserves_reassigned_and_unbound_chords() {
        use arco_core::profile::Profile;
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("profile.json");
        std::fs::write(&path, "{}").unwrap();
        let mut profile = Profile::load(&path).unwrap();
        assert_eq!(
            effective(&profile.shortcuts).get("Shift+F10"),
            Some(&"context-menu")
        );
        change(&mut profile.shortcuts, "Ctrl+Y".into(), Some("compare"));
        change(&mut profile.shortcuts, "F5".into(), None);
        profile.save(&path).unwrap();

        let profile = Profile::load(&path).unwrap();
        let bindings = effective(&profile.shortcuts);
        assert_eq!(bindings.get("Ctrl+Y"), Some(&"compare"));
        assert!(!bindings.contains_key("F5"));
        assert_eq!(bindings.get("F6"), Some(&"move"));
    }
}
