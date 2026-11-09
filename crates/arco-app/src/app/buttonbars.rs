use super::App;
use crate::commands::{Dialog, Pending, valid_button_command};
use crate::{ButtonbarRow, display_path};
use arco_core::profile::{
    self, ButtonbarAction, ButtonbarButton, ButtonbarTemplate, ProgramWindow, TcButtonbarEntry,
};
use slint::{ModelRc, VecModel};
use std::fmt::Write as _;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

mod tc_arguments;
use tc_arguments::TcArguments;

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
const MAX_MENU_DEPTH: usize = 8;
const MAX_BAR_BUTTONS: usize = 512;
const MENU_COMMAND: &str = "Untermenü";
const SWITCH_COMMAND: &str = "Leistenwechsel";

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
            self.buttonbar_edit_path.clear();
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
        self.buttonbar_edit_path.clear();
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
        let buttons = self.editing_buttons(bar);
        let choices: Vec<_> = [
            "Neue Schaltfläche",
            "TC-Buttonbar importieren",
            "TC-Buttonbar exportieren",
        ]
        .into_iter()
        .chain(buttons.iter().map(|button| button.label.as_str()))
        .chain((!self.buttonbar_edit_path.is_empty()).then_some("Zur übergeordneten Leiste"))
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
        if !self.buttonbar_edit_path.is_empty() && option == 3 + self.editing_buttons(bar).len() {
            self.buttonbar_edit_path.pop();
            self.buttonbar_list(bar);
        } else if matches!(option, 1 | 2) {
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
            let Some(button) = self.editing_buttons(bar).get(index) else {
                return;
            };
            let description = format!("Schaltfläche „{}“", button.label);
            let mut options = vec![
                "Bearbeiten",
                "Nach oben verschieben",
                "Nach unten verschieben",
                "Entfernen",
            ];
            if child_buttons(&button.action).is_some() {
                options.push("Einträge im Untermenü bearbeiten");
            }
            self.dialog(
                "confirm",
                Pending::ButtonbarAction(bar, index),
                Dialog {
                    title: BARS[bar],
                    description: &description,
                    options: &options,
                    ..Default::default()
                },
            );
        } else if self.editing_buttons(bar).len() < self.editing_limit(bar) {
            self.buttonbar_editor(bar, None);
        } else {
            self.text(
                BARS[bar],
                &format!(
                    "Diese Leiste hat Platz für {} eigene Schaltflächen.",
                    self.editing_limit(bar)
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
            export_bar_file(&path, self.editing_buttons(bar))?;
            self.status("TC-Buttonbar exportiert.");
            return Ok(());
        }
        let mut importer = BarImport::default();
        let buttons = importer.read(&path, &mut Vec::new(), self.editing_limit(bar))?;
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
            "{} Schaltflächen übernehmen und die Leiste ersetzen? {} Einträge wurden ausgelassen.",
            buttons.len(),
            importer.omitted
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
        let mut buttons = self.editing_buttons(bar).to_vec();
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
            4 if child_buttons(&buttons[index].action).is_some() => {
                self.buttonbar_edit_path.push(index);
                self.buttonbar_list(bar);
                return Ok(());
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
        let button = index.and_then(|index| self.editing_buttons(bar).get(index));
        let initial = button.map_or_else(|| "Dateien suchen\nsuche\nsearch".into(), button_text);
        let working = button
            .and_then(|button| match &button.action {
                ButtonbarAction::Program {
                    working_directory, ..
                } => working_directory.as_deref(),
                _ => None,
            })
            .map(display_path)
            .unwrap_or_default();
        self.show_button_editor(bar, index, &initial, &working, None);
    }

    fn show_button_editor(
        &mut self,
        bar: usize,
        index: Option<usize>,
        value: &str,
        working_directory: &str,
        error: Option<&str>,
    ) {
        let mut description = format!(
            "Je eine Zeile für Beschriftung, Symbol und Befehl oder EXE-Pfad. „Untermenü“ oder „Leistenwechsel“ legt eine verschachtelte Leiste an. Nach einem EXE-Pfad folgt je Argument eine Zeile. Das Präfix tc-ask: vor dem EXE-Pfad öffnet die Argumente zur Prüfung. Für das Symbol gehen {} sowie Bilddateien oder Ressourcen wie C:\\Windows\\System32\\shell32.dll,3.",
            ICONS.join(", ")
        );
        if let Some(error) = error {
            description = format!("{error}\n{description}");
        }
        self.dialog(
            "buttonbar-edit",
            Pending::ButtonbarEdit(bar, index),
            Dialog {
                title: BARS[bar],
                description: &description,
                value,
                second: working_directory,
                second_placeholder: "Startordner für EXE (optional)",
                ..Default::default()
            },
        );
    }

    pub(crate) fn save_button(
        &mut self,
        bar: usize,
        index: Option<usize>,
        value: &str,
        working_directory: &str,
    ) -> io::Result<()> {
        let mut button = match parse_button(value, working_directory) {
            Ok(button) => button,
            Err(error) => {
                self.show_button_editor(
                    bar,
                    index,
                    value,
                    working_directory,
                    Some(&error.to_string()),
                );
                return Ok(());
            }
        };
        let mut buttons = self.editing_buttons(bar).to_vec();
        if let Some(index) = index {
            let current = buttons
                .get_mut(index)
                .ok_or_else(|| io::Error::other("Die Schaltfläche fehlt."))?;
            match (&mut button.action, &mut current.action) {
                (ButtonbarAction::Menu { buttons }, ButtonbarAction::Menu { buttons: old }) => {
                    std::mem::swap(buttons, old);
                }
                (
                    new @ ButtonbarAction::SwitchBar { .. },
                    old @ ButtonbarAction::SwitchBar { .. },
                ) => {
                    std::mem::swap(new, old);
                }
                _ => {}
            }
            *current = button;
        } else if buttons.len() < self.editing_limit(bar) {
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
        let root = if bar == 2 {
            &mut draft.starter
        } else {
            if !buttons.is_empty() {
                if bar == 0 {
                    draft.layout.toolbar = true;
                } else {
                    draft.layout.panel_toolbar = true;
                }
            }
            &mut draft.buttonbars[bar]
        };
        *edit_level_mut(root, &self.buttonbar_edit_path)
            .ok_or_else(|| io::Error::other("Das Untermenü fehlt."))? = buttons;
        validate_tree(root)?;
        self.commit_buttonbar_profile(draft)
    }

    fn editing_buttons(&self, bar: usize) -> &[ButtonbarButton] {
        let mut buttons = self.buttons(bar);
        for &index in &self.buttonbar_edit_path {
            let Some(children) = buttons
                .get(index)
                .and_then(|button| child_buttons(&button.action))
            else {
                return &[];
            };
            buttons = children;
        }
        buttons
    }

    fn editing_limit(&self, bar: usize) -> usize {
        if self.buttonbar_edit_path.is_empty() {
            LIMITS[bar]
        } else {
            LIMITS[2]
        }
    }

    fn commit_buttonbar_profile(&mut self, draft: arco_core::profile::Profile) -> io::Result<()> {
        if !self.save_profile {
            return Err(io::Error::other(
                "Das Arco-Profil konnte beim Start nicht geladen werden.",
            ));
        }
        draft.save(&self.profile_path)?;
        self.profile = draft;
        self.buttonbar_switches.iter_mut().for_each(Vec::clear);
        self.sync_ui_settings();
        Ok(())
    }

    pub(super) fn publish_buttonbars(&self) {
        let rows = |bar: usize| {
            let active = self.buttonbar_switches[bar].last();
            let back = active.map(|_| ButtonbarRow {
                label: "Zur vorherigen Leiste".into(),
                icon: "ordner".into(),
                custom_icon: slint::Image::default(),
            });
            let rows: Vec<_> = back
                .into_iter()
                .chain(
                    active
                        .map_or(self.buttons(bar), Vec::as_slice)
                        .iter()
                        .take(if active.is_some() && bar == 0 {
                            LIMITS[2]
                        } else {
                            LIMITS[bar]
                        })
                        .map(|button| ButtonbarRow {
                            label: button.label.as_str().into(),
                            icon: button.icon.as_str().into(),
                            custom_icon: icon_image(&button.icon)
                                .map_or_else(slint::Image::default, |image| {
                                    crate::preview::to_slint(&image)
                                }),
                        }),
                )
                .collect();
            ModelRc::new(VecModel::from(rows))
        };
        self.ui().set_top_buttons(rows(0));
        self.ui().set_middle_buttons(rows(1));
    }

    pub(crate) fn run_button(&mut self, bar: usize, index: usize) {
        let selected = match self
            .buttonbar_switches
            .get_mut(bar)
            .and_then(|stack| stack.last_mut())
        {
            Some(_) if index == 0 => {
                self.buttonbar_switches[bar].pop();
                self.sync_ui_settings();
                return;
            }
            Some(buttons) => buttons.get(index - 1),
            None => self.buttons(bar).get(index),
        };
        let Some(button) = selected.cloned() else {
            return;
        };
        self.run_button_in_menu(bar, button, Vec::new());
    }

    fn run_button_in_menu(
        &mut self,
        source_bar: usize,
        button: ButtonbarButton,
        mut levels: Vec<(String, Vec<ButtonbarButton>)>,
    ) {
        match button.action {
            ButtonbarAction::Internal { command } => {
                self.command_line(&command);
            }
            ButtonbarAction::Program {
                path,
                arguments,
                working_directory,
                window,
                tc_arguments,
                prompt_arguments,
            } => {
                let result = self.prepare_button_program(
                    path,
                    arguments,
                    working_directory,
                    window,
                    tc_arguments,
                    prompt_arguments,
                );
                self.report(result);
            }
            ButtonbarAction::Menu { buttons } => {
                levels.push((button.label, buttons));
                self.show_buttonbar_menu(source_bar, levels);
            }
            ButtonbarAction::SwitchBar { bar, buttons } => {
                let bar = bar.unwrap_or(source_bar.min(1));
                let Some(stack) = self.buttonbar_switches.get_mut(bar) else {
                    return;
                };
                if let Some(buttons) = buttons {
                    if stack.len() == MAX_MENU_DEPTH {
                        self.status("Die maximale Zahl verschachtelter Leisten ist erreicht.");
                        return;
                    }
                    stack.push(buttons);
                } else {
                    stack.clear();
                }
                self.sync_ui_settings();
            }
        }
    }

    fn prepare_button_program(
        &mut self,
        path: PathBuf,
        arguments: Vec<String>,
        working: Option<PathBuf>,
        window: ProgramWindow,
        tc_arguments: bool,
        prompt_arguments: bool,
    ) -> io::Result<()> {
        if !program_path(&path) {
            return Err(io::Error::other(
                "Die Schaltfläche benötigt einen absoluten EXE-Pfad.",
            ));
        }
        let source = self.current();
        let working = match working {
            Some(path) if path.is_absolute() => Some(path),
            Some(path) if source.is_local() => Some(source.path.join(path)),
            Some(_) => {
                return Err(io::Error::other(
                    "Ein relativer Startordner benötigt einen lokalen Quellordner.",
                ));
            }
            None => source.is_local().then(|| source.path.clone()),
        };
        let (arguments, list_file) = if tc_arguments {
            TcArguments::parse(&arguments)?.expand(
                self.panels.each_ref().map(|panel| panel.current()),
                self.active,
            )?
        } else {
            (arguments, None)
        };
        if prompt_arguments {
            let value = join_windows_arguments(arguments.iter().map(String::as_str));
            let description = format!(
                "{}\n{}",
                display_path(&path),
                crate::locale::tr(
                    "Bearbeite die Windows-Argumente und starte das Programm mit Ausführen."
                )
            );
            self.dialog(
                "input",
                Pending::ButtonbarProgram {
                    path,
                    working,
                    window,
                    list_file,
                },
                Dialog {
                    title: "Programmargumente prüfen",
                    description: &description,
                    value: &value,
                    ..Default::default()
                },
            );
            return Ok(());
        }
        Self::start_button_program(&path, arguments, working, window, list_file)
    }

    pub(crate) fn start_button_program(
        path: &Path,
        arguments: Vec<String>,
        working: Option<PathBuf>,
        window: ProgramWindow,
        list_file: Option<tempfile::TempPath>,
    ) -> io::Result<()> {
        let waiter = list_file.map(keep_list_until_exit).transpose()?;
        let running = if window == ProgramWindow::Normal {
            let mut program = std::process::Command::new(path);
            program.args(arguments);
            if let Some(working) = working {
                program.current_dir(working);
            }
            RunningButton::Process(program.spawn()?)
        } else {
            let parameters = join_windows_arguments(arguments.iter().map(String::as_str));
            RunningButton::Shell(arco_windows::launch_with_window(
                path,
                &parameters,
                working.as_deref(),
                window == ProgramWindow::Maximized,
            )?)
        };
        if let Some(waiter) = waiter {
            let _ = waiter.send(running);
        }
        Ok(())
    }

    fn show_buttonbar_menu(&mut self, bar: usize, levels: Vec<(String, Vec<ButtonbarButton>)>) {
        let Some((label, buttons)) = levels.last() else {
            return;
        };
        let title = label.clone();
        let names: Vec<_> = buttons
            .iter()
            .map(|button| match button.action {
                ButtonbarAction::Menu { .. } => format!("{}  ›", button.label),
                _ => button.label.clone(),
            })
            .collect();
        let options: Vec<_> = names
            .iter()
            .map(String::as_str)
            .chain((levels.len() > 1).then_some("‹ Zurück"))
            .collect();
        if options.is_empty() {
            self.text(&title, "Dieses Untermenü ist leer.");
        } else {
            self.dialog(
                "confirm",
                Pending::ButtonbarMenu(bar, levels),
                Dialog {
                    title: &title,
                    options: &options,
                    ..Default::default()
                },
            );
        }
    }

    pub(crate) fn select_buttonbar_menu(
        &mut self,
        bar: usize,
        mut levels: Vec<(String, Vec<ButtonbarButton>)>,
        option: usize,
    ) {
        if let Some(button) = levels
            .last()
            .and_then(|(_, buttons)| buttons.get(option))
            .cloned()
        {
            self.run_button_in_menu(bar, button, levels);
        } else {
            levels.pop();
            self.show_buttonbar_menu(bar, levels);
        }
    }
}

fn edit_level_mut<'a>(
    buttons: &'a mut Vec<ButtonbarButton>,
    path: &[usize],
) -> Option<&'a mut Vec<ButtonbarButton>> {
    let Some((&index, remaining)) = path.split_first() else {
        return Some(buttons);
    };
    match &mut buttons.get_mut(index)?.action {
        ButtonbarAction::Menu { buttons }
        | ButtonbarAction::SwitchBar {
            buttons: Some(buttons),
            ..
        } => edit_level_mut(buttons, remaining),
        _ => None,
    }
}

fn child_buttons(action: &ButtonbarAction) -> Option<&[ButtonbarButton]> {
    match action {
        ButtonbarAction::Menu { buttons }
        | ButtonbarAction::SwitchBar {
            buttons: Some(buttons),
            ..
        } => Some(buttons),
        _ => None,
    }
}

fn button_text(button: &ButtonbarButton) -> String {
    let mut text = format!("{}\n{}\n", button.label, button.icon);
    match &button.action {
        ButtonbarAction::Internal { command } => text.push_str(command),
        ButtonbarAction::Menu { .. } => text.push_str(MENU_COMMAND),
        ButtonbarAction::SwitchBar { .. } => text.push_str(SWITCH_COMMAND),
        ButtonbarAction::Program {
            path,
            arguments,
            window,
            tc_arguments,
            prompt_arguments,
            ..
        } => {
            text.push_str(match (window, tc_arguments, prompt_arguments) {
                (ProgramWindow::Normal, _, true) => "tc-ask:",
                (ProgramWindow::Minimized, _, true) => "tc-min-ask:",
                (ProgramWindow::Maximized, _, true) => "tc-max-ask:",
                (ProgramWindow::Normal, false, false) => "",
                (ProgramWindow::Normal, true, false) => "tc:",
                (ProgramWindow::Minimized, false, false) => "min:",
                (ProgramWindow::Maximized, false, false) => "max:",
                (ProgramWindow::Minimized, true, false) => "tc-min:",
                (ProgramWindow::Maximized, true, false) => "tc-max:",
            });
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
    if entry.command.is_empty() || entry.command.contains('%') {
        return None;
    }
    let path = PathBuf::from(entry.command.trim_matches('"'));
    let action = if program_path(&path) {
        let prompt = entry.parameters.trim_start().strip_prefix('?');
        let parameters = prompt.unwrap_or(&entry.parameters).trim_start();
        let arguments = arco_windows::parse_windows_arguments(parameters).ok()?;
        if arguments
            .iter()
            .any(|argument| argument.chars().any(char::is_control))
        {
            return None;
        }
        let tc_arguments = prompt.is_some() || parameters.contains('%');
        if tc_arguments {
            TcArguments::parse(&arguments).ok()?;
        }
        let window = match entry.iconic.as_str() {
            "" | "0" => ProgramWindow::Normal,
            "1" => ProgramWindow::Minimized,
            "-1" => ProgramWindow::Maximized,
            _ => return None,
        };
        ButtonbarAction::Program {
            path,
            arguments,
            working_directory: start_directory(&entry.working_directory).ok()?,
            window,
            tc_arguments,
            prompt_arguments: prompt.is_some(),
        }
    } else {
        if !entry.working_directory.is_empty()
            || entry.parameters.contains('%')
            || entry.parameters.trim_start().starts_with('?')
        {
            return None;
        }
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
    button_from_tc(entry, action, "datei")
}

fn button_from_tc(
    entry: TcButtonbarEntry,
    action: ButtonbarAction,
    default_icon: &str,
) -> Option<ButtonbarButton> {
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
        default_icon.into()
    };
    Some(ButtonbarButton {
        label,
        icon,
        action,
    })
}

#[derive(Default)]
struct BarImport {
    visited: usize,
    omitted: usize,
}

impl BarImport {
    fn read(
        &mut self,
        path: &Path,
        ancestors: &mut Vec<PathBuf>,
        limit: usize,
    ) -> io::Result<Vec<ButtonbarButton>> {
        let canonical = path.canonicalize()?;
        if ancestors.len() > MAX_MENU_DEPTH || ancestors.contains(&canonical) {
            return Err(io::Error::other(
                "Zyklische oder zu tief verschachtelte Buttonbar.",
            ));
        }
        let entries = profile::parse_tc_buttonbar(&profile::read_tc_text(path)?)?;
        ancestors.push(canonical);
        let mut buttons = Vec::new();
        for mut entry in entries {
            if buttons.len() >= limit || self.visited >= MAX_BAR_BUTTONS {
                self.omitted += 1;
                continue;
            }
            self.visited += 1;
            let resolve = super::favorites::tc_path_variables(path);
            entry.icon = resolve_tc_icon(&entry.icon, path, &resolve);
            let button = if let Some((reference, display)) = bar_reference(&entry) {
                let mut related = || {
                    profile::tc_related_file(path, reference, &["bar"], &|name| {
                        resolve(
                            name.strip_prefix('|')
                                .and_then(|name| name.strip_suffix('|'))
                                .unwrap_or(name),
                        )
                    })
                    .and_then(|related| self.read(&related, ancestors, LIMITS[2]))
                };
                let action = match display {
                    BarDisplay::Menu => related()
                        .ok()
                        .map(|buttons| ButtonbarAction::Menu { buttons }),
                    BarDisplay::Switch(bar) if reference == "*" => {
                        Some(ButtonbarAction::SwitchBar { bar, buttons: None })
                    }
                    BarDisplay::Switch(bar) => {
                        related().ok().map(|buttons| ButtonbarAction::SwitchBar {
                            bar,
                            buttons: Some(buttons),
                        })
                    }
                };
                action.and_then(|action| button_from_tc(entry, action, "ordner"))
            } else {
                import_tc_button_at(entry, path)
            };
            if let Some(button) = button {
                buttons.push(button);
            } else {
                self.omitted += 1;
            }
        }
        ancestors.pop();
        Ok(buttons)
    }
}

fn import_tc_button_at(mut entry: TcButtonbarEntry, path: &Path) -> Option<ButtonbarButton> {
    if entry.command.contains('%') || entry.working_directory.contains('%') {
        let resolve = super::favorites::tc_path_variables(path);
        let command = profile::expand_tc_path(&entry.command, &resolve)?
            .into_string()
            .ok()?;
        if !program_path(Path::new(&command)) {
            return None;
        }
        entry.command = command;
        entry.working_directory = profile::expand_tc_path(&entry.working_directory, &resolve)?
            .into_string()
            .ok()?;
    }
    import_tc_button(entry)
}

enum BarDisplay {
    Menu,
    Switch(Option<usize>),
}

fn bar_reference(entry: &TcButtonbarEntry) -> Option<(&str, BarDisplay)> {
    let command = entry.command.trim();
    let (verb, inline) = command
        .split_once(char::is_whitespace)
        .unwrap_or((command, ""));
    let display = match verb.to_ascii_uppercase().as_str() {
        "OPENBARMENU" => Some(BarDisplay::Menu),
        "OPENBAR" => Some(BarDisplay::Switch(None)),
        "OPENBAR1" => Some(BarDisplay::Switch(Some(0))),
        "OPENBAR2" => Some(BarDisplay::Switch(Some(1))),
        _ => None,
    };
    if let Some(display) = display {
        return Some((
            if inline.is_empty() {
                &entry.parameters
            } else {
                inline.trim()
            },
            display,
        ));
    }
    let display = if entry.iconic == "1" {
        BarDisplay::Menu
    } else {
        BarDisplay::Switch(None)
    };
    Path::new(command.trim_matches('"'))
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("bar"))
        .then_some((command, display))
}

fn validate_tree(buttons: &[ButtonbarButton]) -> io::Result<()> {
    fn visit(buttons: &[ButtonbarButton], depth: usize, left: &mut usize) -> io::Result<()> {
        *left = left
            .checked_sub(buttons.len())
            .filter(|_| depth <= MAX_MENU_DEPTH)
            .ok_or_else(|| {
                io::Error::other("Die Buttonbar hat zu viele Einträge oder Untermenüebenen.")
            })?;
        for button in buttons {
            if let Some(buttons) = child_buttons(&button.action) {
                visit(buttons, depth + 1, left)?;
            }
        }
        Ok(())
    }
    let mut left = MAX_BAR_BUTTONS;
    visit(buttons, 0, &mut left)
}

fn export_bar_file(path: &Path, buttons: &[ButtonbarButton]) -> io::Result<()> {
    if buttons.is_empty() {
        return Err(io::Error::other(
            "Die Leiste enthält keine eigenen Schaltflächen.",
        ));
    }
    validate_tree(buttons)?;
    let parent = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
    let mut children = None;
    let mut counter = 0;
    let data = export_tc_buttonbar(buttons, &mut |buttons| {
        if children.is_none() {
            children = Some(
                tempfile::Builder::new()
                    .prefix("Arco-Untermenues-")
                    .tempdir_in(parent)?,
            );
        }
        write_child_bar(children.as_ref().unwrap().path(), &mut counter, buttons)
    })?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    write_bar(file.as_file_mut(), &data)?;
    file.persist_noclobber(path).map_err(|error| error.error)?;
    if let Some(children) = children {
        let _ = children.keep();
    }
    Ok(())
}

fn write_child_bar(
    directory: &Path,
    counter: &mut usize,
    buttons: &[ButtonbarButton],
) -> io::Result<PathBuf> {
    *counter += 1;
    let path = directory.join(format!("Leiste-{counter}.bar"));
    let data = export_tc_buttonbar(buttons, &mut |buttons| {
        write_child_bar(directory, counter, buttons)
    })?;
    let mut file = std::fs::File::create_new(&path)?;
    write_bar(&mut file, &data)?;
    Ok(path)
}

fn write_bar(file: &mut std::fs::File, data: &str) -> io::Result<()> {
    file.write_all(&[0xff, 0xfe])?;
    let encoded: Vec<_> = data.encode_utf16().flat_map(u16::to_le_bytes).collect();
    file.write_all(&encoded)?;
    file.sync_all()
}

fn export_tc_buttonbar(
    buttons: &[ButtonbarButton],
    child_bar: &mut dyn FnMut(&[ButtonbarButton]) -> io::Result<PathBuf>,
) -> io::Result<String> {
    let mut output = format!("[Buttonbar]\r\nButtoncount={}\r\n", buttons.len());
    for (index, button) in buttons.iter().enumerate() {
        let (command, parameters, working_directory) = match &button.action {
            ButtonbarAction::Menu { buttons } => (
                "OPENBARMENU".into(),
                quote_argument(&display_path(&child_bar(buttons)?)),
                String::new(),
            ),
            ButtonbarAction::SwitchBar { bar, buttons } => {
                let command = match bar {
                    Some(0) => "OPENBAR1",
                    Some(1) => "OPENBAR2",
                    None => "OPENBAR",
                    _ => return Err(io::Error::other("Unbekannte Zielleiste.")),
                };
                let target = match buttons {
                    Some(buttons) => quote_argument(&display_path(&child_bar(buttons)?)),
                    None => "*".into(),
                };
                (command.into(), target, String::new())
            }
            ButtonbarAction::Program {
                path,
                arguments,
                working_directory,
                tc_arguments,
                prompt_arguments,
                ..
            } => (
                format!("\"{}\"", display_path(path)),
                export_program_arguments(arguments, *tc_arguments, *prompt_arguments),
                working_directory
                    .as_deref()
                    .map(display_path)
                    .unwrap_or_default(),
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
                    "search-separate" => "cm_SearchStandalone",
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
                    (format!("{verb} {parameters}"), String::new(), String::new())
                } else {
                    (verb.into(), parameters.into(), String::new())
                }
            }
        };
        if [&button.label, &command, &button.icon, &working_directory]
            .iter()
            .any(|value| value.contains(['\r', '\n', '\0', '%']))
            || parameters.contains(['\r', '\n', '\0'])
            || (parameters.contains('%')
                && !matches!(button.action, ButtonbarAction::Program { .. }))
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
        if !working_directory.is_empty() {
            writeln!(output, "path{index}={working_directory}\r").unwrap();
        }
        if let ButtonbarAction::Program { window, .. } = button.action {
            let value = match window {
                ProgramWindow::Normal => 0,
                ProgramWindow::Minimized => 1,
                ProgramWindow::Maximized => -1,
            };
            if value != 0 {
                writeln!(output, "iconic{index}={value}\r").unwrap();
            }
        }
    }
    Ok(output)
}

fn export_program_arguments(arguments: &[String], tc_arguments: bool, prompt: bool) -> String {
    let mut parameters = if tc_arguments {
        join_windows_arguments(arguments.iter().map(String::as_str))
    } else {
        let escaped: Vec<_> = arguments
            .iter()
            .map(|value| value.replace('%', "%%"))
            .collect();
        join_windows_arguments(escaped.iter().map(String::as_str))
    };
    if prompt {
        parameters.insert(0, '?');
    }
    parameters
}

fn join_windows_arguments<'a>(arguments: impl Iterator<Item = &'a str>) -> String {
    arguments.map(quote_argument).collect::<Vec<_>>().join(" ")
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

#[derive(Clone, Copy)]
enum ListEncoding {
    Ansi,
    Oem,
    Utf8,
    Utf16Le,
}

#[derive(Clone, Copy)]
struct ListArgument {
    full_path: bool,
    short_path: bool,
    encoding: ListEncoding,
}

impl ListArgument {
    fn parse(token: &str) -> Option<Self> {
        let encoding = match token {
            "%L" | "%F" | "%l" | "%f" => ListEncoding::Ansi,
            "%D" | "%d" => ListEncoding::Oem,
            "%UL" | "%UF" => ListEncoding::Utf8,
            "%WL" | "%WF" => ListEncoding::Utf16Le,
            _ => return None,
        };
        Some(Self {
            full_path: token.ends_with(['L', 'l', 'D', 'd']),
            short_path: matches!(token, "%l" | "%f" | "%D" | "%d"),
            encoding,
        })
    }

    fn write(self, entries: Vec<PathBuf>) -> io::Result<tempfile::TempPath> {
        let mut text = String::new();
        for path in entries {
            let path = if self.short_path {
                arco_windows::short_path(&path)?
            } else {
                path
            };
            let name = if self.full_path {
                if path.to_str().is_none() {
                    return Err(io::Error::other(
                        "Ein ausgewählter Pfad ist nicht in Unicode darstellbar.",
                    ));
                }
                display_path(&path)
            } else {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| {
                        io::Error::other(
                            "Ein ausgewählter Dateiname ist nicht in Unicode darstellbar.",
                        )
                    })?
                    .to_owned()
            };
            if name.contains(['\r', '\n']) {
                return Err(io::Error::other(
                    "Eine TC-Listendatei unterstützt keine Zeilenumbrüche im Namen.",
                ));
            }
            text.push_str(&name);
            text.push_str("\r\n");
        }
        let bytes = match self.encoding {
            ListEncoding::Ansi => arco_windows::encode_system_text(&text, false)?,
            ListEncoding::Oem => arco_windows::encode_system_text(&text, true)?,
            ListEncoding::Utf8 => [b"\xef\xbb\xbf", text.as_bytes()].concat(),
            ListEncoding::Utf16Le => [0xff, 0xfe]
                .into_iter()
                .chain(text.encode_utf16().flat_map(u16::to_le_bytes))
                .collect(),
        };
        let mut file = tempfile::Builder::new()
            .prefix("arco-selection-")
            .suffix(".lst")
            .tempfile()?;
        file.write_all(&bytes)?;
        Ok(file.into_temp_path())
    }
}

enum RunningButton {
    Process(std::process::Child),
    Shell(Option<arco_windows::ShellProgram>),
}

fn keep_list_until_exit(
    file: tempfile::TempPath,
) -> io::Result<std::sync::mpsc::Sender<RunningButton>> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("arco-button-list".into())
        .spawn(move || {
            match receiver.recv() {
                Ok(RunningButton::Process(mut process)) => {
                    let _ = process.wait();
                }
                Ok(RunningButton::Shell(Some(process))) => {
                    let _ = process.wait();
                }
                Ok(RunningButton::Shell(None)) => {
                    // A shell handoff can reuse an existing process without a waitable handle.
                    std::thread::sleep(std::time::Duration::from_secs(120));
                }
                Err(_) => (),
            }
            drop(file);
        })?;
    Ok(sender)
}

fn icon_image(value: &str) -> Option<image::RgbaImage> {
    if let Some((path, index)) = resource_icon(value) {
        if !path.is_absolute() {
            return None;
        }
        let metadata = arco_core::files::regular_metadata(path).ok()?;
        if !metadata.is_file() {
            return None;
        }
        let rgba = arco_windows::resource_icon(path, index).ok()?;
        arco_core::files::ensure_unchanged(path, &metadata).ok()?;
        return image::RgbaImage::from_raw(32, 32, rgba);
    }
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

fn resource_icon(value: &str) -> Option<(&Path, i32)> {
    let (path, index) = value.rsplit_once(',').map_or((value, 0), |(path, index)| {
        index
            .trim()
            .parse()
            .map_or((value, 0), |index| (path, index))
    });
    let path = Path::new(path.trim().trim_matches('"'));
    path.extension()
        .is_some_and(|extension| {
            ["exe", "dll", "icl"]
                .iter()
                .any(|suffix| extension.eq_ignore_ascii_case(suffix))
        })
        .then_some((path, index))
}

fn resolve_tc_icon(
    value: &str,
    bar: &Path,
    resolve: &impl Fn(&str) -> Option<std::ffi::OsString>,
) -> String {
    if value.is_empty() {
        return String::new();
    }
    let (raw, index) = resource_icon(value).map_or((value, None), |(path, index)| {
        (path.to_str().unwrap_or(value), Some(index))
    });
    let Some(path) = profile::expand_tc_path(raw, resolve).map(PathBuf::from) else {
        return String::new();
    };
    let path = if path.is_absolute() {
        path
    } else {
        let relative = bar.parent().unwrap_or_else(|| Path::new("")).join(&path);
        if relative.is_file() {
            relative
        } else {
            resolve("COMMANDER_PATH")
                .map(PathBuf::from)
                .map_or(relative, |directory| directory.join(path))
        }
    };
    let path = display_path(&path);
    index.map_or(path.clone(), |index| format!("{path},{index}"))
}

fn start_directory(value: &str) -> io::Result<Option<PathBuf>> {
    let value = value.trim().trim_matches('"');
    if value.chars().any(char::is_control) || value.contains('"') {
        return Err(io::Error::other(
            "Der Startordner enthält ungültige Zeichen.",
        ));
    }
    let path = PathBuf::from(value);
    if !path.is_absolute() && (path.has_root() || value.contains(':')) {
        return Err(io::Error::other(
            "Gib einen absoluten oder relativ zum Quellordner gültigen Startordner an.",
        ));
    }
    Ok((!value.is_empty()).then_some(path))
}

fn parse_button(value: &str, working_directory: &str) -> io::Result<ButtonbarButton> {
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
    let (path, window, tc_arguments, prompt_arguments) = match command.split_once(':') {
        Some(("tc", path)) => (path, ProgramWindow::Normal, true, false),
        Some(("min", path)) => (path, ProgramWindow::Minimized, false, false),
        Some(("max", path)) => (path, ProgramWindow::Maximized, false, false),
        Some(("tc-min", path)) => (path, ProgramWindow::Minimized, true, false),
        Some(("tc-max", path)) => (path, ProgramWindow::Maximized, true, false),
        Some(("tc-ask", path)) => (path, ProgramWindow::Normal, true, true),
        Some(("tc-min-ask", path)) => (path, ProgramWindow::Minimized, true, true),
        Some(("tc-max-ask", path)) => (path, ProgramWindow::Maximized, true, true),
        _ => (command, ProgramWindow::Normal, false, false),
    };
    let path = PathBuf::from(path.trim_start());
    let working_directory = start_directory(working_directory)?;
    if working_directory.is_some() && !program_path(&path) {
        return Err(io::Error::other(
            "Ein Startordner gilt für EXE-Schaltflächen.",
        ));
    }
    let action = if arguments.is_empty() && command.eq_ignore_ascii_case(MENU_COMMAND) {
        ButtonbarAction::Menu {
            buttons: Vec::new(),
        }
    } else if arguments.is_empty() && command.eq_ignore_ascii_case(SWITCH_COMMAND) {
        ButtonbarAction::SwitchBar {
            bar: None,
            buttons: Some(Vec::new()),
        }
    } else if program_path(&path) {
        if tc_arguments {
            TcArguments::parse(&arguments)?;
        }
        ButtonbarAction::Program {
            path,
            arguments,
            working_directory,
            window,
            tc_arguments,
            prompt_arguments,
        }
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

    #[test]
    fn tc_buttonbars_preserve_switch_targets_and_default_leiste() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("root.bar");
        let child = fixture.path().join("child.bar");
        std::fs::write(&root, "[Buttonbar]\nButtoncount=2\ncmd1=OPENBAR2\nparam1=child.bar\nmenu1=Leiste\ncmd2=OPENBAR1 *\nmenu2=Standard\n").unwrap();
        std::fs::write(&child, "[Buttonbar]\nButtoncount=1\ncmd1=cm_SearchFor\n").unwrap();
        let buttons = BarImport::default()
            .read(&root, &mut Vec::new(), 16)
            .unwrap();
        assert!(
            matches!(&buttons[0].action, ButtonbarAction::SwitchBar { bar: Some(1), buttons: Some(items) } if items.len() == 1)
        );
        assert!(matches!(
            &buttons[1].action,
            ButtonbarAction::SwitchBar {
                bar: Some(0),
                buttons: None
            }
        ));
        let export = fixture.path().join("export.bar");
        export_bar_file(&export, &buttons).unwrap();
        assert_eq!(
            BarImport::default()
                .read(&export, &mut Vec::new(), 16)
                .unwrap(),
            buttons
        );
        assert_eq!(
            resource_icon(r#""C:\Ordner, mit Komma\icons.dll",-110"#)
                .unwrap()
                .1,
            -110
        );
    }

    #[test]
    fn nested_buttonbars_round_trip_and_skip_cycle() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("root.bar");
        let child = fixture.path().join("child bar.bar");
        let leaf = fixture.path().join("leaf.bar");
        std::fs::write(&root, "[Buttonbar]\nButtoncount=1\ncmd1=OPENBARMENU\nparam1=\"child bar.bar\"\nmenu1=Werkzeuge\n").unwrap();
        std::fs::write(
            &child,
            "[Buttonbar]\nButtoncount=2\ncmd1=leaf.bar\niconic1=1\nmenu1=Dateien\ncmd2=OPENBAR root.bar\n",
        )
        .unwrap();
        std::fs::write(
            &leaf,
            "[Buttonbar]\nButtoncount=1\ncmd1=cm_SearchStandalone\nmenu1=Suche\n",
        )
        .unwrap();
        let mut importer = BarImport::default();
        let buttons = importer.read(&root, &mut Vec::new(), 16).unwrap();
        assert_eq!(importer.omitted, 1);
        assert_eq!(buttons[0].icon, "ordner");
        let ButtonbarAction::Menu { buttons: children } = &buttons[0].action else {
            panic!()
        };
        let ButtonbarAction::Menu { buttons: leaves } = &children[0].action else {
            panic!()
        };
        assert_eq!(leaves[0].label, "Suche");
        let exported = fixture.path().join("export.bar");
        export_bar_file(&exported, &buttons).unwrap();
        let mut importer = BarImport::default();
        assert_eq!(
            importer.read(&exported, &mut Vec::new(), 16).unwrap(),
            buttons
        );
        assert_eq!(importer.omitted, 0);
        let entries_before = fixture.path().read_dir().unwrap().count();
        assert!(export_bar_file(&exported, &buttons).is_err());
        assert_eq!(fixture.path().read_dir().unwrap().count(), entries_before);
    }

    #[cfg(windows)]
    #[test]
    fn tc_buttonbar_round_trip_keeps_command_parameters_and_windows_quoting() {
        let imported: Vec<_> = profile::parse_tc_buttonbar(concat!(
            "[Buttonbar]\nBUTTONCOUNT=3\n",
            "cmd1=cm_CompareDirs\nparam1=2\nmenu1=Größen prüfen\n",
            "cmd2=\"C:\\Program Files\\Tool.exe\"\nparam2=\"mit Leerzeichen\" \"\"\nmenu2=Tool\n",
            "path2=C:\\Start Ordner\n",
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
                working_directory: Some(PathBuf::from(r"C:\Start Ordner")),
                window: ProgramWindow::Normal,
                tc_arguments: false,
                prompt_arguments: false,
            }
        );
        let output = export_tc_buttonbar(&imported, &mut |_| unreachable!()).unwrap();
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

    #[cfg(windows)]
    #[test]
    fn tc_buttonbar_round_trips_program_window_mode_and_argument_placeholders() {
        let button = parse_button(
            "Öffnen\nterminal\ntc-min:C:\\Programme\\test.exe\n%P%N\n%%",
            "",
        )
        .unwrap();
        let encoded =
            export_tc_buttonbar(std::slice::from_ref(&button), &mut |_| unreachable!()).unwrap();
        assert!(encoded.contains("iconic1=1"));
        let restored = profile::parse_tc_buttonbar(&encoded)
            .unwrap()
            .into_iter()
            .filter_map(import_tc_button)
            .next()
            .unwrap();
        assert_eq!(restored.action, button.action);
        assert_eq!(parse_button(&button_text(&button), "").unwrap(), button);
        assert!(parse_button("X\nterminal\ntc:C:\\test.exe\n%L", "").is_ok());
        assert!(parse_button("X\nterminal\ntc:C:\\test.exe\n%L\n%UL", "").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn tc_buttonbar_preserves_an_argument_prompt_and_keeps_quoted_question_marks_literal() {
        let imported: Vec<_> = profile::parse_tc_buttonbar(concat!(
            "[Buttonbar]\nButtoncount=3\n",
            "cmd1=C:\\Tools\\Test.exe\nparam1=?\"/in=%v\" \"C:\\\\\"\niconic1=-1\n",
            "cmd2=C:\\Tools\\Test.exe\nparam2=\"?literal\" %v\n",
            "cmd3=C:\\Tools\\Test.exe\nparam3=?\n",
        ))
        .unwrap()
        .into_iter()
        .map(|entry| import_tc_button(entry).unwrap())
        .collect();
        assert!(matches!(
            &imported[0].action,
            ButtonbarAction::Program { prompt_arguments: true, window: ProgramWindow::Maximized, arguments, .. }
                if arguments == &["/in=%v", r"C:\"]
        ));
        assert!(matches!(
            &imported[1].action,
            ButtonbarAction::Program { prompt_arguments: false, arguments, .. }
                if arguments == &["?literal", "%v"]
        ));
        assert!(matches!(
            &imported[2].action,
            ButtonbarAction::Program { prompt_arguments: true, arguments, .. } if arguments.is_empty()
        ));
        let output = export_tc_buttonbar(&imported, &mut |_| unreachable!()).unwrap();
        assert!(output.contains(r#"param1=?"/in=%v" "C:\\""#));
        assert!(output.contains(r#"param2="?literal" "%v""#));
        assert!(output.contains("param3=?\r\n"));
        for (button, entry) in imported
            .iter()
            .zip(profile::parse_tc_buttonbar(&output).unwrap())
        {
            assert_eq!(&import_tc_button(entry).unwrap(), button);
            assert_eq!(&parse_button(&button_text(button), "").unwrap(), button);
        }
    }

    #[test]
    fn buttonbar_parses_internal_parameters_and_separate_program_arguments() {
        let button = parse_button("Abgleichen\nsuche\ncm_CompareDirs 2", "").unwrap();
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
        let parsed = parse_button(&value, "bin").unwrap();
        assert_eq!(
            parsed.action,
            ButtonbarAction::Program {
                path: executable,
                arguments: vec!["--file".into(), "A B.txt".into()],
                working_directory: Some(PathBuf::from("bin")),
                window: ProgramWindow::Normal,
                tc_arguments: false,
                prompt_arguments: false,
            }
        );
        assert_eq!(parse_button(&button_text(&parsed), "bin").unwrap(), parsed);
        assert!(parse_button("Programm\nterminal\ntest.exe", "").is_err());
        assert!(parse_button(&value, r"C:relative").is_err());
        assert!(parse_button("Suchen\nsuche\nsearch", "bin").is_err());
    }

    #[test]
    fn custom_button_icon_preserves_path_and_color() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("Eigene Grafik.png");
        image::RgbaImage::from_pixel(64, 32, image::Rgba([1, 22, 45, 255]))
            .save(&path)
            .unwrap();
        let button = parse_button(&format!("Öffnen\n{}\nsearch", path.display()), "").unwrap();
        assert_eq!(Path::new(&button.icon), path);
        let icon = icon_image(&button.icon).unwrap();
        assert_eq!(icon.dimensions(), (32, 16));
        assert_eq!(icon.get_pixel(0, 0).0, [1, 22, 45, 255]);
        assert!(parse_button("Öffnen\nrelative.png\nsearch", "").is_err());
    }

    #[test]
    fn tc_buttonbar_writes_unicode_lists_and_releases_the_temporary_path() {
        let paths = ["C:/A Space/Größe.txt", "C:/A Space/𐐀.txt"]
            .map(PathBuf::from)
            .to_vec();
        let utf8 = ListArgument::parse("%UL")
            .unwrap()
            .write(paths.clone())
            .unwrap();
        assert_eq!(
            std::fs::read(&utf8).unwrap(),
            "\u{feff}C:/A Space/Größe.txt\r\nC:/A Space/𐐀.txt\r\n".as_bytes()
        );
        let utf16 = ListArgument::parse("%WF").unwrap().write(paths).unwrap();
        let bytes = std::fs::read(&utf16).unwrap();
        let units = bytes
            .chunks_exact(2)
            .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
            .collect::<Vec<_>>();
        assert_eq!(
            String::from_utf16(&units).unwrap(),
            "\u{feff}Größe.txt\r\n𐐀.txt\r\n"
        );
        let saved = utf16.to_path_buf();
        drop(utf16);
        assert!(!saved.exists());
        assert!(TcArguments::parse(&["%a".into()]).is_err());
    }
}
