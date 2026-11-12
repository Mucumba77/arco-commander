use super::{App, FILE_FONT_SIZES};
use crate::{NtfsDraft, SettingsData, display_path, locale};
use arco_core::files::NtfsCopyOptions;
use arco_core::profile::{LayoutSettings, ListFont, Profile, QuickFindStart};
use std::io;
use std::path::{Path, PathBuf};

#[repr(i32)]
pub(crate) enum SettingsPage {
    General = 0,
    Transfers = 5,
    Programs = 6,
}

pub(super) fn path_label(path: Option<&Path>) -> String {
    path.map(display_path).unwrap_or_default()
}

pub(super) fn ntfs_draft(options: NtfsCopyOptions) -> NtfsDraft {
    NtfsDraft {
        streams: options.streams,
        permissions: options.permissions,
        owner: options.owner,
        audit: options.audit,
        hardlinks: options.hardlinks,
        sparse: options.sparse,
        creation_time: options.creation_time,
        copy_links: options.copy_links,
    }
}

impl App {
    pub(super) fn apply_settings(&mut self, value: SettingsData, close: bool) {
        self.ui().set_settings_error_page(-1);
        self.ui().set_settings_error("".into());
        if let Err(error) = self.commit_settings(value, close) {
            self.ui().set_settings_error(error.to_string().into());
            let ui = self.ui();
            ui.set_settings_validation_revision(
                ui.get_settings_validation_revision().wrapping_add(1),
            );
        }
    }

    fn commit_settings(&mut self, value: SettingsData, close: bool) -> io::Result<()> {
        let mut draft = self.profile_snapshot();
        apply_configuration(&mut draft, &value).map_err(|invalid| {
            self.ui().set_settings_error_page(invalid.page);
            invalid.error
        })?;
        if let Some(shortcuts) = self.draft_shortcuts() {
            draft.shortcuts.clone_from(shortcuts);
        }

        let previous_language = self.profile.language;
        let language_changed = previous_language != draft.language;
        if language_changed {
            locale::select(draft.language).map_err(io::Error::other)?;
        }
        if self.save_profile
            && let Err(error) = draft.save(&self.profile_path)
        {
            if language_changed {
                let _ = locale::select(previous_language);
            }
            return Err(error);
        }

        // set_hidden needs the previous value to refresh the panels and trees.
        draft.show_hidden = self.profile.show_hidden;
        self.profile = draft;
        self.set_hidden(value.show_hidden);
        self.sync_ui_settings();
        self.reload_shortcuts();
        self.sync_editor(language_changed);
        self.publish(0);
        self.publish(1);
        self.ui().set_settings_error("".into());
        self.status(if self.save_profile {
            "Einstellungen angewendet."
        } else {
            "Einstellungen gelten für diese Sitzung."
        });
        self.ui().set_settings_status(
            locale::tr(if self.save_profile {
                "Einstellungen angewendet."
            } else {
                "Einstellungen gelten für diese Sitzung."
            })
            .into(),
        );
        let ui = self.ui();
        ui.set_settings_applied_revision(ui.get_settings_applied_revision().wrapping_add(1));
        if close {
            self.close_settings();
        }
        Ok(())
    }
}

#[derive(Debug)]
struct ConfigurationError {
    page: i32,
    error: io::Error,
}

impl ConfigurationError {
    fn at(page: i32) -> impl FnOnce(io::Error) -> Self {
        move |error| Self { page, error }
    }
}

fn apply_configuration(
    draft: &mut Profile,
    value: &SettingsData,
) -> Result<(), ConfigurationError> {
    let language = locale::LANGUAGES
        .get(value.language as usize)
        .copied()
        .ok_or_else(|| invalid("Wähle eine verfügbare Oberflächensprache."))
        .map_err(ConfigurationError::at(0))?;
    let editor = optional_path(
        &value.editor,
        draft.editor.as_deref(),
        |path| path.is_absolute() && path.is_file(),
        "Editor benötigt einen absoluten Programmpfad",
    )
    .map_err(ConfigurationError::at(6))?;
    let everything = optional_path(
        &value.everything,
        draft.everything_cli.as_deref(),
        |path| {
            path.is_absolute()
                && path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
        },
        "Gib den absoluten EXE-Pfad für Everything an.",
    )
    .map_err(ConfigurationError::at(6))?;
    let transfer_log = optional_path(
        &value.transfer_log,
        draft.transfer_log.as_deref(),
        |path| path.is_absolute() && path.file_name().is_some(),
        "Gib einen absoluten Pfad zur Protokolldatei ein.",
    )
    .map_err(ConfigurationError::at(5))?;
    let transfer_rate_kib = value
        .transfer_rate
        .trim()
        .parse()
        .map_err(|_| invalid("Gib eine ganze Zahl für KiB/s ein."))
        .map_err(ConfigurationError::at(5))?;

    draft.language = language;
    draft.editor = editor;
    draft.everything_cli = everything;
    draft.transfer_rate_kib = transfer_rate_kib;
    draft.transfer_log = transfer_log;
    draft.transfer_descriptions = value.transfer_comments;
    draft.transfer_ntfs = NtfsCopyOptions {
        streams: value.ntfs.streams,
        permissions: value.ntfs.permissions,
        owner: value.ntfs.owner,
        audit: value.ntfs.audit,
        hardlinks: value.ntfs.hardlinks,
        sparse: value.ntfs.sparse,
        creation_time: value.ntfs.creation_time,
        copy_links: value.ntfs.copy_links,
    };
    draft.layout = LayoutSettings {
        toolbar: value.toolbar,
        panel_toolbar: value.panel_toolbar,
        drive_buttons: value.drive_buttons,
        drive_list: value.drive_list,
        folder_tabs: value.folder_tabs,
        current_path: value.current_path,
        breadcrumbs: value.breadcrumbs,
        history_buttons: value.history_buttons,
        column_headers: value.column_headers,
        panel_status: value.panel_status,
        command_line: value.command_line,
        function_keys: value.function_keys,
        numpad_folders: value.numpad_folders,
        quick_find_start: match value.quick_find_start {
            1 => QuickFindStart::Typing,
            2 => QuickFindStart::Alt,
            3 => QuickFindStart::CtrlAlt,
            _ => QuickFindStart::Shortcut,
        },
    };
    draft.stacked = value.stacked;
    draft.light_mode = value.light;
    draft.system_theme = value.system_theme;
    draft.show_hidden = value.show_hidden;
    draft.list_font = match value.list_font {
        1 => ListFont::Arial,
        2 => ListFont::Verdana,
        _ => ListFont::SegoeUi,
    };
    draft.list_font_size = FILE_FONT_SIZES
        .get(value.list_font_size as usize)
        .copied()
        .unwrap_or(14);
    Ok(())
}

fn optional_path(
    value: &str,
    previous: Option<&Path>,
    valid: impl FnOnce(&Path) -> bool,
    error: &str,
) -> io::Result<Option<PathBuf>> {
    // An unrelated change must also apply when an old configured file has moved.
    if value == path_label(previous) {
        return Ok(previous.map(Path::to_path_buf));
    }
    let value = value.trim().trim_matches('"');
    let path = (!value.is_empty()).then(|| PathBuf::from(value));
    if path.as_deref().is_some_and(|path| !valid(path)) {
        return Err(invalid(error));
    }
    Ok(path)
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, locale::tr(message))
}

#[cfg(test)]
mod tests {
    use super::*;
    use arco_core::profile::Language;

    #[test]
    fn invalid_settings_identify_the_category_without_changing_saved_values() {
        for (value, page) in [
            (
                SettingsData {
                    language: -1,
                    ..Default::default()
                },
                0,
            ),
            (
                SettingsData {
                    editor: "relative.exe".into(),
                    ..Default::default()
                },
                6,
            ),
            (
                SettingsData {
                    everything: "relative.exe".into(),
                    ..Default::default()
                },
                6,
            ),
            (
                SettingsData {
                    transfer_log: "relative.log".into(),
                    ..Default::default()
                },
                5,
            ),
            (
                SettingsData {
                    transfer_rate: "-50".into(),
                    ..Default::default()
                },
                5,
            ),
        ] {
            let mut profile = Profile::default();
            let error = apply_configuration(&mut profile, &value).unwrap_err();
            assert_eq!(error.page, page);
            assert_eq!(error.error.kind(), io::ErrorKind::InvalidInput);
            assert_eq!(profile.language, Language::German);
            assert!(profile.editor.is_none());
            assert_eq!(profile.transfer_rate_kib, 0);
        }
    }

    #[test]
    fn validates_integrated_settings_before_changing_the_draft() {
        let mut profile = Profile::default();
        let settings = SettingsData {
            language: 2,
            transfer_rate: "invalid".into(),
            ..Default::default()
        };
        assert!(apply_configuration(&mut profile, &settings).is_err());
        assert_eq!(profile.language, Language::German);
        let settings = SettingsData {
            transfer_rate: "128".into(),
            ..settings
        };
        apply_configuration(&mut profile, &settings).unwrap();
        assert_eq!(profile.language, Language::French);
        assert_eq!(profile.transfer_rate_kib, 128);
    }

    #[test]
    fn retaining_a_saved_path_does_not_require_the_program_to_be_online() {
        let previous = Path::new("Z:/Tools/editor.exe");
        let text = path_label(Some(previous));
        let preserved = optional_path(&text, Some(previous), |_| false, "Invalid").unwrap();
        assert_eq!(preserved.as_deref(), Some(previous));
        assert!(
            optional_path("relative.exe", Some(previous), Path::is_absolute, "Invalid").is_err()
        );
    }
}
