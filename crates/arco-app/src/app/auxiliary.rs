use super::{App, strings};
use crate::{
    MessageData, MessageLevel, MessageStat, NavigationData, NavigationForm, NavigationRow,
    TextDraftData, TextDraftForm, TextDraftKind,
    commands::{Dialog, Pending},
    locale::tr,
};
use arco_core::files::TransferReport;
use std::{collections::VecDeque, io};

#[derive(Clone)]
pub(crate) struct Notice {
    level: MessageLevel,
    title: String,
    summary: String,
    details: String,
    stats: Vec<(String, String)>,
}
#[derive(Default)]
pub(crate) struct Notices {
    waiting: VecDeque<Notice>,
    last_result: Option<Notice>,
}

impl Notice {
    pub(super) fn info(title: &str, body: &str) -> Self {
        let summary = if body.len() < 240 && !body.contains('\n') {
            body.to_owned()
        } else {
            tr("Weitere Informationen stehen in den Details.").into()
        };
        Self {
            level: MessageLevel::Information,
            title: title.into(),
            summary,
            details: body.into(),
            stats: Vec::new(),
        }
    }
    fn error(title: &str, body: &str) -> Self {
        Self{level:MessageLevel::Error,summary:tr("Der Vorgang konnte nicht abgeschlossen werden. Die Details enthalten die Fehlermeldung.").into(),..Self::info(title,body)}
    }
    fn warning(title: &str, body: &str) -> Self {
        Self {level:MessageLevel::Warning,summary:tr("Einige Einträge konnten nicht vollständig verarbeitet werden. Die Details enthalten die betroffenen Stellen.").into(),..Self::info(title,body)}
    }
    fn transfer(label: &str, report: &TransferReport) -> Self {
        let level = if report.errors.is_empty() && !report.cancelled {
            MessageLevel::Success
        } else {
            MessageLevel::Warning
        };
        let summary = if report.cancelled {
            tr("Der Auftrag wurde abgebrochen. Bereits abgeschlossene Schritte bleiben bestehen.")
        } else if report.errors.is_empty() {
            tr("Der Auftrag ist abgeschlossen.")
        } else {
            tr(
                "Der Auftrag ist mit Fehlern abgeschlossen. Prüfe die betroffenen Einträge in den Details.",
            )
        };
        Self {
            level,
            title: label.into(),
            summary: summary.into(),
            details: report.errors.join("\n"),
            stats: [
                ("Abgeschlossen", report.completed),
                ("Übersprungen", report.skipped),
                ("Ignoriert", report.ignored),
                ("Fehler", report.errors.len()),
            ]
            .into_iter()
            .map(|(l, n)| (tr(l).into(), n.to_string()))
            .collect(),
        }
    }
    fn copy_text(&self) -> String {
        let mut text = format!(
            "{}\n{}",
            crate::locale::message(&self.title),
            crate::locale::message(&self.summary)
        );
        for (label, value) in &self.stats {
            text.push_str(&format!("\n{label}: {value}"));
        }
        if !self.details.is_empty() && self.details != self.summary {
            text.push_str("\n\n");
            text.push_str(&crate::locale::message(&self.details));
        }
        text
    }
    fn data(&self) -> MessageData {
        MessageData {
            level: self.level,
            title: crate::locale::message(&self.title).as_ref().into(),
            summary: crate::locale::message(&self.summary).as_ref().into(),
            details: crate::locale::message(&self.details).as_ref().into(),
            copy_text: self.copy_text().into(),
            stats: slint::ModelRc::new(slint::VecModel::from(
                self.stats
                    .iter()
                    .map(|(l, v)| MessageStat {
                        label: l.clone().into(),
                        value: v.clone().into(),
                    })
                    .collect::<Vec<_>>(),
            )),
            expanded: self.details.len() > 240
                || self.details.contains('\n')
                || self.level == MessageLevel::Error
                || self.level == MessageLevel::Warning,
            details_lines: self.details.lines().count().min(40) as i32,
            wide: self.details.len() > 500 || self.details.lines().count() > 10,
            ..Default::default()
        }
    }
}

fn busy(kind: &str, settings: bool, drive: i32, expanded_job: bool) -> bool {
    !kind.is_empty() || settings || drive >= 0 || expanded_job
}

// Only initial, dialog-producing results wait. In-place updates retain their
// existing session/request checks and must continue to reach their own editor.
fn result_waits(outcome: &super::Outcome, kind: &str) -> bool {
    use super::Outcome as O;
    let owner = match outcome {
        O::UtilityReady(_) => Some("utility"),
        O::Properties(_) => Some("properties"),
        O::SearchReview(_) => Some("advanced-search"),
        O::RenamePreview(_) => Some("multi-rename"),
        O::Sync(_) => Some("sync-options"),
        O::PluginsUpdated(..)
        | O::PluginPackage(_)
        | O::PluginImport(_)
        | O::WdxFields(..)
        | O::WdxEditable(..) => Some("plugins"),
        O::Preview { .. } | O::Image(..) | O::RichPreview(..) | O::StagedPreview(..) => {
            Some("viewer")
        }
        O::TextCompared(_, _, false) | O::BinaryCompared(_, _, false) => Some("text-comparison"),
        O::FileComment(..)
        | O::PrintPreview(..)
        | O::RenameNameList(..)
        | O::Ftp(_)
        | O::FtpEditReady(_)
        | O::Imported(_)
        | O::ImportedTabs(..)
        | O::ImportedFtp(_)
        | O::WfxColumnFields(..)
        | O::IgnoreImported(_)
        | O::LocalShares(_)
        | O::VolumeLabel(_) => Some(""),
        _ => return false,
    };
    !kind.is_empty() && owner != Some(kind)
}

impl App {
    pub(super) fn defer_result_dialog(&self, outcome: &super::Outcome) -> bool {
        let ui = self.ui();
        if self.job.is_some() && matches!(outcome, super::Outcome::UtilityReady(_)) {
            return true;
        }
        if result_waits(outcome, "__other_dialog__") {
            return ui.get_settings_open()
                || ui.get_drive_side() >= 0
                || (ui.get_job_active() && ui.get_job_expanded())
                || result_waits(outcome, &ui.get_dialog_kind());
        }
        false
    }
    pub(super) fn flush_deferred_results(&mut self) {
        let ui = self.ui();
        if self.job.is_none()
            && !busy(
                &ui.get_dialog_kind(),
                ui.get_settings_open(),
                ui.get_drive_side(),
                ui.get_job_active() && ui.get_job_expanded(),
            )
        {
            if let Some(outcome) = self.deferred_results.pop_front() {
                self.completed(outcome);
            }
        }
        ui.set_pending_result_count(self.deferred_results.len().min(i32::MAX as usize) as i32);
    }
    pub(crate) fn clone_retry_pending(&self) -> Option<Pending> {
        self.pending
            .as_ref()
            .and_then(super::navigation::clone_pending)
    }
    pub(crate) fn error(&self, title: &str, body: &str) {
        let notice = Notice::error(title, body);
        self.notices.borrow_mut().last_result = Some(notice.clone());
        self.ui().set_has_last_result(true);
        self.enqueue_notice(notice);
    }
    pub(super) fn warning(&self, title: &str, body: &str) {
        let notice = Notice::warning(title, body);
        self.notices.borrow_mut().last_result = Some(notice.clone());
        self.ui().set_has_last_result(true);
        self.enqueue_notice(notice);
    }
    pub(super) fn remember_information(&self, title: &str, body: &str) {
        let notice = Notice::info(title, body);
        self.notices.borrow_mut().last_result = Some(notice.clone());
        self.ui().set_has_last_result(true);
        self.enqueue_notice(notice);
    }
    pub(super) fn remember_success(&self, message: &str) {
        self.notices.borrow_mut().last_result = Some(Notice {
            level: MessageLevel::Success,
            title: tr("Auftrag abgeschlossen").into(),
            summary: message.into(),
            details: String::new(),
            stats: Vec::new(),
        });
        self.ui().set_has_last_result(true);
    }
    pub(super) fn enqueue_notice(&self, notice: Notice) {
        self.notices.borrow_mut().waiting.push_back(notice);
        self.flush_notices();
    }
    pub(super) fn flush_notices(&self) {
        let ui = self.ui();
        if !busy(
            &ui.get_dialog_kind(),
            ui.get_settings_open(),
            ui.get_drive_side(),
            ui.get_job_active() && ui.get_job_expanded(),
        ) {
            let next = self.notices.borrow_mut().waiting.pop_front();
            if let Some(next) = next {
                ui.set_message_data(next.data());
                ui.set_message_feedback("".into());
                ui.set_dialog_title(tr(&next.title).into());
                ui.set_dialog_value(next.details.into());
                ui.set_dialog_kind("message".into());
                ui.set_dialog_revision(ui.get_dialog_revision().wrapping_add(1));
            }
        }
        let notices = self.notices.borrow();
        ui.set_message_waiting(notices.waiting.len().min(i32::MAX as usize) as i32);
        ui.set_has_last_result(notices.last_result.is_some());
    }
    pub(super) fn remember_result(&self, label: &str, report: &TransferReport) {
        let notice = Notice::transfer(label, report);
        self.notices.borrow_mut().last_result = Some(notice.clone());
        self.ui().set_has_last_result(true);
        if report.cancelled || !report.errors.is_empty() {
            self.enqueue_notice(notice);
        }
    }
    pub(crate) fn show_messages(&self) {
        if !self.deferred_results.is_empty() {
            return;
        }
        if self.notices.borrow().waiting.is_empty() {
            let last = self.notices.borrow().last_result.clone();
            if let Some(last) = last {
                self.enqueue_notice(last);
            } else {
                self.status("Es liegen keine weiteren Meldungen vor.");
            }
        } else {
            self.flush_notices();
        }
    }
    pub(crate) fn copy_message(&self, all: bool) {
        let data = self.ui().get_message_data();
        let text = if all { &data.copy_text } else { &data.details };
        let result = arco_windows::copy_text(text.as_str());
        self.ui().set_message_feedback(match result {
            Ok(()) => tr("In die Zwischenablage kopiert.").into(),
            Err(e) => e.to_string().into(),
        });
    }
    pub(crate) fn present_auxiliary(
        &mut self,
        kind: &str,
        pending: &Pending,
        data: &Dialog<'_>,
    ) -> Option<&'static str> {
        if kind == "confirm"
            && let Pending::PackerProfiles(names) = pending
        {
            let rows = names
                .iter()
                .enumerate()
                .map(|(i, name)| NavigationRow {
                    label: name.clone().into(),
                    detail: self
                        .profile
                        .packer_profiles
                        .get(name)
                        .map(|p| {
                            format!(
                                "{} · {}",
                                p.extension.to_uppercase(),
                                crate::display_path(&p.executable)
                            )
                        })
                        .unwrap_or_default()
                        .into(),
                    choice: i as i32 + 1,
                    kind: 4,
                })
                .collect();
            self.install_navigation(
                NavigationData {
                    title: tr(data.title).into(),
                    description: tr(data.description).into(),
                    manager: 10,
                    primary: tr("Bearbeiten").into(),
                    ..Default::default()
                },
                rows,
            );
            return Some("navigation-list");
        }
        if kind == "confirm"
            && let Pending::RenameSetup(super::rename_setup::Setup::Choose(names)) = pending
        {
            let rows = names
                .iter()
                .enumerate()
                .map(|(i, name)| NavigationRow {
                    label: name.clone().into(),
                    detail: self
                        .profile
                        .rename_templates
                        .get(name)
                        .map(|p| p.mask.clone())
                        .unwrap_or_default()
                        .into(),
                    choice: i as i32 + 1,
                    kind: 7,
                })
                .collect();
            self.install_navigation(
                NavigationData {
                    title: tr(data.title).into(),
                    description: tr(data.description).into(),
                    manager: 11,
                    primary: tr("Umbenennvorschau").into(),
                    ..Default::default()
                },
                rows,
            );
            return Some("navigation-list");
        }
        if kind == "confirm" && !data.options.is_empty() {
            let mut presentation = NavigationData {
                title: tr(data.title).into(),
                description: tr(data.description).into(),
                primary: tr("Auswahl bestätigen").into(),
                ..Default::default()
            };
            if matches!(
                pending,
                Pending::PackerProfileAction(_)
                    | Pending::RenameSetup(super::rename_setup::Setup::Manage(_))
            ) {
                presentation.manager = 12;
                presentation.entry_offset = if matches!(pending, Pending::PackerProfileAction(_)) {
                    1
                } else {
                    2
                };
            }
            let rows = data
                .options
                .iter()
                .enumerate()
                .map(|(i, label)| NavigationRow {
                    label: tr(label).into(),
                    detail: "".into(),
                    choice: i as i32,
                    kind: if matches!(pending, Pending::KnownFolder | Pending::VirtualShellFolder) {
                        2
                    } else {
                        4
                    },
                })
                .collect();
            self.install_navigation(presentation, rows);
            return Some("navigation-list");
        }
        if matches!(kind, "review" | "print-preview" | "delete") {
            let message = review_data(kind, data);
            self.ui().set_message_data(message);
            self.ui().set_message_feedback("".into());
            return Some("message");
        }
        if kind == "input"
            && matches!(
                pending,
                Pending::IgnoreImport | Pending::ImportTc | Pending::SelectionFile(..)
            )
        {
            let save = matches!(pending, Pending::SelectionFile(_, Some(_)));
            self.install_navigation(
                NavigationData {
                    mode: 3,
                    title: tr(data.title).into(),
                    description: tr(data.description).into(),
                    primary: tr(if save { "Speichern" } else { "Einlesen" }).into(),
                    first_label: tr(if data.placeholder.is_empty() {
                        "Dateipfad"
                    } else {
                        data.placeholder
                    })
                    .into(),
                    initial: NavigationForm {
                        name: data.value.into(),
                        ..Default::default()
                    },
                    file_mode: if save { 2 } else { 1 },
                    ..Default::default()
                },
                Vec::new(),
            );
            return Some("navigation-editor");
        }
        if kind == "multiline" {
            let mut initial = TextDraftForm {
                body: data.value.into(),
                second: data.second.into(),
                ..Default::default()
            };
            let mut editor = TextDraftData {
                title: tr(data.title).into(),
                description: tr(data.description).into(),
                label: tr(if data.placeholder.is_empty() {
                    "Inhalt"
                } else {
                    data.placeholder
                })
                .into(),
                second_label: tr(data.second_placeholder).into(),
                options: strings(data.options.iter().map(|l| tr(l))),
                options_label: tr(if data.options_label.is_empty() {
                    "Auswahl"
                } else {
                    data.options_label
                })
                .into(),
                ..Default::default()
            };
            match pending {
                Pending::IgnoreList => {
                    editor.kind = TextDraftKind::Ignore;
                    initial.enabled = self.profile.ignore_enabled;
                    editor.description=tr("Regeln und Aktivierung gemeinsam bearbeiten. Änderungen gelten erst nach dem Speichern.").into();
                }
                Pending::PackerProfileEdit(_) => {
                    editor.kind = TextDraftKind::Packer;
                    let mut parts = data.value.splitn(4, '\n');
                    initial.name = parts.next().unwrap_or_default().into();
                    initial.program = parts.next().unwrap_or_default().into();
                    initial.extension = parts.next().unwrap_or_default().into();
                    initial.body = parts.next().unwrap_or_default().into();
                    editor.description =
                        tr("Profilname, Programm, Archivendung und Argumente getrennt bearbeiten.")
                            .into();
                }
                Pending::RenameSetup(super::rename_setup::Setup::Template(_)) => {
                    editor.kind = TextDraftKind::Rename;
                    initial = rename_form(data.value);
                    editor.description=tr("Namensmaske, Erweiterung und Suchoptionen als Vorlage speichern. Dabei werden keine Dateien umbenannt.").into();
                }
                _ => (),
            }
            editor.initial = initial;
            self.ui().set_text_draft_data(editor);
            return Some("text-draft");
        }
        None
    }
    pub(crate) fn submit_text_draft(&mut self, form: TextDraftForm) {
        let result = match self.pending.as_ref() {
            Some(Pending::IgnoreList) => self
                .save_ignore_list(&form.body, form.enabled)
                .map(|()| self.close_dialog()),
            Some(Pending::PackerProfileEdit(original)) => {
                let previous = original.clone();
                packer_text(&form)
                    .and_then(|text| self.save_packer_profile(previous.as_deref(), &text))
                    .map(|()| {
                        self.open_packer_profiles();
                    })
            }
            Some(Pending::RenameSetup(super::rename_setup::Setup::Template(_))) => {
                rename_text(&form).map(|text| self.submit(&text, "", 0))
            }
            _ => {
                self.submit(&form.body, &form.second, form.option);
                Ok(())
            }
        };
        if let Err(error) = result {
            self.ui().set_dialog_notice(tr(&error.to_string()).into());
        }
    }
    pub(crate) fn auxiliary_list_action(
        &mut self,
        action: i32,
        choice: Option<usize>,
    ) -> io::Result<()> {
        match self.clone_retry_pending() {
            Some(Pending::PackerProfiles(names)) => {
                if action == 1 {
                    self.edit_packer_profile(None, None, None);
                    return Ok(());
                }
                let name = choice
                    .and_then(|i| i.checked_sub(1))
                    .and_then(|i| names.get(i))
                    .cloned()
                    .ok_or_else(|| io::Error::other("Das Packerprofil fehlt."))?;
                match action {
                    0 | 2 => self.edit_packer_profile(Some(name), None, None),
                    5 => {
                        self.remove_packer_profile(&name)?;
                        self.open_packer_profiles();
                    }
                    _ => (),
                };
                Ok(())
            }
            Some(Pending::RenameSetup(super::rename_setup::Setup::Choose(names))) => {
                if action == 1 {
                    return self.submit_rename_setup(
                        super::rename_setup::Setup::Choose(names),
                        "",
                        0,
                    );
                }
                let name = choice
                    .and_then(|i| i.checked_sub(1))
                    .and_then(|i| names.get(i))
                    .cloned()
                    .ok_or_else(|| io::Error::other("Diese Umbenennvorlage fehlt."))?;
                self.submit_rename_setup(
                    super::rename_setup::Setup::Manage(name),
                    "",
                    match action {
                        0 => 0,
                        2 => 1,
                        5 => 2,
                        _ => return Ok(()),
                    },
                )
            }
            _ => Ok(()),
        }
    }
}

fn one_line(value: &str) -> io::Result<()> {
    if value.chars().any(char::is_control) {
        Err(io::Error::other(
            "Ein einzeiliges Feld enthält Steuerzeichen.",
        ))
    } else {
        Ok(())
    }
}

fn review_data(kind: &str, data: &Dialog<'_>) -> MessageData {
    let mut notice = Notice::info(data.title, data.value);
    notice.summary = tr(data.description).into();
    notice.level = if kind == "delete" {
        MessageLevel::Warning
    } else {
        MessageLevel::Information
    };
    let mut message = notice.data();
    message.expanded = !data.value.is_empty();
    message.options = strings(data.options.iter().map(|label| tr(label)));
    message.options_label = tr(if data.options_label.is_empty() {
        "Auswahl"
    } else {
        data.options_label
    })
    .into();
    message.destructive = kind == "delete";
    message.confirm = tr(if !data.confirm_label.is_empty() {
        data.confirm_label
    } else if kind == "print-preview" {
        "Drucken …"
    } else if kind == "delete" {
        "Löschen"
    } else {
        "Übernehmen"
    })
    .into();
    message
}
fn packer_text(form: &TextDraftForm) -> io::Result<String> {
    for text in [&form.name, &form.program, &form.extension] {
        one_line(text)?;
    }
    Ok(format!(
        "{}\n{}\n{}\n{}",
        form.name, form.program, form.extension, form.body
    ))
}
fn rename_form(value: &str) -> TextDraftForm {
    let mut lines = value.lines();
    let mut form = TextDraftForm {
        name: lines.next().unwrap_or_default().into(),
        body: lines.next().unwrap_or_default().into(),
        second: lines.next().unwrap_or_default().into(),
        counter_start: "1".into(),
        counter_step: "1".into(),
        counter_width: "1".into(),
        counter_every: "1".into(),
        ..Default::default()
    };
    for line in lines {
        if let Some((key, value)) = line.split_once('=') {
            match key {
                "Erweiterung" => {
                    form.separate = true;
                    form.extension = value.into();
                }
                "Suchen" => {
                    form.search = true;
                    form.find = value.into();
                }
                "Ersetzen" => {
                    form.search = true;
                    form.replace = value.into();
                }
                "Optionen" => {
                    form.search = true;
                    let flags: Vec<_> = value.split(',').collect();
                    form.regex = flags.contains(&"regex");
                    form.wildcards = flags.contains(&"platzhalter");
                    form.match_case = flags.contains(&"schreibweise");
                    form.first_only = flags.contains(&"erstes");
                    form.include_extension = flags.contains(&"erweiterung");
                    form.substitute = flags.contains(&"substitution");
                    form.auto_number = flags.contains(&"autonummer");
                    for flag in flags {
                        if let Some((key, value)) = flag.split_once('=') {
                            match key {
                                "start" => form.counter_start = value.into(),
                                "step" => form.counter_step = value.into(),
                                "width" => form.counter_width = value.into(),
                                "every" => form.counter_every = value.into(),
                                "case" => form.case = value.parse().unwrap_or_default(),
                                _ => (),
                            }
                        }
                    }
                }
                _ => (),
            }
        }
    }
    form
}
fn rename_text(form: &TextDraftForm) -> io::Result<String> {
    let counter = arco_core::rename::CounterSettings {
        start: form.counter_start.to_string(),
        step: form.counter_step.to_string(),
        width: form.counter_width.to_string(),
        every: form.counter_every.to_string(),
    };
    counter.pattern()?;
    arco_core::rename::NameCase::from_index(form.case)?;
    if !form.search && (counter != Default::default() || form.case != 0 || form.auto_number) {
        let search = arco_core::rename::SearchOptions::from_legacy(&form.second)?;
        let mut actual = form.clone();
        actual.search = true;
        actual.find = search.find.into();
        actual.replace = search.replace.into();
        actual.regex = search.regex;
        actual.match_case = search.match_case;
        actual.include_extension = search.include_extension;
        return rename_text(&actual);
    }
    for text in [
        &form.name,
        &form.body,
        &form.second,
        &form.extension,
        &form.find,
        &form.replace,
    ] {
        one_line(text)?;
    }
    let mut text = format!(
        "{}\n{}\n{}",
        form.name,
        form.body,
        if form.search { "" } else { &form.second }
    );
    if form.separate {
        text.push_str(&format!("\nErweiterung={}", form.extension));
    }
    if form.search {
        let flags = [
            (form.regex, "regex"),
            (form.wildcards, "platzhalter"),
            (form.match_case, "schreibweise"),
            (form.first_only, "erstes"),
            (form.include_extension, "erweiterung"),
            (form.substitute, "substitution"),
            (form.auto_number, "autonummer"),
        ]
        .into_iter()
        .filter_map(|(yes, flag)| yes.then_some(flag))
        .collect::<Vec<_>>()
        .join(",");
        let flags = super::rename_setup::counter_flags(&flags, &counter, form.case);
        text.push_str(&format!(
            "\nSuchen={}\nErsetzen={}\nOptionen={flags}",
            form.find, form.replace
        ));
    }
    Ok(text)
}

pub(super) fn clone_pending(p: &Pending) -> Option<Pending> {
    Some(match p {
        Pending::WdxChoose(p, f) => Pending::WdxChoose(p.clone(), f.clone()),
        Pending::WdxEditPlugin(p, f) => Pending::WdxEditPlugin(p.clone(), f.clone()),
        Pending::WdxEditField(t, f) => Pending::WdxEditField(t.clone(), f.clone()),
        Pending::WdxEditMode(t, f) => Pending::WdxEditMode(t.clone(), f.clone()),
        Pending::WdxEditValue(t, f) => Pending::WdxEditValue(t.clone(), f.clone()),
        Pending::WdxEditApply(t, f, i) => Pending::WdxEditApply(t.clone(), f.clone(), i.clone()),
        Pending::WdxSearchPlugin(t, p) => Pending::WdxSearchPlugin(t.clone(), p.clone()),
        Pending::WdxSearchField(t, p, f) => {
            Pending::WdxSearchField(t.clone(), p.clone(), f.clone())
        }
        Pending::WdxSearchCriteria(t, p, f) => {
            Pending::WdxSearchCriteria(t.clone(), p.clone(), f.clone())
        }
        Pending::WdxSearchReview(t) => Pending::WdxSearchReview(t.clone()),
        Pending::WdxSearchSave(t) => Pending::WdxSearchSave(t.clone()),
        Pending::WdxSearchReplace(t, n) => Pending::WdxSearchReplace(t.clone(), n.clone()),
        Pending::WdxSearchProfiles(n) => Pending::WdxSearchProfiles(n.clone()),
        Pending::WdxSearchProfileAction(n) => Pending::WdxSearchProfileAction(n.clone()),
        Pending::WdxSearchProfileDelete(n) => Pending::WdxSearchProfileDelete(n.clone()),
        Pending::WdxRemove(c) => Pending::WdxRemove(c.clone()),
        Pending::KnownFolder => Pending::KnownFolder,
        Pending::VirtualShellFolder => Pending::VirtualShellFolder,
        Pending::QuickFind => Pending::QuickFind,
        Pending::TabFileFilter(s, r) => Pending::TabFileFilter(*s, *r),
        Pending::SelectionFile(p, paths) => Pending::SelectionFile(p.clone(), paths.clone()),
        Pending::EditComment(p, c) => Pending::EditComment(p.clone(), c.clone()),
        Pending::PrintDirectory(p, paths) => Pending::PrintDirectory(p.clone(), paths.clone()),
        Pending::PrintText(t, b) => Pending::PrintText(t.clone(), b.clone()),
        Pending::Create(p, d) => Pending::Create(p.clone(), *d),
        Pending::ShellName(p, a, b) => Pending::ShellName(p.clone(), *a, *b),
        Pending::Mask(m) => Pending::Mask(match m {
            crate::commands::MaskEdit::Replace => crate::commands::MaskEdit::Replace,
            crate::commands::MaskEdit::Add { folders } => {
                crate::commands::MaskEdit::Add { folders: *folders }
            }
            crate::commands::MaskEdit::Remove { folders } => {
                crate::commands::MaskEdit::Remove { folders: *folders }
            }
        }),
        Pending::IgnoreList => Pending::IgnoreList,
        Pending::IgnoreImport => Pending::IgnoreImport,
        Pending::ImportTc => Pending::ImportTc,
        Pending::PackerProfiles(n) => Pending::PackerProfiles(n.clone()),
        Pending::PackerProfileAction(n) => Pending::PackerProfileAction(n.clone()),
        Pending::PackerProfileEdit(n) => Pending::PackerProfileEdit(n.clone()),
        Pending::RenameSetup(setup) => Pending::RenameSetup(setup.retry_clone()?),
        Pending::Administration(setup) => Pending::Administration(setup.clone()),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::Model;
    #[test]
    fn background_notices_wait_for_all_modal_surfaces() {
        assert!(!busy("", false, -1, false));
        for kind in ["input", "text-draft", "configuration", "message"] {
            assert!(busy(kind, false, -1, false));
        }
        assert!(busy("", true, -1, false));
        assert!(busy("", false, 0, false));
        assert!(busy("", false, -1, true));
        let mut notices = Notices::default();
        notices.waiting.push_back(Notice::error("A", "First"));
        notices.waiting.push_back(Notice::info("B", "Second"));
        assert_eq!(notices.waiting.pop_front().unwrap().details, "First");
        assert_eq!(notices.waiting.pop_front().unwrap().details, "Second");
    }
    #[test]
    fn transfer_outcomes_distinguish_success_partial_failure_and_cancellation() {
        let mut report = TransferReport::default();
        report.completed = 3;
        report.skipped = 2;
        assert_eq!(
            Notice::transfer("Copy", &report).level,
            MessageLevel::Success
        );
        report.errors.push("A: denied".into());
        let notice = Notice::transfer("Copy", &report);
        assert_eq!(notice.level, MessageLevel::Warning);
        assert!(notice.copy_text().contains("A: denied"));
        assert!(notice.copy_text().contains("3"));
        report.errors.clear();
        report.cancelled = true;
        assert_eq!(
            Notice::transfer("Copy", &report).level,
            MessageLevel::Warning
        );
    }
    #[test]
    fn rename_template_form_retains_separate_masks_flags_and_legacy_rules() {
        for source in [
            "Template\n[N]\nold=>new",
            "Template\n[N]-[C]\n\nErweiterung=[E]\nSuchen=(a)\nErsetzen=$1\nOptionen=regex,schreibweise,erstes,erweiterung,substitution",
            "Template\n[N]-[C]\n\nSuchen=\nErsetzen=\nOptionen=start=5,step=2,width=3,every=2,case=5",
            "Template\n[N]\n\nSuchen=\nErsetzen=\nOptionen=autonummer",
        ] {
            let form = rename_form(source);
            assert_eq!(rename_text(&form).unwrap(), source);
        }
        let mut form = rename_form("Template\n[N]\n");
        form.name = "bad\n[N]".into();
        assert!(rename_text(&form).is_err());
    }
    #[test]
    fn confirmation_and_print_previews_keep_options_and_full_copyable_details() {
        let body = "A\nB\nC";
        let data = review_data(
            "review",
            &Dialog {
                title: "Import",
                description: "Check",
                value: body,
                options: &[crate::locale::tr("Append"), crate::locale::tr("Replace")],
                confirm_label: "Apply",
                ..Default::default()
            },
        );
        assert_eq!(data.details, body);
        assert_eq!(data.details_lines, 3);
        assert_eq!(data.options.row_count(), 2);
        assert_eq!(data.options.row_data(1).unwrap(), "Replace");
        assert_eq!(data.confirm, "Apply");
        assert!(!data.destructive);
        let deletion = review_data(
            "delete",
            &Dialog {
                value: body,
                ..Default::default()
            },
        );
        assert!(deletion.destructive);
        assert_eq!(
            review_data("print-preview", &Dialog::default()).confirm,
            tr("Drucken …")
        );
    }
    #[test]
    fn copied_notices_preserve_long_details_and_error_severity() {
        let body = "A very long detail\n".repeat(10_000);
        let notice = Notice::error("Failure", &body);
        let text = notice.copy_text();
        assert!(text.ends_with(&body));
        assert_eq!(notice.data().level, MessageLevel::Error);
        assert_eq!(notice.data().details.len(), body.len());
    }
    #[test]
    fn dialog_results_wait_without_deferring_regular_background_updates() {
        let print = super::super::Outcome::PrintPreview("Title".into(), "Document".into());
        assert!(result_waits(&print, "configuration"));
        assert!(result_waits(&print, "text-draft"));
        assert!(!result_waits(&print, ""));
        let status = super::super::Outcome::Notice("Done".into());
        assert!(!result_waits(&status, "configuration"));
        let refresh = super::super::Outcome::PluginsUpdated(0, None);
        assert!(!result_waits(&refresh, "plugins"));
        assert!(result_waits(&refresh, "input"));
    }
}
