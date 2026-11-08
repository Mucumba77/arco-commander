use super::App;
use crate::commands::{Dialog, Pending, valid_button_command};
use crate::{ButtonbarRow, display_path};
use arco_core::profile::{
    self, ButtonbarAction, ButtonbarButton, ButtonbarTemplate, TcButtonbarEntry,
};
use slint::{ModelRc, VecModel};
use std::fmt::Write as _;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

const BARS: [&str; 3] = [
    "Obere Buttonbar",
    "Buttonbar zwischen den Dateifenstern",
    "Startermenü",
];
const ICONS: &[&str] = &[
    "ordner",
    "datei",
    "suche",
    "kopieren",
    "verschieben",
    "archiv",
    "netz",
    "terminal",
    "stern",
    "einstellungen",
];
const LIMITS: [usize; 3] = [16, 6, 64];

impl App {
    fn buttons(&self, bar: usize) -> &[ButtonbarButton] {
        match bar {
            0 | 1 => &self.profile.buttonbars[bar],
            2 => &self.profile.starter,
            _ => &[],
        }
    }

    pub(crate) fn starter(&mut self) {
        let names: Vec<_> = self
            .profile
            .starter
            .iter()
            .map(|button| button.label.clone())
            .collect();
        let options: Vec<_> = names.iter().map(String::as_str).collect();
        if options.is_empty() {
            self.buttonbar_list(2);
        } else {
            self.dialog(
                "confirm",
                Pending::Starter,
                Dialog {
                    title: "Starter",
                    options: &options,
                    ..Default::default()
                },
            );
        }
    }

    pub(crate) fn buttonbar_templates(&mut self) {
        let names: Vec<_> = self.profile.buttonbar_templates.keys().cloned().collect();
        let options: Vec<_> = std::iter::once("Aktuellen Stand als Vorlage speichern")
            .chain(names.iter().map(String::as_str))
            .collect();
        self.dialog(
            "confirm",
            Pending::ButtonbarTemplates(names.clone()),
            Dialog {
                title: "Buttonbar- und Startervorlagen",
                options: &options,
                ..Default::default()
            },
        );
    }

    pub(crate) fn select_buttonbar_template(&mut self, name: Option<String>) {
        if let Some(name) = name {
            let title = name.clone();
            self.dialog(
                "confirm",
                Pending::ButtonbarTemplate(name),
                Dialog {
                    title: &title,
                    options: &[
                        "Vorlage anwenden",
                        "Vorlage mit aktuellem Stand ersetzen",
                        "Vorlage entfernen",
                    ],
                    ..Default::default()
                },
            );
        } else {
            self.dialog(
                "input",
                Pending::ButtonbarTemplateSave,
                Dialog {
                    title: "Buttonbar-Vorlage speichern",
                    placeholder: "Vorlagenname",
                    ..Default::default()
                },
            );
        }
    }

    pub(crate) fn save_buttonbar_template(&mut self, name: &str) -> io::Result<()> {
        let name = name.trim();
        if name.is_empty() || name.chars().any(char::is_control) || name.chars().count() > 80 {
            return Err(io::Error::other(
                "Gib einen Vorlagennamen mit höchstens 80 Zeichen ein.",
            ));
        }
        let mut draft = self.profile_snapshot();
        if draft.buttonbar_templates.contains_key(name) {
            return Err(io::Error::other(
                "Diese Buttonbar-Vorlage existiert bereits.",
            ));
        }
        draft
            .buttonbar_templates
            .insert(name.into(), self.buttonbar_snapshot());
        self.commit_buttonbar_profile(draft)?;
        self.status("Buttonbar-Vorlage gespeichert.");
        Ok(())
    }

    pub(crate) fn apply_buttonbar_template(
        &mut self,
        name: String,
        action: usize,
    ) -> io::Result<()> {
        let mut draft = self.profile_snapshot();
        let saved = draft
            .buttonbar_templates
            .get(&name)
            .cloned()
            .ok_or_else(|| io::Error::other("Die Vorlage fehlt."))?;
        match action {
            0 => {
                draft.buttonbars = saved.bars;
                draft.starter = saved.starter;
                draft.layout.toolbar |= !draft.buttonbars[0].is_empty();
                draft.layout.panel_toolbar |= !draft.buttonbars[1].is_empty();
            }
            1 => {
                draft
                    .buttonbar_templates
                    .insert(name, self.buttonbar_snapshot());
            }
            2 => {
                draft.buttonbar_templates.remove(&name);
            }
            _ => return Ok(()),
        }
        self.commit_buttonbar_profile(draft)?;
        self.status("Buttonbar- und Startervorlagen aktualisiert.");
        Ok(())
    }

    fn buttonbar_snapshot(&self) -> ButtonbarTemplate {
        ButtonbarTemplate {
            bars: self.profile.buttonbars.clone(),
            starter: self.profile.starter.clone(),
        }
    }

    pub(crate) fn open_buttonbars(&mut self) {
        self.dialog(
            "confirm",
            Pending::Buttonbar,
            Dialog {
                title: "Eigene Buttonbars",
                description: "Wähle die Leiste, die du bearbeiten möchtest.",
                options: &BARS,
                ..Default::default()
            },
        );
    }

    pub(crate) fn buttonbar_list(&mut self, bar: usize) {
        if bar >= BARS.len() {
            return;
        }
        let buttons = self.buttons(bar);
        let choices: Vec<_> = [
            "Neue Schaltfläche",
            "TC-Buttonbar importieren",
            "TC-Buttonbar exportieren",
        ]
        .into_iter()
        .chain(buttons.iter().map(|button| button.label.as_str()))
        .collect();
        let choices: Vec<_> = choices.into_iter().map(str::to_owned).collect();
        let options: Vec<_> = choices.iter().map(String::as_str).collect();
        self.dialog("confirm", Pending::ButtonbarList(bar), Dialog {
            title: BARS[bar],
            description: "Die Leiste zeigt deine Schaltflächen in dieser Reihenfolge. Ist sie leer, erscheint die Standardbelegung.",
            options: &options,
            ..Default::default()
        });
    }

    pub(crate) fn buttonbar_select(&mut self, bar: usize, option: usize) {
        if matches!(option, 1 | 2) {
            self.dialog(
                "input",
                Pending::ButtonbarFile(bar, option == 1),
                Dialog {
                    title: if option == 1 {
                        "TC-Buttonbar importieren"
                    } else {
                        "TC-Buttonbar exportieren"
                    },
                    description: if option == 1 {
                        "Wähle eine .bar-Datei von Total Commander."
                    } else {
                        "Gib einen freien Pfad für die .bar-Datei ein."
                    },
                    placeholder: "C:\\Ordner\\Leiste.bar",
                    ..Default::default()
                },
            );
        } else if let Some(index) = option.checked_sub(3) {
            let Some(button) = self.buttons(bar).get(index) else {
                return;
            };
            let description = format!("Schaltfläche „{}“", button.label);
            self.dialog(
                "confirm",
                Pending::ButtonbarAction(bar, index),
                Dialog {
                    title: BARS[bar],
                    description: &description,
                    options: &[
                        "Bearbeiten",
                        "Nach oben verschieben",
                        "Nach unten verschieben",
                        "Entfernen",
                    ],
                    ..Default::default()
                },
            );
        } else if self.buttons(bar).len() < LIMITS[bar] {
            self.buttonbar_editor(bar, None);
        } else {
            self.text(
                BARS[bar],
                &format!(
                    "Diese Leiste hat Platz für {} eigene Schaltflächen.",
                    LIMITS[bar]
                ),
            );
        }
    }

    pub(crate) fn buttonbar_file(
        &mut self,
        bar: usize,
        importing: bool,
        value: &str,
    ) -> io::Result<()> {
        let path = super::plugins::user_path(value);
        if !path.is_absolute()
            || !path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("bar"))
        {
            return Err(io::Error::other(
                "Gib einen absoluten Pfad mit der Endung .bar ein.",
            ));
        }
        if !importing {
            let data = export_tc_buttonbar(self.buttons(bar))?;
            let mut file =
                tempfile::NamedTempFile::new_in(path.parent().expect("absolute .bar path"))?;
            file.write_all(&[0xff, 0xfe])?;
            for unit in data.encode_utf16() {
                file.write_all(&unit.to_le_bytes())?;
            }
            file.as_file().sync_all()?;
            file.persist_noclobber(path).map_err(|error| error.error)?;
            self.status("TC-Buttonbar exportiert.");
            return Ok(());
        }
        let entries = profile::parse_tc_buttonbar(&profile::read_tc_text(&path)?)?;
        let total = entries.len();
        let buttons: Vec<_> = entries
            .into_iter()
            .filter_map(import_tc_button)
            .take(LIMITS[bar])
            .collect();
        if buttons.is_empty() {
            return Err(io::Error::other(
                "Diese .bar-Datei enthält keine übernehmbaren Schaltflächen.",
            ));
        }
        let preview = buttons
            .iter()
            .map(|button| button.label.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let description = format!(
            "{} von {total} Schaltflächen übernehmen und die Leiste ersetzen?",
            buttons.len()
        );
        self.dialog(
            "review",
            Pending::ButtonbarImport(bar, buttons),
            Dialog {
                title: BARS[bar],
                description: &description,
                value: &preview,
                ..Default::default()
            },
        );
        Ok(())
    }

    pub(crate) fn confirm_buttonbar_import(
        &mut self,
        bar: usize,
        buttons: Vec<ButtonbarButton>,
    ) -> io::Result<()> {
        self.commit_buttonbar(bar, buttons)?;
        self.status("TC-Buttonbar importiert.");
        Ok(())
    }

    pub(crate) fn buttonbar_action(
        &mut self,
        bar: usize,
        index: usize,
        option: usize,
    ) -> io::Result<()> {
        let mut buttons = self.buttons(bar).to_vec();
        if index >= buttons.len() {
            return Ok(());
        }
        match option {
            0 => {
                self.buttonbar_editor(bar, Some(index));
                return Ok(());
            }
            1 if index > 0 => buttons.swap(index, index - 1),
            2 if index + 1 < buttons.len() => buttons.swap(index, index + 1),
            3 => {
                buttons.remove(index);
            }
            _ => {
                self.buttonbar_list(bar);
                return Ok(());
            }
        }
        self.commit_buttonbar(bar, buttons)?;
        self.buttonbar_list(bar);
        Ok(())
    }

    fn buttonbar_editor(&mut self, bar: usize, index: Option<usize>) {
        let initial = index
            .and_then(|index| self.buttons(bar).get(index))
            .map_or_else(|| "Dateien suchen\nsuche\nsearch".into(), button_text);
        self.show_button_editor(bar, index, &initial, None);
    }

    fn show_button_editor(
        &mut self,
        bar: usize,
        index: Option<usize>,
        value: &str,
        error: Option<&str>,
    ) {
        let mut description = format!(
            "Je eine Zeile für Beschriftung, Symbol und Arco-Befehl, cm_-Alias oder absoluten EXE-Pfad. Nach einem EXE-Pfad folgt je Argument eine eigene Zeile. Für das Symbol gehen {} oder ein absoluter Pfad zu einem Bild bis 1 MiB.",
            ICONS.join(", ")
        );
        if let Some(error) = error {
            description = format!("{error}\n{description}");
        }
        self.dialog(
            "multiline",
            Pending::ButtonbarEdit(bar, index),
            Dialog {
                title: BARS[bar],
                description: &description,
                value,
                ..Default::default()
            },
        );
    }

    pub(crate) fn save_button(
        &mut self,
        bar: usize,
        index: Option<usize>,
        value: &str,
    ) -> io::Result<()> {
        let button = match parse_button(value) {
            Ok(button) => button,
            Err(error) => {
                self.show_button_editor(bar, index, value, Some(&error.to_string()));
                return Ok(());
            }
        };
        let mut buttons = self.buttons(bar).to_vec();
        if let Some(index) = index {
            let current = buttons
                .get_mut(index)
                .ok_or_else(|| io::Error::other("Die Schaltfläche fehlt."))?;
            *current = button;
        } else if buttons.len() < LIMITS[bar] {
            buttons.push(button);
        } else {
            return Err(io::Error::other("Die Buttonbar ist voll."));
        }
        self.commit_buttonbar(bar, buttons)?;
        self.buttonbar_list(bar);
        Ok(())
    }

    fn commit_buttonbar(&mut self, bar: usize, buttons: Vec<ButtonbarButton>) -> io::Result<()> {
        let mut draft = self.profile_snapshot();
        if bar == 2 {
            draft.starter = buttons;
        } else {
            if !buttons.is_empty() {
                if bar == 0 {
                    draft.layout.toolbar = true;
                } else {
                    draft.layout.panel_toolbar = true;
                }
            }
            draft.buttonbars[bar] = buttons;
        }
        self.commit_buttonbar_profile(draft)
    }

    fn commit_buttonbar_profile(&mut self, draft: arco_core::profile::Profile) -> io::Result<()> {
        if !self.save_profile {
            return Err(io::Error::other(
                "Das Arco-Profil konnte beim Start nicht geladen werden.",
            ));
        }
        draft.save(&self.profile_path)?;
        self.profile = draft;
        self.sync_ui_settings();
        Ok(())
    }

    pub(super) fn publish_buttonbars(&self) {
        let rows = |bar: usize| {
            let rows: Vec<_> = self.profile.buttonbars[bar]
                .iter()
                .take(LIMITS[bar])
                .map(|button| ButtonbarRow {
                    label: button.label.as_str().into(),
                    icon: button.icon.as_str().into(),
                    custom_icon: icon_image(&button.icon)
                        .map_or_else(slint::Image::default, |image| {
                            crate::preview::to_slint(&image)
                        }),
                })
                .collect();
            ModelRc::new(VecModel::from(rows))
        };
        self.ui().set_top_buttons(rows(0));
        self.ui().set_middle_buttons(rows(1));
    }

    pub(crate) fn run_button(&mut self, bar: usize, index: usize) {
        let Some(action) = self
            .buttons(bar)
            .get(index)
            .map(|button| button.action.clone())
        else {
            return;
        };
        match action {
            ButtonbarAction::Internal { command } => {
                self.command_line(&command);
            }
            ButtonbarAction::Program { path, arguments } => {
                if !program_path(&path) {
                    self.status("Die Schaltfläche benötigt einen absoluten EXE-Pfad.");
                    return;
                }
                let mut program = std::process::Command::new(path);
                program.args(arguments);
                if self.current().is_local() {
                    program.current_dir(&self.current().path);
                }
                self.report(program.spawn().map(|_| ()));
            }
        }
    }
}

fn button_text(button: &ButtonbarButton) -> String {
    let mut text = format!("{}\n{}\n", button.label, button.icon);
    match &button.action {
        ButtonbarAction::Internal { command } => text.push_str(command),
        ButtonbarAction::Program { path, arguments } => {
            text.push_str(&display_path(path));
            for argument in arguments {
                text.push('\n');
                text.push_str(argument);
            }
        }
    }
    text
}

fn program_path(path: &std::path::Path) -> bool {
    path.is_absolute()
        && path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
}

fn import_tc_button(entry: TcButtonbarEntry) -> Option<ButtonbarButton> {
    if entry.command.is_empty()
        || !entry.working_directory.is_empty()
        || entry.command.contains('%')
        || entry.parameters.contains('%')
        || entry.parameters.starts_with('?')
    {
        return None;
    }
    let path = PathBuf::from(entry.command.trim_matches('"'));
    let action = if program_path(&path) {
        let arguments = arco_windows::parse_windows_arguments(&entry.parameters).ok()?;
        if arguments
            .iter()
            .any(|argument| argument.chars().any(char::is_control))
        {
            return None;
        }
        ButtonbarAction::Program { path, arguments }
    } else {
        let command = if entry.parameters.is_empty() {
            entry.command.clone()
        } else {
            format!("{} {}", entry.command, entry.parameters)
        };
        if !valid_button_command(&command) || command.chars().any(char::is_control) {
            return None;
        }
        ButtonbarAction::Internal { command }
    };
    let label = if entry.label.is_empty() {
        entry.command
    } else {
        entry.label
    };
    let label: String = label
        .chars()
        .filter(|character| !character.is_control())
        .take(80)
        .collect();
    if label.trim().is_empty() {
        return None;
    }
    let icon = entry.icon.trim_matches('"');
    let icon = if icon_image(icon).is_some() {
        icon.into()
    } else {
        "datei".into()
    };
    Some(ButtonbarButton {
        label,
        icon,
        action,
    })
}

fn export_tc_buttonbar(buttons: &[ButtonbarButton]) -> io::Result<String> {
    if buttons.is_empty() {
        return Err(io::Error::other(
            "Die Leiste enthält keine eigenen Schaltflächen.",
        ));
    }
    let mut output = format!("[Buttonbar]\r\nButtoncount={}\r\n", buttons.len());
    for (index, button) in buttons.iter().enumerate() {
        let (command, parameters) = match &button.action {
            ButtonbarAction::Program { path, arguments } => (
                format!("\"{}\"", display_path(path)),
                arguments
                    .iter()
                    .map(|argument| quote_argument(argument))
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
            ButtonbarAction::Internal { command } => {
                let (verb, parameters) = command
                    .split_once(char::is_whitespace)
                    .unwrap_or((command, ""));
                let verb = match verb {
                    "copy" => "cm_Copy",
                    "move" => "cm_RenMov",
                    "mkdir" => "cm_MkDir",
                    "delete" => "cm_Delete",
                    "view" => "cm_List",
                    "edit" => "cm_Edit",
                    "search" => "cm_SearchFor",
                    "refresh" => "cm_RereadSource",
                    "cd" => verb,
                    _ if verb.to_ascii_lowercase().starts_with("cm_")
                        || verb.chars().all(|c| c.is_ascii_digit()) =>
                    {
                        verb
                    }
                    _ => {
                        return Err(io::Error::other(format!(
                            "Für „{}“ fehlt ein TC-Befehl.",
                            button.label
                        )));
                    }
                };
                if verb == "cd" {
                    (format!("{verb} {parameters}"), String::new())
                } else {
                    (verb.into(), parameters.into())
                }
            }
        };
        if [&button.label, &command, &parameters, &button.icon]
            .iter()
            .any(|value| value.contains(['\r', '\n', '\0', '%']))
        {
            return Err(io::Error::other(
                "Die Schaltfläche enthält TC-Variablen oder ungültige Zeichen.",
            ));
        }
        let index = index + 1;
        let icon = if Path::new(&button.icon).is_absolute() {
            &button.icon
        } else {
            ""
        };
        write!(output, "button{index}={icon}\r\ncmd{index}={command}\r\nparam{index}={parameters}\r\nmenu{index}={}\r\n", button.label).unwrap();
    }
    Ok(output)
}

fn quote_argument(argument: &str) -> String {
    let mut quoted = String::from("\"");
    let mut backslashes = 0;
    for character in argument.chars() {
        if character == '\\' {
            backslashes += 1;
            continue;
        }
        let count = if character == '"' {
            backslashes * 2 + 1
        } else {
            backslashes
        };
        quoted.extend(std::iter::repeat_n('\\', count));
        quoted.push(character);
        backslashes = 0;
    }
    quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
    quoted.push('"');
    quoted
}

fn icon_image(value: &str) -> Option<image::RgbaImage> {
    let path = Path::new(value);
    if !path.is_absolute() {
        return None;
    }
    let metadata = arco_core::files::regular_metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > 1024 * 1024 {
        return None;
    }
    let icon = crate::preview::decode_image(path, 32, 32).ok()?;
    arco_core::files::ensure_unchanged(path, &metadata).ok()?;
    Some(icon)
}

fn parse_button(value: &str) -> io::Result<ButtonbarButton> {
    if value.len() > 4096 {
        return Err(io::Error::other("Die Schaltfläche enthält zu viel Text."));
    }
    let mut lines = value.lines();
    let label = lines.next().unwrap_or_default().trim();
    let icon = lines.next().unwrap_or_default().trim().trim_matches('"');
    let command = lines.next().unwrap_or_default().trim();
    if label.is_empty() || label.chars().count() > 80 || label.chars().any(char::is_control) {
        return Err(io::Error::other(
            "Gib eine Beschriftung mit höchstens 80 Zeichen ein.",
        ));
    }
    let built_in = icon.to_ascii_lowercase();
    let icon = if ICONS.contains(&built_in.as_str()) {
        built_in
    } else if icon_image(icon).is_some() {
        icon.to_owned()
    } else {
        return Err(io::Error::other(
            "Wähle einen Symbolnamen oder eine lesbare Bilddatei bis 1 MiB.",
        ));
    };
    let arguments: Vec<_> = lines.map(str::to_owned).collect();
    if command.chars().any(char::is_control)
        || arguments
            .iter()
            .any(|argument| argument.chars().any(char::is_control))
    {
        return Err(io::Error::other(
            "Der Befehl oder ein Argument enthält Steuerzeichen.",
        ));
    }
    let path = PathBuf::from(command);
    let action = if program_path(&path) {
        ButtonbarAction::Program { path, arguments }
    } else if arguments.is_empty() && valid_button_command(command) {
        ButtonbarAction::Internal {
            command: command.to_owned(),
        }
    } else {
        return Err(io::Error::other(
            "Gib einen Arco-Befehl oder einen absoluten EXE-Pfad ein.",
        ));
    };
    Ok(ButtonbarButton {
        label: label.to_owned(),
        icon,
        action,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn tc_buttonbar_round_trip_keeps_command_parameters_and_windows_quoting() {
        let imported: Vec<_> = profile::parse_tc_buttonbar(concat!(
            "[Buttonbar]\nBUTTONCOUNT=3\n",
            "cmd1=cm_CompareDirs\nparam1=2\nmenu1=Größen prüfen\n",
            "cmd2=\"C:\\Program Files\\Tool.exe\"\nparam2=\"mit Leerzeichen\" \"\"\nmenu2=Tool\n",
            "cmd3=cm_Copy\npath3=C:\\Ziel\n",
        ))
        .unwrap()
        .into_iter()
        .filter_map(import_tc_button)
        .collect();
        assert_eq!(imported.len(), 2);
        assert_eq!(imported[0].label, "Größen prüfen");
        assert_eq!(
            imported[1].action,
            ButtonbarAction::Program {
                path: PathBuf::from(r"C:\Program Files\Tool.exe"),
                arguments: vec!["mit Leerzeichen".into(), "".into()],
            }
        );
        let output = export_tc_buttonbar(&imported).unwrap();
        let round_trip: Vec<_> = profile::parse_tc_buttonbar(&output)
            .unwrap()
            .into_iter()
            .filter_map(import_tc_button)
            .collect();
        assert_eq!(round_trip, imported);
        let special = [r#"a\"b"#, r"C:\Ende\", "Über !"];
        let args = special
            .iter()
            .map(|argument| quote_argument(argument))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(
            arco_windows::parse_windows_arguments(&args).unwrap(),
            special
        );
    }

    #[test]
    fn buttonbar_parses_internal_parameters_and_separate_program_arguments() {
        let button = parse_button("Abgleichen\nsuche\ncm_CompareDirs 2").unwrap();
        assert_eq!(
            button.action,
            ButtonbarAction::Internal {
                command: "cm_CompareDirs 2".into()
            }
        );
        let executable = std::env::temp_dir().join("Program Folder").join("test.exe");
        let value = format!(
            "Programm\nterminal\n{}\n--file\nA B.txt",
            executable.display()
        );
        let parsed = parse_button(&value).unwrap();
        assert_eq!(
            parsed.action,
            ButtonbarAction::Program {
                path: executable,
                arguments: vec!["--file".into(), "A B.txt".into()],
            }
        );
        assert_eq!(parse_button(&button_text(&parsed)).unwrap(), parsed);
        assert!(parse_button("Programm\nterminal\ntest.exe").is_err());
    }

    #[test]
    fn custom_button_icon_preserves_path_and_color() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("Eigene Grafik.png");
        image::RgbaImage::from_pixel(64, 32, image::Rgba([1, 22, 45, 255]))
            .save(&path)
            .unwrap();
        let button = parse_button(&format!("Öffnen\n{}\nsearch", path.display())).unwrap();
        assert_eq!(Path::new(&button.icon), path);
        let icon = icon_image(&button.icon).unwrap();
        assert_eq!(icon.dimensions(), (32, 16));
        assert_eq!(icon.get_pixel(0, 0).0, [1, 22, 45, 255]);
        assert!(parse_button("Öffnen\nrelative.png\nsearch").is_err());
    }
}
