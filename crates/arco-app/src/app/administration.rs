use super::{App, Outcome};
use crate::commands::{Dialog, Pending};
use crate::display_path;
use arco_core::files::{self, Kind};
use arco_windows::{LocalShare, ShareAccess, Volume};
use std::io;
use std::path::PathBuf;

pub(crate) enum Setup {
    Shares(Vec<LocalShare>),
    Create(PathBuf),
    Remove(LocalShare),
    Volume(Volume),
}

impl App {
    pub(crate) fn administration_action(&mut self, command: &str) -> bool {
        match command {
            "shares" => self.start_job("Lese lokale Windows-Freigaben …", false, move |_| {
                Ok(Outcome::LocalShares(arco_windows::local_shares()?))
            }),
            "share-create" | "volume-label" => {
                if !self.current().is_local() || self.panels[self.active].loading {
                    self.status("Wähle einen lokalen Ordner.");
                    return true;
                }
                let directory = self.current().path.clone();
                if command == "volume-label" {
                    self.start_job("Lese Datenträgerbezeichnung …", false, move |_| {
                        Ok(Outcome::VolumeLabel(arco_windows::volume(&directory)?))
                    });
                } else {
                    let path = self
                        .current()
                        .current()
                        .filter(|entry| entry.kind == Kind::Directory)
                        .map_or(directory, |entry| entry.path.clone());
                    self.create_share_dialog(path);
                }
            }
            _ => return false,
        }
        true
    }

    fn create_share_dialog(&mut self, path: PathBuf) {
        let label = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let description = format!(
            "Ordner: {}\nLeerlassen gewährt Zugriff für dein aktuelles Konto. In einer Liste kannst du Rechte je Eintrag angeben, etwa PC\\Benutzer=Lesen. Windows prüft zusätzlich die Rechte des Ordners.",
            display_path(&path)
        );
        self.dialog(
            "input",
            Pending::Administration(Setup::Create(path)),
            Dialog {
                title: "Windows-Freigabe erstellen",
                description: &description,
                value: &label,
                placeholder: "Freigabename",
                second_placeholder: "Konten und Gruppen, durch Semikolon getrennt (optional)",
                options: &["Nur lesen", "Ändern", "Vollzugriff"],
                ..Default::default()
            },
        );
    }

    pub(super) fn show_shares(&mut self, shares: Vec<LocalShare>) {
        let labels: Vec<_> = shares
            .iter()
            .map(|share| {
                let kind = if share.special {
                    " · Systemfreigabe"
                } else {
                    ""
                };
                format!("{} → {}{kind}", share.name, display_path(&share.path))
            })
            .collect();
        let options: Vec<_> = std::iter::once("Ordner freigeben …")
            .chain(labels.iter().map(String::as_str))
            .collect();
        self.dialog(
            "confirm",
            Pending::Administration(Setup::Shares(shares)),
            Dialog {
                title: "Lokale Windows-Freigaben",
                description: "Wähle eine Freigabe, um ihre Entfernung zu bestätigen.",
                options: &options,
                ..Default::default()
            },
        );
    }

    pub(super) fn show_volume_label(&mut self, volume: Volume) {
        let root = display_path(&volume.root);
        let label = volume.label.clone();
        self.dialog(
            "input",
            Pending::Administration(Setup::Volume(volume)),
            Dialog {
                title: "Datenträgerbezeichnung ändern",
                description: &root,
                value: &label,
                placeholder: "Bezeichnung. Ein leeres Feld entfernt sie.",
                ..Default::default()
            },
        );
    }

    pub(crate) fn submit_administration(
        &mut self,
        setup: Setup,
        value: &str,
        second: &str,
        option: usize,
    ) -> io::Result<()> {
        match setup {
            Setup::Shares(_) if option == 0 => {
                self.administration_action("share-create");
            }
            Setup::Shares(mut shares) => {
                if option <= shares.len() {
                    let share = shares.remove(option - 1);
                    let description = format!(
                        "{} → {}\nGib den Freigabenamen ein, um die Freigabe zu entfernen.",
                        share.name,
                        display_path(&share.path)
                    );
                    self.dialog(
                        "input",
                        Pending::Administration(Setup::Remove(share)),
                        Dialog {
                            title: "Windows-Freigabe entfernen",
                            description: &description,
                            placeholder: "Freigabename",
                            ..Default::default()
                        },
                    );
                }
            }
            Setup::Create(directory) => {
                let name = value.trim().to_owned();
                let accounts = second.trim().to_owned();
                let access = [ShareAccess::Read, ShareAccess::Change, ShareAccess::Full]
                    .get(option)
                    .copied()
                    .ok_or_else(|| io::Error::other("Wähle die Freigaberechte."))?;
                self.start_job("Erstelle Windows-Freigabe …", false, move |job| {
                    files::check_cancel(&job.cancel)?;
                    arco_windows::create_share(&directory, &name, access, &accounts)?;
                    Ok(Outcome::LocalShares(arco_windows::local_shares()?))
                });
            }
            Setup::Remove(share) => {
                if value.trim() != share.name {
                    return Err(io::Error::other(
                        "Der eingegebene Freigabename stimmt nicht überein.",
                    ));
                }
                self.start_job("Entferne Windows-Freigabe …", false, move |job| {
                    files::check_cancel(&job.cancel)?;
                    arco_windows::remove_share(&share)?;
                    Ok(Outcome::LocalShares(arco_windows::local_shares()?))
                });
            }
            Setup::Volume(volume) => {
                let label = value.to_owned();
                self.start_job("Ändere Datenträgerbezeichnung …", true, move |job| {
                    files::check_cancel(&job.cancel)?;
                    arco_windows::rename_volume(&volume, &label)?;
                    Ok(Outcome::Notice("Datenträgerbezeichnung geändert.".into()))
                });
            }
        }
        Ok(())
    }
}
