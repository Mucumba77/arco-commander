use super::{App, FtpEndpoint, JobDescription, Outcome};
use crate::commands::{Dialog, Pending};
use crate::panel::{FtpLocation, Location};
use crate::{OperationKind, display_path};
use arco_app::{network_crypto, network_transfer};
use arco_core::{encrypted_file, files};
use arco_windows::ftp;
use std::io;
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

impl App {
    pub(crate) fn encrypted_upload_dialog(&mut self) -> io::Result<()> {
        let paths = self.current().paths();
        let [source] = paths.as_slice() else {
            return Err(io::Error::other("Wähle eine einzelne lokale Datei."));
        };
        if !files::regular_metadata(source)?.is_file() {
            return Err(io::Error::other("Wähle eine einzelne lokale Datei."));
        }
        let Location::Ftp(remote) = self.panels[1 - self.active].current().location() else {
            return Err(io::Error::other(
                "Öffne im Gegenfenster den Zielordner auf dem Server.",
            ));
        };
        let detail = format!(
            "{}\n→ {}",
            display_path(source),
            ftp::Session::directory_path(&remote.directory)
        );
        self.dialog(
            "input",
            Pending::EncryptedUpload(source.clone(), remote),
            Dialog {
                title: "Datei verschlüsselt hochladen",
                description: &detail,
                placeholder: "Passphrase (mindestens zwölf Zeichen)",
                second_placeholder: "Passphrase wiederholen",
                confirm_label: "Verschlüsseln und hochladen",
                secret: true,
                second_secret: true,
                ..Dialog::default()
            },
        );
        Ok(())
    }

    pub(crate) fn encrypted_download_dialog(&mut self) -> io::Result<()> {
        let tab = self.current();
        let paths = tab.selected_paths();
        let Some((remote, entry)) = tab.ftp.as_ref().and_then(|remote| {
            let [path] = paths.as_slice() else {
                return None;
            };
            let entry = remote.entry(path)?;
            (entry.directory == Some(false) && entry.size.is_some()).then_some((remote, entry))
        }) else {
            return Err(io::Error::other(
                "Wähle ein verschlüsseltes Dateipaket im Serverfenster.",
            ));
        };
        let entry = entry.clone();
        let remote = remote.location.clone();
        let Location::Local(folder) = self.panels[1 - self.active].current().location() else {
            return Err(io::Error::other(
                "Öffne im Gegenfenster den lokalen Zielordner.",
            ));
        };
        let detail = format!("{}\n→ {}", entry.name, display_path(&folder));
        self.dialog(
            "input",
            Pending::EncryptedDownload(remote, entry, folder),
            Dialog {
                title: "Datei entschlüsselt herunterladen",
                description: &detail,
                placeholder: "Passphrase",
                confirm_label: "Herunterladen und entschlüsseln",
                secret: true,
                ..Dialog::default()
            },
        );
        Ok(())
    }

    fn encrypted_endpoint(
        &self,
        remote: FtpLocation,
        local: Option<&Path>,
    ) -> io::Result<FtpEndpoint> {
        network_transfer::endpoint(
            &self.profile.network_transfer_presets,
            &(remote.session, remote.directory),
            local,
            None,
        )
    }

    pub(crate) fn start_encrypted_upload(
        &mut self,
        source: PathBuf,
        remote: FtpLocation,
        passphrase: &str,
        repeat: &str,
    ) -> io::Result<()> {
        encrypted_file::validate_passphrase(passphrase, true)?;
        if passphrase != repeat {
            return Err(io::Error::other("Die Passphrasen stimmen nicht überein."));
        }
        let password = Zeroizing::new(passphrase.to_owned());
        let endpoint = self.encrypted_endpoint(remote, source.parent())?;
        let title = "Datei verschlüsselt hochladen";
        let description = JobDescription::new(
            OperationKind::Ftp,
            display_path(&source),
            ftp::Session::directory_path(&endpoint.1),
        );
        self.enqueue_virtual_operation(title, true, description, move |job| {
            let published =
                network_crypto::upload(&endpoint, &source, &password, &job.cancel, |phase| {
                    job.phase(phase)
                })?;
            Ok(Outcome::Notice(format!(
                "{} · {}",
                crate::locale::tr("Verschlüsselt hochgeladen"),
                published.name
            )))
        });
        Ok(())
    }

    pub(crate) fn start_encrypted_download(
        &mut self,
        remote: FtpLocation,
        entry: ftp::Entry,
        folder: PathBuf,
        passphrase: &str,
    ) -> io::Result<()> {
        encrypted_file::validate_passphrase(passphrase, false)?;
        let password = Zeroizing::new(passphrase.to_owned());
        let endpoint = self.encrypted_endpoint(remote, Some(&folder))?;
        let title = "Datei entschlüsselt herunterladen";
        let description = JobDescription::new(
            OperationKind::Ftp,
            entry.name.clone(),
            display_path(&folder),
        );
        self.enqueue_virtual_operation(title, true, description, move |job| {
            let published = network_crypto::download(
                &endpoint,
                &entry,
                &folder,
                &password,
                &job.cancel,
                |phase| job.phase(phase),
            )?;
            Ok(Outcome::Notice(format!(
                "{} · {}",
                crate::locale::tr("Entschlüsselt gespeichert"),
                display_path(&published)
            )))
        });
        Ok(())
    }
}
