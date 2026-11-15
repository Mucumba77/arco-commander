//! Shared presentation for navigation configuration. Choice IDs always refer to the
//! original backend options, including after filtering or hiding toolbar actions.
use super::{App, buttonbars, favorites};
use crate::{
    NavigationData, NavigationForm, NavigationRow,
    commands::{COMMANDS, Dialog, Pending},
    display_path,
    locale::tr,
};
use arco_core::profile::{ButtonbarAction, ButtonbarButton, ProgramWindow};
use slint::{Model, ModelRc, VecModel};
use std::io;

fn row(
    label: impl Into<String>,
    detail: impl Into<String>,
    choice: usize,
    kind: i32,
) -> NavigationRow {
    NavigationRow {
        label: label.into().into(),
        detail: detail.into().into(),
        choice: choice as i32,
        kind,
    }
}

fn button_row(button: &ButtonbarButton, choice: usize) -> NavigationRow {
    let (detail, kind) = match &button.action {
        ButtonbarAction::Internal { command } => {
            (format!("{} · {command}", tr("Interner Befehl")), 4)
        }
        ButtonbarAction::Program {
            path, arguments, ..
        } => (
            format!(
                "{} · {} · {} {}",
                tr("Programm"),
                display_path(path),
                arguments.len(),
                tr("Argumente")
            ),
            3,
        ),
        ButtonbarAction::Menu { buttons } => (
            format!("{} · {} {}", tr("Untermenü"), buttons.len(), tr("Einträge")),
            2,
        ),
        ButtonbarAction::SwitchBar { bar, buttons } => (
            format!(
                "{} · {} · {} {}",
                tr("Leistenwechsel"),
                bar.map_or_else(|| tr("Aktuelle Leiste").into(), |b| format!("{}", b + 1)),
                buttons.as_ref().map_or(0, Vec::len),
                tr("Einträge")
            ),
            if buttons.is_some() { 2 } else { 4 },
        ),
    };
    row(button.label.clone(), detail, choice, kind)
}

fn action_kind(action: &ButtonbarAction) -> i32 {
    match action {
        ButtonbarAction::Internal { .. } => 0,
        ButtonbarAction::Program { .. } => 1,
        ButtonbarAction::Menu { .. } => 2,
        ButtonbarAction::SwitchBar { .. } => 3,
    }
}

fn button_form(button: &ButtonbarButton) -> NavigationForm {
    let mut form = NavigationForm {
        name: button.label.clone().into(),
        icon: button.icon.clone().into(),
        action: action_kind(&button.action),
        ..Default::default()
    };
    match &button.action {
        ButtonbarAction::Internal { command } => form.command = command.clone().into(),
        ButtonbarAction::Program {
            path,
            arguments,
            working_directory,
            window,
            tc_arguments,
            prompt_arguments,
        } => {
            form.command = display_path(path).into();
            form.arguments = arguments.join("\n").into();
            form.args = !arguments.is_empty();
            form.working = working_directory
                .as_deref()
                .map(display_path)
                .unwrap_or_default()
                .into();
            form.window = match window {
                ProgramWindow::Normal => 0,
                ProgramWindow::Minimized => 1,
                ProgramWindow::Maximized => 2,
            };
            form.tc = *tc_arguments;
            form.prompt = *prompt_arguments;
        }
        _ => (),
    }
    form
}

fn child_count(button: &ButtonbarButton) -> usize {
    match &button.action {
        ButtonbarAction::Menu { buttons }
        | ButtonbarAction::SwitchBar {
            buttons: Some(buttons),
            ..
        } => buttons.len(),
        _ => 0,
    }
}

impl App {
    pub(crate) fn present_navigation(
        &mut self,
        pending: &Pending,
        source: &Dialog<'_>,
    ) -> Option<&'static str> {
        let mut data = NavigationData {
            title: tr(source.title).into(),
            description: tr(source.description).into(),
            primary: tr("Öffnen").into(),
            first_label: tr(source.placeholder).into(),
            second_label: tr(source.second_placeholder).into(),
            initial: NavigationForm {
                name: source.value.into(),
                other: source.second.into(),
                ..Default::default()
            },
            options: super::strings(source.options.iter().map(|label| tr(label))),
            ..Default::default()
        };
        let mut rows: Vec<_> = source
            .options
            .iter()
            .enumerate()
            .map(|(i, label)| row(tr(label), "", i, 4))
            .collect();
        match pending {
            Pending::Favorites(favorites::Setup::Browse(choices)) => {
                rows = choices
                    .iter()
                    .enumerate()
                    .map(|(i, choice)| match choice {
                        favorites::Choice::Open(entry) => row(
                            favorites::label(entry),
                            format!(
                                "{}{}",
                                display_path(&entry.path),
                                entry
                                    .other
                                    .as_deref()
                                    .map(|p| format!("  ↔  {}", display_path(p)))
                                    .unwrap_or_default()
                            ),
                            i,
                            1,
                        ),
                        favorites::Choice::Group(group) => row(
                            source.options.get(i).copied().unwrap_or_default(),
                            group.join(" › "),
                            i,
                            2,
                        ),
                        favorites::Choice::Edit => row(
                            tr("Favoriten verwalten …"),
                            tr("Anlegen, bearbeiten und sortieren"),
                            i,
                            4,
                        ),
                    })
                    .collect();
                data.description = tr(
                    "Ordner und Gruppen öffnen; gekoppelte Favoriten wechseln beide Dateifenster.",
                )
                .into();
            }
            Pending::Favorites(favorites::Setup::Manage(entries)) => {
                data.manager = 1;
                data.primary = tr("Bearbeiten").into();
                data.description =
                    tr("Favoriten mit Namen, Gruppen und vollständigen Ordnerpfaden verwalten.")
                        .into();
                rows = entries
                    .iter()
                    .enumerate()
                    .map(|(i, e)| {
                        row(
                            favorites::label(e),
                            format!(
                                "{} · {}{}",
                                e.groups.join(" › "),
                                display_path(&e.path),
                                e.other
                                    .as_deref()
                                    .map(|p| format!(" ↔ {}", display_path(p)))
                                    .unwrap_or_default()
                            ),
                            i + 1,
                            1,
                        )
                    })
                    .collect();
            }
            Pending::Favorites(favorites::Setup::Change(_, _)) => {
                data.primary = tr("Auswählen").into();
            }
            Pending::Favorites(favorites::Setup::Edit(_, _)) => {
                data.mode = 1;
                data.primary = tr("Speichern").into();
                data.first_label = tr("Name (optional)").into();
                data.description =
                    tr("Ein Favorit öffnet einen Ordner oder ein zusammengehöriges Ordnerpaar.")
                        .into();
                let values: Vec<_> = source.value.split('\n').collect();
                data.initial = NavigationForm {
                    name: values.first().copied().unwrap_or_default().into(),
                    groups: values.get(1).copied().unwrap_or_default().into(),
                    path: values.get(2).copied().unwrap_or_default().into(),
                    other: values.get(3).copied().unwrap_or_default().into(),
                    ..Default::default()
                };
            }
            Pending::Buttonbar => {
                data.primary = tr("Verwalten").into();
            }
            Pending::ButtonbarList(bar) => {
                data.manager = 2;
                data.primary = tr("Bearbeiten").into();
                rows = self
                    .editing_buttons(*bar)
                    .iter()
                    .enumerate()
                    .map(|(i, b)| button_row(b, i + 3))
                    .collect();
                data.description = format!(
                    "{} · {} / {} · {} {}",
                    tr("Schaltflächen in Anzeigereihenfolge"),
                    rows.len(),
                    self.editing_limit(*bar),
                    self.buttonbar_edit_path.len(),
                    tr("Untermenüebenen")
                )
                .into();
            }
            Pending::ButtonbarEdit(bar, index) => {
                data.mode = 2;
                data.primary = tr("Speichern").into();
                data.first_label = tr("Beschriftung").into();
                data.description = tr("Darstellung und Aktion getrennt bearbeiten. Programme werden erst beim ausdrücklichen Aufruf gestartet.").into();
                let default = ButtonbarButton {
                    label: tr("Dateien suchen").into(),
                    icon: "suche".into(),
                    action: ButtonbarAction::Internal {
                        command: "search".into(),
                    },
                };
                let button = index
                    .and_then(|i| self.editing_buttons(*bar).get(i))
                    .unwrap_or(&default);
                data.initial = button_form(button);
                data.original_action = data.initial.action;
                data.children = child_count(button) as i32;
                data.options = super::strings(
                    std::iter::once(tr("Befehl wählen …").to_owned()).chain(
                        COMMANDS
                            .iter()
                            .map(|(label, command)| format!("{} · {command}", tr(label))),
                    ),
                );
            }
            Pending::Starter => {
                data.manager = 6;
                data.primary = tr("Ausführen").into();
                data.description = tr("Wähle eine Aktion. Ausführen oder Doppelklick startet sie; Verwalten ändert nur die Konfiguration.").into();
                rows = self
                    .profile
                    .starter
                    .iter()
                    .enumerate()
                    .map(|(i, b)| button_row(b, i))
                    .collect();
            }
            Pending::ButtonbarMenu(_, levels) => {
                if let Some((_, buttons)) = levels.last() {
                    rows = buttons
                        .iter()
                        .enumerate()
                        .map(|(i, b)| button_row(b, i))
                        .collect();
                    if source.options.len() > buttons.len() {
                        rows.push(row(tr("Eine Ebene nach oben"), "", buttons.len(), 2));
                    }
                }
                data.primary = tr("Ausführen / öffnen").into();
            }
            Pending::ButtonbarAction(_, _) => {
                data.manager = 5;
                data.primary = tr("Auswählen").into();
            }
            Pending::ButtonbarTemplates(names) => {
                data.manager = 4;
                data.primary = tr("Vorlage öffnen").into();
                rows = names
                    .iter()
                    .enumerate()
                    .map(|(i, name)| {
                        let detail = self
                            .profile
                            .buttonbar_templates
                            .get(name)
                            .map(|t| {
                                format!(
                                    "{} / {} / {} {}",
                                    t.bars[0].len(),
                                    t.bars[1].len(),
                                    t.starter.len(),
                                    tr("Schaltflächen")
                                )
                            })
                            .unwrap_or_default();
                        row(name.clone(), detail, i + 1, 4)
                    })
                    .collect();
            }
            Pending::ButtonbarTemplate(_) => {
                data.manager = 5;
                data.primary = tr("Auswählen").into();
            }
            Pending::ButtonbarTemplateSave
            | Pending::TabRename
            | Pending::TabGroupSave
            | Pending::TabImport
            | Pending::TabExport(_)
            | Pending::ButtonbarFile(_, _)
            | Pending::ButtonbarProgram { .. } => {
                data.mode = 3;
                data.primary = tr(match pending {
                    Pending::TabImport | Pending::ButtonbarFile(_, true) => "Vorschau laden",
                    Pending::TabExport(_) | Pending::ButtonbarFile(_, false) => "Exportieren",
                    Pending::ButtonbarProgram { .. } => "Programm starten",
                    _ => "Speichern",
                })
                .into();
                data.first_label = tr(match pending {
                    Pending::TabRename => "Tab-Name (leer = Ordnername)",
                    Pending::TabGroupSave => "Gruppenname",
                    Pending::ButtonbarTemplateSave => "Vorlagenname",
                    Pending::ButtonbarProgram { .. } => "Windows-Argumente",
                    _ => "Absoluter Dateipfad",
                })
                .into();
                data.file_mode = match pending {
                    Pending::TabImport | Pending::ButtonbarFile(_, true) => 1,
                    Pending::TabExport(_) | Pending::ButtonbarFile(_, false) => 2,
                    _ => 0,
                };
            }
            Pending::TabGroups(names) => {
                data.manager = 3;
                data.primary = tr("Gruppe öffnen").into();
                data.description = tr("Tab-Gruppen speichern beide Dateifenster einschließlich Pfaden, Namen und Sperren.").into();
                rows = names
                    .iter()
                    .enumerate()
                    .map(|(i, name)| {
                        let detail = self
                            .profile
                            .tab_groups
                            .get(name)
                            .map(|p| {
                                format!(
                                    "{}: {} · {}: {}",
                                    tr("Links"),
                                    p[0].tabs.len(),
                                    tr("Rechts"),
                                    p[1].tabs.len()
                                )
                            })
                            .unwrap_or_default();
                        row(name.clone(), detail, i + 3, 2)
                    })
                    .collect();
            }
            Pending::TabMenu => {
                data.primary = tr("Auswählen").into();
                for r in &mut rows {
                    r.detail = self.current().location().display().into();
                    r.kind = match r.choice {
                        0 => 7,
                        1..=3 => 8,
                        4..=6 => 9,
                        7..=9 => 5,
                        10 => 2,
                        _ => 6,
                    };
                }
            }
            Pending::TabGroupAction(_) => data.primary = tr("Auswählen").into(),
            Pending::TabList => {
                data.manager = 7;
                rows = self.panels[self.active]
                    .tabs
                    .iter()
                    .enumerate()
                    .map(|(i, tab)| {
                        let lock = match tab.options.lock {
                            arco_core::profile::TabLock::Unlocked => tr("Nicht gesperrt"),
                            arco_core::profile::TabLock::Fixed => tr("Ordner fest gesperrt"),
                            arco_core::profile::TabLock::ReturnToBase => {
                                tr("Rückkehr zum Stammordner")
                            }
                        };
                        row(
                            if tab.options.title.is_empty() {
                                format!("{} · {}", i + 1, tab.location().display())
                            } else {
                                format!("{} · {}", i + 1, tab.options.title)
                            },
                            format!("{lock} · {}", tab.location().display()),
                            i,
                            2,
                        )
                    })
                    .collect();
            }
            Pending::ReopenTab => {
                for r in &mut rows {
                    let text = r.label.to_string();
                    if let Some((name, path)) = text.rsplit_once(" · ") {
                        r.label = name.into();
                        r.detail = path.into();
                    }
                    r.kind = 2;
                }
            }
            Pending::TabClose
            | Pending::TabGroupReplace(_)
            | Pending::TabGroupDelete(_)
            | Pending::ButtonbarImport(_, _)
            | Pending::ApplyTabImport(_, _) => {
                data.review = true;
                data.primary = tr(match pending {
                    Pending::TabClose => "Tab schließen",
                    Pending::TabGroupDelete(_) => "Gruppe löschen",
                    Pending::TabGroupReplace(_) => "Tabs ersetzen",
                    _ => "Übernehmen",
                })
                .into();
                rows = source
                    .value
                    .lines()
                    .enumerate()
                    .filter(|(_, line)| !line.trim().is_empty())
                    .map(|(i, line)| row(line, "", i, 2))
                    .collect();
            }
            _ => return None,
        }
        let kind = if data.mode == 0 {
            "navigation-list"
        } else {
            "navigation-editor"
        };
        self.install_navigation(data, rows);
        Some(kind)
    }

    pub(super) fn install_navigation(&mut self, data: NavigationData, rows: Vec<NavigationRow>) {
        self.navigation_rows = rows;
        let ui = self.ui();
        ui.set_navigation_notice("".into());
        ui.set_navigation_selection(0);
        ui.set_navigation_rows(ModelRc::new(VecModel::from(self.navigation_rows.clone())));
        ui.set_navigation_data(data);
    }

    pub(crate) fn filter_navigation(&self, value: &str) {
        let ui = self.ui();
        ui.set_navigation_rows(ModelRc::new(VecModel::from(filtered_rows(
            &self.navigation_rows,
            value,
        ))));
        ui.set_navigation_selection(0);
    }

    pub(crate) fn navigation_action(&mut self, action: i32, choice: i32) {
        let selected = usize::try_from(choice).ok();
        // Reject stale or filtered-out IDs before mutating configuration.
        if matches!(action, 0 | 2..=6)
            && !self.ui().get_navigation_data().review
            && !self
                .ui()
                .get_navigation_rows()
                .iter()
                .any(|r| Some(r.choice as usize) == selected)
        {
            return;
        }
        let revision = self.ui().get_dialog_revision();
        let result = match self.pending.as_ref() {
            Some(
                Pending::PackerProfiles(_)
                | Pending::RenameSetup(super::rename_setup::Setup::Choose(_)),
            ) => self.auxiliary_list_action(action, selected),
            Some(Pending::ViewSetup(_) | Pending::Associations(_)) => {
                self.configuration_list_action(action, selected)
            }
            Some(Pending::Favorites(favorites::Setup::Manage(entries))) => {
                let entries = entries.clone();
                let index = selected.and_then(|i| i.checked_sub(1));
                if action == 1 {
                    self.edit_favorite(entries, None);
                    Ok(())
                } else if let Some(index) = index {
                    self.submit_favorite(
                        favorites::Setup::Change(entries, index),
                        "",
                        match action {
                            0 | 2 => 0,
                            3 => 1,
                            4 => 2,
                            5 => 3,
                            _ => return,
                        },
                    )
                } else {
                    return;
                }
            }
            Some(Pending::ButtonbarList(bar)) => {
                let bar = *bar;
                match action {
                    1 => {
                        self.buttonbar_select(bar, 0);
                        Ok(())
                    }
                    7 => {
                        self.buttonbar_edit_path.pop();
                        self.buttonbar_list(bar);
                        Ok(())
                    }
                    8 | 9 => {
                        self.buttonbar_select(bar, if action == 8 { 1 } else { 2 });
                        Ok(())
                    }
                    _ => {
                        if let Some(index) = selected.and_then(|i| i.checked_sub(3)) {
                            self.buttonbar_action(
                                bar,
                                index,
                                match action {
                                    0 | 2 => 0,
                                    3 => 1,
                                    4 => 2,
                                    5 => 3,
                                    6 => 4,
                                    _ => return,
                                },
                            )
                        } else {
                            return;
                        }
                    }
                }
            }
            Some(Pending::TabGroups(names)) if matches!(action, 1 | 8 | 9) => {
                let names = names.clone();
                self.choose_tab_group(
                    &names,
                    match action {
                        1 => 0,
                        8 => 1,
                        _ => 2,
                    },
                );
                Ok(())
            }
            Some(Pending::ButtonbarTemplates(_)) if action == 1 => {
                self.select_buttonbar_template(None);
                Ok(())
            }
            Some(Pending::Starter) if action == 10 => {
                self.buttonbar_edit_path.clear();
                self.buttonbar_list(2);
                Ok(())
            }
            Some(Pending::TabList) if action == 10 => {
                if let Some(index) = selected {
                    self.select_tab(index);
                    self.show_tab_menu();
                }
                Ok(())
            }
            _ if action == 0 => {
                let option = if self.ui().get_navigation_data().review {
                    0
                } else {
                    selected.unwrap_or(0)
                };
                self.submit_navigation_pending("", "", option)
            }
            _ => return,
        };
        match result {
            Err(error) => self
                .ui()
                .set_navigation_notice(tr(&error.to_string()).into()),
            Ok(()) => {
                if self.ui().get_dialog_revision() == revision {
                    self.close_dialog();
                } else if matches!(action, 3 | 4 | 5) {
                    let next = selected.unwrap_or(0) as i32
                        + match action {
                            3 => -1,
                            4 => 1,
                            _ => 0,
                        };
                    let rows = self.ui().get_navigation_rows();
                    if let Some(index) = rows.iter().position(|r| r.choice == next) {
                        self.ui().set_navigation_selection(index as i32);
                    }
                }
            }
        }
        self.publish(self.active);
    }

    pub(crate) fn submit_navigation_form(&mut self, form: NavigationForm) {
        let revision = self.ui().get_dialog_revision();
        let result = match self.pending.as_ref() {
            Some(Pending::Favorites(favorites::Setup::Edit(entries, index))) => {
                let setup = favorites::Setup::Edit(entries.clone(), *index);
                favorite_text(&form)
                    .and_then(|value| self.submit_favorite(setup, &value, 0))
                    .map(|()| {
                        self.manage_favorites();
                    })
            }
            Some(Pending::ButtonbarEdit(bar, index)) => {
                let (bar, index) = (*bar, *index);
                let previous = index
                    .and_then(|i| self.editing_buttons(bar).get(i))
                    .cloned();
                button_text(&form, previous.as_ref()).and_then(|text| {
                    self.save_button(
                        bar,
                        index,
                        &text,
                        if form.action == 1 {
                            form.working.as_str()
                        } else {
                            ""
                        },
                    )
                })
            }
            _ => self.submit_navigation_pending(
                form.name.as_str(),
                form.other.as_str(),
                form.option.max(0) as usize,
            ),
        };
        match result {
            Err(error) => self
                .ui()
                .set_navigation_notice(tr(&error.to_string()).into()),
            Ok(()) if self.ui().get_dialog_revision() == revision => self.close_dialog(),
            _ => (),
        }
        self.publish(self.active);
    }

    fn submit_navigation_pending(
        &mut self,
        value: &str,
        second: &str,
        option: usize,
    ) -> io::Result<()> {
        if let Some(Pending::ButtonbarProgram { .. }) = &self.pending {
            arco_windows::parse_windows_arguments(value)?;
            self.submit(value, second, option as i32);
            return Ok(());
        }
        if let Some(pending) = self.pending.as_ref().and_then(clone_pending) {
            self.execute(pending, value, second, option, 0, "")
        } else {
            // Import reviews contain owned non-cloneable parser state. Their existing
            // acceptance path is retained; they never contain editable user input.
            self.submit(value, second, option as i32);
            Ok(())
        }
    }
}

fn filtered_rows(rows: &[NavigationRow], query: &str) -> Vec<NavigationRow> {
    let terms: Vec<_> = query.split_whitespace().map(str::to_lowercase).collect();
    rows.iter()
        .filter(|r| {
            let haystack = format!("{} {}", r.label, r.detail).to_lowercase();
            terms.iter().all(|term| haystack.contains(term))
        })
        .cloned()
        .collect()
}

fn one_line(value: &str) -> io::Result<()> {
    if value.chars().any(char::is_control) {
        return Err(io::Error::other(
            "Ein einzeiliges Feld enthält Steuerzeichen.",
        ));
    }
    Ok(())
}
fn favorite_text(form: &NavigationForm) -> io::Result<String> {
    for value in [&form.name, &form.groups, &form.path, &form.other] {
        one_line(value)?;
    }
    Ok(format!(
        "{}\n{}\n{}\n{}",
        form.name, form.groups, form.path, form.other
    ))
}
fn button_text(form: &NavigationForm, previous: Option<&ButtonbarButton>) -> io::Result<String> {
    for value in [&form.name, &form.icon, &form.command, &form.working] {
        one_line(value)?;
    }
    if let Some(previous) = previous {
        if child_count(previous) > 0
            && action_kind(&previous.action) != form.action
            && !form.drop_children
        {
            return Err(io::Error::other(
                "Bestätige zuerst das Entfernen der vorhandenen Untereinträge.",
            ));
        }
    }
    let command = match form.action {
        0 => COMMANDS
            .iter()
            .find(|(label, command)| format!("{} · {command}", tr(label)) == form.command.as_str())
            .map_or_else(
                || form.command.to_string(),
                |(_, command)| (*command).into(),
            ),
        1 => {
            let prefix = match (form.window, form.tc || form.prompt, form.prompt) {
                (0, false, false) => "",
                (0, true, false) => "tc:",
                (0, _, true) => "tc-ask:",
                (1, false, false) => "min:",
                (1, true, false) => "tc-min:",
                (1, _, true) => "tc-min-ask:",
                (2, false, false) => "max:",
                (2, true, false) => "tc-max:",
                (2, _, true) => "tc-max-ask:",
                _ => return Err(io::Error::other("Wähle einen gültigen Fensterzustand.")),
            };
            format!("{prefix}{}", form.command.trim().trim_matches('"'))
        }
        2 => "Untermenü".into(),
        3 => "Leistenwechsel".into(),
        _ => return Err(io::Error::other("Wähle einen gültigen Aktionstyp.")),
    };
    let mut text = format!("{}\n{}\n{command}", form.name, form.icon);
    if form.action == 1 && form.args {
        text.push('\n');
        text.push_str(&form.arguments);
        text.push('\n');
    }
    let parsed =
        buttonbars::parse_button(&text, if form.action == 1 { &form.working } else { "" })?;
    if action_kind(&parsed.action) != form.action {
        return Err(io::Error::other(
            "Der Befehl passt nicht zum gewählten Aktionstyp.",
        ));
    }
    Ok(text)
}

pub(super) fn clone_pending(pending: &Pending) -> Option<Pending> {
    Some(match pending {
        Pending::ViewSetup(setup) => Pending::ViewSetup(setup.clone()),
        Pending::Associations(setup) => Pending::Associations(setup.clone()),
        Pending::Favorites(setup) => Pending::Favorites(setup.clone()),
        Pending::Buttonbar => Pending::Buttonbar,
        Pending::Starter => Pending::Starter,
        Pending::ButtonbarTemplates(v) => Pending::ButtonbarTemplates(v.clone()),
        Pending::ButtonbarTemplate(n) => Pending::ButtonbarTemplate(n.clone()),
        Pending::ButtonbarTemplateSave => Pending::ButtonbarTemplateSave,
        Pending::ButtonbarList(b) => Pending::ButtonbarList(*b),
        Pending::ButtonbarAction(b, i) => Pending::ButtonbarAction(*b, *i),
        Pending::ButtonbarFile(b, i) => Pending::ButtonbarFile(*b, *i),
        Pending::ButtonbarMenu(b, v) => Pending::ButtonbarMenu(*b, v.clone()),
        Pending::ButtonbarImport(b, v) => Pending::ButtonbarImport(*b, v.clone()),
        Pending::TabMenu => Pending::TabMenu,
        Pending::TabRename => Pending::TabRename,
        Pending::TabList => Pending::TabList,
        Pending::TabClose => Pending::TabClose,
        Pending::ReopenTab => Pending::ReopenTab,
        Pending::TabGroups(v) => Pending::TabGroups(v.clone()),
        Pending::TabGroupSave => Pending::TabGroupSave,
        Pending::TabGroupAction(n) => Pending::TabGroupAction(n.clone()),
        Pending::TabGroupReplace(n) => Pending::TabGroupReplace(n.clone()),
        Pending::TabGroupDelete(n) => Pending::TabGroupDelete(n.clone()),
        Pending::TabImport => Pending::TabImport,
        Pending::TabExport(n) => Pending::TabExport(n.clone()),
        _ => return super::auxiliary::clone_pending(pending),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn program(arguments: Vec<String>) -> ButtonbarButton {
        ButtonbarButton {
            label: "Editor".into(),
            icon: "terminal".into(),
            action: ButtonbarAction::Program {
                path: std::env::temp_dir().join("Arco Test").join("editor.exe"),
                arguments,
                working_directory: Some("bin".into()),
                window: ProgramWindow::Maximized,
                tc_arguments: true,
                prompt_arguments: true,
            },
        }
    }

    #[test]
    fn program_editor_preserves_empty_arguments_and_start_options() {
        for arguments in [
            vec![],
            vec![String::new()],
            vec![String::new(), "%P%N".into(), String::new()],
            vec!["a b".into(), "Größe.txt".into()],
        ] {
            let button = program(arguments);
            let form = button_form(&button);
            let text = button_text(&form, Some(&button)).unwrap();
            let saved = buttonbars::parse_button(&text, &form.working).unwrap();
            assert_eq!(saved, button);
        }
    }

    #[test]
    fn selecting_a_command_by_label_saves_the_command_id() {
        let (label, command) = COMMANDS.iter().find(|(_, c)| *c == "search").unwrap();
        let form = NavigationForm {
            name: "Suchen".into(),
            icon: "suche".into(),
            command: format!("{} · {command}", tr(label)).into(),
            ..Default::default()
        };
        let saved = buttonbars::parse_button(&button_text(&form, None).unwrap(), "").unwrap();
        assert_eq!(
            saved.action,
            ButtonbarAction::Internal {
                command: "search".into()
            }
        );
        let exe = NavigationForm {
            command: display_path(&std::env::temp_dir().join("test.exe")).into(),
            ..form
        };
        assert!(button_text(&exe, None).is_err());
    }

    #[test]
    fn nested_menus_and_imported_switch_targets_survive_editing() {
        let child = program(vec![]);
        for action in [
            ButtonbarAction::Menu {
                buttons: vec![child.clone()],
            },
            ButtonbarAction::SwitchBar {
                bar: Some(1),
                buttons: Some(vec![child]),
            },
            ButtonbarAction::SwitchBar {
                bar: Some(0),
                buttons: None,
            },
        ] {
            let previous = ButtonbarButton {
                label: "Vorher".into(),
                icon: "ordner".into(),
                action,
            };
            let mut form = button_form(&previous);
            form.name = "Nachher".into();
            let mut saved =
                buttonbars::parse_button(&button_text(&form, Some(&previous)).unwrap(), "")
                    .unwrap();
            buttonbars::retain_nested_action(&mut saved, &previous);
            assert_eq!(saved.action, previous.action);
            assert_eq!(saved.label, "Nachher");
            if child_count(&previous) > 0 {
                form.action = 0;
                form.command = "search".into();
                assert!(button_text(&form, Some(&previous)).is_err());
                form.drop_children = true;
                assert!(button_text(&form, Some(&previous)).is_ok());
            }
        }
    }

    #[test]
    fn filtered_rows_keep_backend_ids_and_match_paths_with_multiple_terms() {
        let rows = vec![
            row("Arbeit", r"C:\Projekt\Belege", 3, 2),
            row("Privat", r"D:\Fotos", 19, 1),
            row("Belege", r"C:\Privat", 24, 1),
        ];
        let filtered = filtered_rows(&rows, "PROJEKT arbeit");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].choice, 3);
        assert_eq!(filtered_rows(&rows, "fotos")[0].choice, 19);
        assert!(filtered_rows(&rows, "fehlt").is_empty());
        assert_eq!(filtered_rows(&rows, " ").len(), 3);
    }

    #[test]
    fn editors_reject_line_injection_and_mismatched_action_types() {
        let form = NavigationForm {
            name: "Echt\nAnderer Favorit".into(),
            path: r"C:\Ordner".into(),
            ..Default::default()
        };
        assert!(favorite_text(&form).is_err());
        let mut button = button_form(&program(vec![]));
        button.command = "search".into();
        assert!(button_text(&button, None).is_err());
        button.action = 0;
        button.icon = "suche\nsearch".into();
        assert!(button_text(&button, None).is_err());
    }
}
