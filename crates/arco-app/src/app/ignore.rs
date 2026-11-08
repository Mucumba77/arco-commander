use super::{App, Outcome};
use crate::commands::{Dialog, Pending};
use arco_core::ignore::IgnoreList;
use std::{io, path::Path, sync::Arc};

impl App {
    pub(crate) fn edit_ignore_list(&mut self) {
        let rules = self.profile.ignore_rules.clone();
        self.edit_ignore_rules(&rules, None);
    }

    pub(crate) fn edit_ignore_rules(&mut self, rules: &str, notice: Option<&str>) {
        self.dialog("multiline", Pending::IgnoreList, Dialog {
            title: "Ignorierliste",
                description: notice.unwrap_or(if self.profile.ignore_enabled {
                "Aktiv. Je Zeile ein Pfad oder mehrere Dateimasken, etwa *.bak *.old. Ein abschließender Backslash gilt nur für Ordner."
            } else {
                "Inaktiv. Zum Einschalten im Menü Ansicht auf „Ignorierliste umschalten“ klicken. Je Zeile ein Pfad oder mehrere Dateimasken."
                }),
                value: rules,
            ..Dialog::default()
        });
    }

    pub(crate) fn import_ignore_list(&mut self) {
        self.dialog(
            "input",
            Pending::IgnoreImport,
            Dialog {
                title: "Ignorierliste importieren",
                placeholder: "Pfad zu tcignore.txt",
                description: "Du kannst die importierten Regeln vor dem Speichern bearbeiten.",
                ..Dialog::default()
            },
        );
    }

    pub(crate) fn read_ignore_list(&mut self, value: &str) -> io::Result<()> {
        let value = value.trim().trim_matches('"');
        if value.is_empty() {
            return Err(io::Error::other("Gib den Pfad zur Ignorierliste ein."));
        }
        let path = self.current().path.join(Path::new(value));
        self.start_job("Lese Ignorierliste …", false, move |_| {
            Ok(Outcome::IgnoreImported(IgnoreList::read_tc(&path)?))
        });
        Ok(())
    }

    pub(crate) fn toggle_ignore_list(&mut self) -> io::Result<()> {
        self.set_ignore_enabled(!self.profile.ignore_enabled)
    }

    pub(crate) fn set_ignore_enabled(&mut self, enabled: bool) -> io::Result<()> {
        self.save_ignore_list(&self.profile.ignore_rules.clone(), enabled)
    }

    pub(crate) fn save_ignore_list(&mut self, rules: &str, enabled: bool) -> io::Result<()> {
        let parsed = IgnoreList::parse(rules)?;
        if !self.save_profile {
            return Err(io::Error::other("Das Arco-Profil ist nicht verfügbar."));
        }
        let mut updated = self.profile_snapshot();
        updated.ignore_enabled = enabled;
        updated.ignore_rules = rules.to_owned();
        updated.save(&self.profile_path)?;
        self.profile = updated;
        self.ignore = enabled.then(|| Arc::new(parsed));
        for panel in &mut self.panels {
            for tab in &mut panel.tabs {
                tab.update_ignore(self.ignore.clone(), self.profile.show_hidden);
            }
        }
        self.publish(0);
        self.publish(1);
        self.invalidate_tree(0);
        self.invalidate_tree(1);
        self.status(if enabled {
            "Ignorierliste ist aktiv."
        } else {
            "Ignorierliste ist inaktiv."
        });
        Ok(())
    }

    pub(crate) fn ignore_list(&self) -> Option<Arc<IgnoreList>> {
        self.ignore.clone()
    }
}
