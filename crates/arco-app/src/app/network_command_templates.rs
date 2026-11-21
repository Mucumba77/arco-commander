use super::App;
use crate::commands::{Dialog, Pending};
use arco_app::network_commands;
use arco_app::network_sync::FtpEndpoint;
use std::io;
use std::sync::Arc;

impl App {
    pub(crate) fn ssh_command_selection(
        &self,
        endpoint: &FtpEndpoint,
    ) -> (Vec<String>, Option<String>) {
        [self.active, 1 - self.active]
            .into_iter()
            .find_map(|side| {
                let tab = self.panels[side].current();
                let ftp = tab.ftp.as_ref()?;
                if !Arc::ptr_eq(&ftp.location.session, &endpoint.0)
                    || ftp.location.directory != endpoint.1
                {
                    return None;
                }
                let selected = tab
                    .selected_paths()
                    .iter()
                    .filter_map(|path| ftp.entry(path).map(|entry| entry.name.clone()))
                    .collect();
                Some((selected, ftp.location.profile.clone()))
            })
            .unwrap_or_default()
    }

    pub(crate) fn open_network_command_templates(&mut self) {
        let names: Vec<_> = self
            .profile
            .network_command_templates
            .keys()
            .cloned()
            .collect();
        let options: Vec<_> = std::iter::once("Neue Befehlsvorlage")
            .chain(names.iter().map(String::as_str))
            .collect();
        self.dialog(
            "confirm",
            Pending::SshTemplates(names.clone()),
            Dialog {
                title: "SSH-Befehlsvorlagen",
                options: &options,
                confirm_label: "Öffnen",
                ..Default::default()
            },
        );
    }

    pub(crate) fn open_network_command_template(&mut self, name: Option<String>) {
        let existing = name
            .as_ref()
            .and_then(|name| self.profile.network_command_templates.get(name));
        let command = existing.cloned().unwrap_or_default();
        let label = name.clone().unwrap_or_default();
        let actions: &[&str] = if existing.is_some() {
            &["Ausführen", "Speichern", "Entfernen"]
        } else {
            &["Speichern"]
        };
        self.dialog("input", Pending::SshTemplate(name), Dialog {
            title: "SSH-Befehlsvorlage",
            value: &label,
            second: &command,
            placeholder: "Name der Vorlage",
            second_placeholder: "Programm und Argumente",
            description: "Vorlagen verwenden ! für einen Eintrag, !& für die Auswahl und !/ für den Serverordner. Sitzungsdaten liefern !@, !U, !#, !N und !S.",
            options: actions,
            options_label: "Aktion",
            confirm_label: "Übernehmen",
            ..Default::default()
        });
    }

    pub(crate) fn edit_network_command_template(
        &mut self,
        old: Option<String>,
        name: &str,
        command: &str,
        action: usize,
    ) -> io::Result<()> {
        if old.is_some() && action == 0 {
            let endpoint = self
                .ftp_endpoint()
                .filter(|endpoint| endpoint.0.security().is_ssh())
                .ok_or_else(|| io::Error::other("Öffne einen SFTP- oder SCP-Ordner."))?;
            let (selected, profile) = self.ssh_command_selection(&endpoint);
            return self.execute(
                Pending::FtpServerCommand(endpoint, selected, profile),
                command,
                "",
                1,
                0,
                "",
            );
        }
        if let Some(name) = old.as_ref()
            && action == 2
        {
            let name = name.clone();
            self.dialog(
                "confirm",
                Pending::SshTemplateRemove(name.clone()),
                Dialog {
                    title: "SSH-Befehlsvorlage entfernen",
                    description: &name,
                    confirm_label: "Entfernen",
                    ..Default::default()
                },
            );
            return Ok(());
        }
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
            return Err(io::Error::other(
                "Gib einen Namen mit 1 bis 80 Zeichen für die Vorlage ein.",
            ));
        }
        network_commands::validate(command)?;
        let mut templates = self.profile.network_command_templates.clone();
        if old.as_deref() != Some(name) && templates.contains_key(name) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Dieser Vorlagenname ist bereits vergeben.",
            ));
        }
        if let Some(old) = old {
            templates.remove(&old);
        }
        templates.insert(name.to_owned(), command.to_owned());
        self.save_network_command_templates(templates)
    }

    pub(crate) fn remove_network_command_template(&mut self, name: &str) -> io::Result<()> {
        let mut templates = self.profile.network_command_templates.clone();
        templates.remove(name);
        self.save_network_command_templates(templates)
    }

    fn save_network_command_templates(
        &mut self,
        templates: std::collections::BTreeMap<String, String>,
    ) -> io::Result<()> {
        if self.save_profile {
            let mut profile = self.profile_snapshot();
            profile.network_command_templates = templates.clone();
            profile.save(&self.profile_path)?;
        }
        self.profile.network_command_templates = templates;
        self.open_network_command_templates();
        Ok(())
    }
}
