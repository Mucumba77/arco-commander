use super::{App, Outcome};
use crate::commands::{Dialog, Pending};
use crate::display_path;
use arco_core::{packer_profile::PackerProfile, tools};
use std::{io, path::PathBuf};

impl App {
    pub(crate) fn open_packer_profiles(&mut self) {
        let names: Vec<_> = self.profile.packer_profiles.keys().cloned().collect();
        let options: Vec<_> = std::iter::once("Neues Profil")
            .chain(names.iter().map(String::as_str))
            .collect();
        self.dialog(
            "confirm", Pending::PackerProfiles(names.clone()),
            Dialog {
                title: "Externe Packerprofile",
                description: "Wähle ein Profil oder lege ein neues an. Zum Starten dient „Archive → Mit Profil packen“.",
                options: &options,
                ..Dialog::default()
            },
        );
    }

    pub(crate) fn choose_packer_profile(&mut self, names: Vec<String>, option: usize) {
        if option == 0 {
            self.edit_packer_profile(None, None, None);
        } else if let Some(name) = names.get(option - 1) {
            self.dialog(
                "confirm",
                Pending::PackerProfileAction(name.clone()),
                Dialog {
                    title: "Packerprofil verwalten",
                    description: name,
                    options: &["Bearbeiten", "Löschen"],
                    ..Dialog::default()
                },
            );
        }
    }

    pub(crate) fn edit_packer_profile(
        &mut self,
        previous: Option<String>,
        draft: Option<&str>,
        error: Option<&str>,
    ) {
        let value = draft.map(str::to_owned).or_else(|| {
            let name = previous.as_ref()?;
            let packer = self.profile.packer_profiles.get(name)?;
            Some(format!("{name}\n{}\n{}\n{}", packer.executable.display(), packer.extension, packer.arguments.join("\n")))
        }).unwrap_or_else(|| "7-Zip ZIP\nC:\\Program Files\\7-Zip\\7z.exe\nzip\na\n-tzip\n-spd\n{archive}\n{sources}".into());
        self.dialog(
            "multiline", Pending::PackerProfileEdit(previous),
            Dialog {
                title: "Packerprofil bearbeiten",
                description: error.unwrap_or(
                    "Je eine Zeile für Profilname, EXE-Pfad und Archivendung. Danach steht jedes Argument auf einer eigenen Zeile. Verwende {archive} und {sources} oder @{list}.",
                ),
                value: &value,
                ..Dialog::default()
            },
        );
    }

    pub(crate) fn save_packer_profile(
        &mut self,
        previous: Option<&str>,
        value: &str,
    ) -> io::Result<()> {
        let mut lines = value.lines().map(str::trim);
        let name = lines.next().unwrap_or_default();
        if name.is_empty() || name.chars().count() > 80 {
            return Err(io::Error::other(
                "Der Profilname muss 1 bis 80 Zeichen enthalten.",
            ));
        }
        let executable = PathBuf::from(lines.next().unwrap_or_default().trim_matches('"'));
        let extension = lines
            .next()
            .unwrap_or_default()
            .trim_start_matches('.')
            .to_ascii_lowercase();
        let arguments = lines
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect();
        let profile = PackerProfile {
            executable,
            extension,
            arguments,
        };
        profile.validate()?;
        let mut updated = self.profile_snapshot();
        if let Some(previous) = previous {
            updated.packer_profiles.remove(previous);
        }
        if updated
            .packer_profiles
            .keys()
            .any(|key| key.to_lowercase() == name.to_lowercase())
        {
            return Err(io::Error::other("Dieser Profilname ist bereits vergeben."));
        }
        updated.packer_profiles.insert(name.to_owned(), profile);
        self.save_packer_profiles(updated)
    }

    pub(crate) fn remove_packer_profile(&mut self, name: &str) -> io::Result<()> {
        let mut updated = self.profile_snapshot();
        updated.packer_profiles.remove(name);
        self.save_packer_profiles(updated)
    }

    fn save_packer_profiles(&mut self, updated: arco_core::profile::Profile) -> io::Result<()> {
        if !self.save_profile {
            return Err(io::Error::other("Das Arco-Profil ist nicht verfügbar."));
        }
        updated.save(&self.profile_path)?;
        self.profile = updated;
        self.status("Packerprofile gespeichert.");
        Ok(())
    }

    pub(crate) fn external_pack_dialog(&mut self) {
        let paths = self.current().paths();
        if paths.is_empty() {
            self.status("Wähle Dateien oder Ordner.");
            return;
        }
        let names: Vec<_> = self.profile.packer_profiles.keys().cloned().collect();
        if names.is_empty() {
            self.status("Lege unter Konfigurieren ein externes Packerprofil an.");
            return;
        }
        let options: Vec<_> = names.iter().map(String::as_str).collect();
        let target = self.target().to_path_buf();
        let description = format!(
            "{} · {}",
            crate::locale::tr("Ausgewählte Einträge"),
            paths.len()
        );
        self.prepare_operation_dialog(&paths, &display_path(&target), false, false);
        self.ui().set_dialog_operation_note(
            crate::locale::tr("Die Dateiendung ergibt sich aus dem gewählten Packerprofil.").into(),
        );
        self.dialog(
            "archive-pack-profile",
            Pending::ExternalPack(paths, self.current().path.clone(), target, names.clone()),
            Dialog {
                title: "Mit Packerprofil packen",
                description: &description,
                placeholder: "Archivname",
                value: "archiv",
                options: &options,
                options_label: "Packerprofil",
                confirm_label: "Packen",
                ..Dialog::default()
            },
        );
    }

    pub(crate) fn run_external_packer(
        &mut self,
        paths: Vec<PathBuf>,
        base: PathBuf,
        directory: PathBuf,
        profile: &str,
        value: &str,
    ) -> io::Result<()> {
        let profile = self
            .profile
            .packer_profiles
            .get(profile)
            .cloned()
            .ok_or_else(|| io::Error::other("Das Packerprofil fehlt."))?;
        let value = value.trim();
        let suffix = format!(".{}", profile.extension);
        let name = tools::output_name(value)?;
        let target = if value.to_ascii_lowercase().ends_with(&suffix) {
            directory.join(name)
        } else {
            directory.join(tools::output_name(&format!("{value}{suffix}"))?)
        };
        self.start_operation_job(
            "Packe mit externem Profil …",
            true,
            super::JobDescription::files(crate::OperationKind::Pack, &paths, &target),
            move |job| {
                profile.pack(&paths, &base, &target, &job.cancel)?;
                Ok(Outcome::Notice(format!(
                    "Erstellt: {}",
                    display_path(&target)
                )))
            },
        );
        Ok(())
    }
}
