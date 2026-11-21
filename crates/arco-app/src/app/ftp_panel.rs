use super::ftp_sync::{MAX_DEPTH, MAX_ENTRIES, ftp_ignores, modified};
use super::{App, FtpEndpoint, LoadedDirectory, Navigation, Outcome, StagedPreview};
use crate::commands::{Dialog, Pending};
use crate::panel::{FtpLocation, FtpTab, Location};
use crate::{display_path, preview};
use arco_app::network_sync::{copy_names, scan_for_delete};
use arco_core::archive_view::MAX_PREVIEW;
use arco_core::files::{self, Directory, Kind, TransferReport};
use arco_core::profile::TabLock;
use arco_core::sync::Scope;
use arco_windows::ftp;
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) enum FtpEdit {
    Create,
    Rename(ftp::Entry),
    Delete(Vec<ftp::Entry>, bool),
    Modified(Vec<ftp::Entry>),
}

pub(crate) struct SearchHit {
    folder: ftp::Url,
    name: PathBuf,
    directory: bool,
}

pub(crate) struct S3History {
    endpoint: FtpEndpoint,
    name: String,
    versions: Vec<ftp::S3Version>,
    action: S3HistoryAction,
    mfa_delete: Option<bool>,
}

#[derive(Clone, Copy)]
pub(crate) enum S3DeleteMode {
    Standard,
    Choose,
    Mfa,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum S3HistoryAction {
    Download,
    Restore,
    Delete,
}

impl S3HistoryAction {
    fn title(self) -> &'static str {
        match self {
            Self::Download => "S3-Versionshistorie",
            Self::Restore => "S3-Version wiederherstellen",
            Self::Delete => "S3-Version löschen",
        }
    }

    fn can_select(self, version: &ftp::S3Version) -> bool {
        match self {
            Self::Download => version.size.is_some(),
            Self::Restore => !version.latest && version.size.is_some(),
            Self::Delete => version.can_delete(),
        }
    }
}

pub(super) fn search_remote(
    location: &FtpLocation,
    matches: impl Fn(&OsStr) -> bool,
    ignore: Option<&arco_core::ignore::IgnoreList>,
    cancel: &AtomicBool,
    mut progress: impl FnMut(&Path),
) -> io::Result<Vec<SearchHit>> {
    let ignored = ftp_ignores(ignore, &location.directory, location.profile.as_deref());
    let mut directories = vec![(PathBuf::new(), 0)];
    let mut visited = BTreeSet::new();
    let mut scanned = 0;
    let mut hits = Vec::new();
    while let Some((relative, depth)) = directories.pop() {
        files::check_cancel(cancel)?;
        if depth >= MAX_DEPTH {
            return Err(io::Error::other(
                "Der Serverordnerbaum ist für die rekursive Suche zu tief.",
            ));
        }
        progress(&relative);
        let folder = location
            .session
            .relative_directory(&location.directory, &relative)?;
        let listing = location.session.list(&folder, cancel)?;
        if listing.format == ftp::ListFormat::Names {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Der FTP-Server liefert keine Ordnertypen für die rekursive Suche.",
            ));
        }
        for entry in listing.entries {
            files::check_cancel(cancel)?;
            scanned += 1;
            if scanned > MAX_ENTRIES {
                return Err(io::Error::other(
                    "Der Serverordnerbaum enthält zu viele Einträge für die Suche.",
                ));
            }
            let Ok(name) = files::leaf_name(OsStr::new(&entry.name)) else {
                continue;
            };
            let path = relative.join(name);
            let directory = entry.directory == Some(true);
            if ignored(&path, directory) {
                continue;
            }
            if matches(name) {
                hits.push(SearchHit {
                    folder: folder.clone(),
                    name: name.into(),
                    directory,
                });
            }
            if directory && entry.unique.is_none_or(|id| visited.insert(id)) {
                directories.push((path, depth + 1));
            }
        }
    }
    hits.sort_by(|a, b| {
        a.folder
            .as_str()
            .cmp(b.folder.as_str())
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(hits)
}

pub(super) fn read_panel(
    location: &FtpLocation,
    cancel: &AtomicBool,
) -> io::Result<LoadedDirectory> {
    let listing = location.session.list(&location.directory, cancel)?;
    let mut directory = Directory {
        entries: Vec::new(),
        errors: Vec::new(),
    };
    let mut seen = BTreeSet::new();
    let mut reserved = listing
        .entries
        .iter()
        .filter(|entry| files::leaf_name(OsStr::new(&entry.name)).is_ok())
        .map(|entry| entry.name.to_lowercase())
        .collect::<BTreeSet<_>>();
    let mut name_aliases = std::collections::BTreeMap::new();
    let windows_names = arco_core::transfer_names::NameRules {
        windows: true,
        ..Default::default()
    };
    for entry in &listing.entries {
        if !seen.insert(&entry.name) {
            directory.errors.push(format!(
                "Ungültiger oder doppelter FTP-Name: {}",
                entry.name
            ));
            continue;
        }
        let path = match files::leaf_name(OsStr::new(&entry.name)) {
            Ok(name) => PathBuf::from(name),
            Err(_) => {
                let Ok(encoded) = windows_names.download(&entry.name) else {
                    directory.errors.push(format!(
                        "Ungültiger oder doppelter FTP-Name: {}",
                        entry.name
                    ));
                    continue;
                };
                let mut alias = encoded;
                while !reserved.insert(alias.to_lowercase()) {
                    alias.push_str("%00");
                }
                let path = PathBuf::from(alias);
                name_aliases.insert(path.clone(), entry.name.clone());
                path
            }
        };
        let kind = match (entry.directory, entry.symlink) {
            (_, true) => Kind::Link,
            (Some(true), false) => Kind::Directory,
            (Some(false), false) => Kind::File,
            (None, false) => Kind::Other,
        };
        let mut row = files::Entry::from_remote(
            path,
            kind,
            entry.size.filter(|_| kind == Kind::File),
            modified(entry).ok().flatten(),
        );
        row.name = entry.name.as_str().into();
        directory.entries.push(row);
    }
    Ok(LoadedDirectory {
        directory,
        space: None,
        archive: None,
        ftp: Some(FtpTab {
            location: location.clone(),
            listing,
            name_aliases,
        }),
        shell: Default::default(),
    })
}

impl App {
    pub(crate) fn open_remote_recycle_bin(&mut self) -> io::Result<()> {
        let location = self
            .current()
            .ftp
            .as_ref()
            .ok_or_else(|| io::Error::other("Öffne zuerst ein Netzwerk-Dateifenster."))?
            .location
            .clone();
        location.session.recycle_directory()?;
        let side = self.active;
        let request = self.panels[side].request;
        let detail = location.directory.to_string();
        self.start_operation_job(
            "Server-Papierkorb öffnen",
            false,
            super::JobDescription::new(crate::OperationKind::Ftp, detail, String::new()),
            move |job| {
                let directory = location.session.ensure_recycle_directory(&job.cancel)?;
                Ok(Outcome::FtpRecycleBin(
                    side,
                    request,
                    location.at(directory),
                ))
            },
        );
        Ok(())
    }

    pub(crate) fn restore_remote_recycle_dialog(&mut self) -> io::Result<()> {
        let tab = self.current();
        let ftp = tab
            .ftp
            .as_ref()
            .ok_or_else(|| io::Error::other("Öffne zuerst den Server-Papierkorb."))?;
        let location = ftp.location.clone();
        let bin = location.session.recycle_directory()?;
        let slot = if bin == location.directory {
            tab.display_row(tab.cursor)
                .and_then(|row| ftp.entry(&row.path))
                .filter(|entry| entry.directory == Some(true))
                .map(|entry| entry.name.clone())
        } else if location.session.parent_directory(&location.directory)? == bin {
            ftp::Session::directory_name(&location.directory)
        } else {
            None
        }
        .ok_or_else(|| io::Error::other("Wähle einen Eintrag im Server-Papierkorb."))?;
        let detail = format!("{} · {slot}", crate::locale::tr("Papierkorbeintrag"));
        self.dialog(
            "input",
            Pending::RestoreRemoteRecycle(location.session, slot),
            Dialog {
                title: "Servereintrag wiederherstellen",
                description: &detail,
                placeholder: "Zielordner relativ zum Netzwerk-Startordner oder vollständige URL",
                value: ".",
                confirm_label: "Wiederherstellen",
                ..Default::default()
            },
        );
        Ok(())
    }

    pub(crate) fn restore_remote_recycle_entry(
        &mut self,
        session: Arc<ftp::Session>,
        slot: String,
        destination: &str,
    ) -> io::Result<()> {
        let target = session.recycle_restore_destination(destination)?;
        let description =
            super::JobDescription::new(crate::OperationKind::Ftp, slot.clone(), target.to_string());
        self.start_operation_job(
            "Servereintrag wiederherstellen",
            true,
            description,
            move |job| {
                let entry = session.restore_recycled_entry(&slot, &target, &job.cancel)?;
                Ok(Outcome::Notice(format!(
                    "Wiederhergestellt · {}",
                    entry.name
                )))
            },
        );
        Ok(())
    }
    pub(crate) fn open_s3_history(&mut self, action: S3HistoryAction) {
        let tab = self.current();
        let Some(remote) = tab
            .ftp
            .as_ref()
            .filter(|tab| tab.location.session.security() == ftp::Security::S3)
        else {
            self.status("Öffne ein S3-Dateifenster.");
            return;
        };
        let name = tab
            .display_row(tab.cursor)
            .and_then(|row| remote.entry(&row.path))
            .filter(|entry| entry.directory == Some(false))
            .map_or_else(String::new, |entry| entry.name.clone());
        let endpoint = (
            remote.location.session.clone(),
            remote.location.directory.clone(),
        );
        self.dialog(
            "input",
            Pending::S3History(endpoint, action),
            Dialog {
                title: action.title(),
                value: &name,
                placeholder: "Exakter Objektname",
                confirm_label: "Versionen anzeigen",
                ..Default::default()
            },
        );
    }

    pub(crate) fn read_s3_history(
        &mut self,
        endpoint: FtpEndpoint,
        action: S3HistoryAction,
        name: &str,
    ) -> io::Result<()> {
        files::leaf_name(OsStr::new(name))?;
        let name = name.to_owned();
        let description = super::JobDescription::new(
            crate::OperationKind::Ftp,
            format!("{}{name}", endpoint.1),
            String::new(),
        );
        self.start_operation_job("S3-Versionen lesen", false, description, move |job| {
            let mut versions = endpoint
                .0
                .list_s3_versions(&endpoint.1, &name, &job.cancel)?;
            if action == S3HistoryAction::Restore {
                versions.retain(|version| !version.latest && version.size.is_some());
            }
            // Some roles may list and delete versions without reading bucket settings.
            let mfa_delete = if action == S3HistoryAction::Delete {
                endpoint
                    .0
                    .s3_bucket_versioning_details(&endpoint.1, &job.cancel)
                    .ok()
                    .map(|(_, config)| config.mfa_delete)
            } else {
                None
            };
            Ok(Outcome::S3History(Box::new(S3History {
                endpoint,
                name,
                versions,
                action,
                mfa_delete,
            })))
        });
        Ok(())
    }

    pub(super) fn show_s3_history(&mut self, history: Box<S3History>) {
        use crate::locale::tr;
        let rows = history
            .versions
            .iter()
            .map(|version| {
                let state = match (version.latest, version.size) {
                    (true, None) => tr("Aktuelle Löschmarkierung").into(),
                    (false, None) => tr("Löschmarkierung").into(),
                    (true, Some(size)) => {
                        format!("{} · {}", tr("Aktuell"), crate::human_size(size))
                    }
                    (false, Some(size)) => crate::human_size(size),
                };
                crate::ReviewRow {
                    primary: format!("{} · {state}", version.modified).into(),
                    secondary: version.id.as_str().into(),
                    kind: if version.size.is_some() {
                        "file"
                    } else {
                        "delete"
                    }
                    .into(),
                    ..Default::default()
                }
            })
            .collect();
        let action = history.action;
        self.management_review(
            crate::OperationKind::Ftp,
            rows,
            ["Stand", "Versions-ID"],
            match action {
                S3HistoryAction::Download => "Version herunterladen",
                _ => "Weiter",
            },
        );
        let local = self.panels[1 - self.active].current().is_local();
        self.ui().set_review_can_confirm(
            (local || action != S3HistoryAction::Download)
                && history
                    .versions
                    .iter()
                    .any(|version| action.can_select(version)),
        );
        let status = tr(match action {
            S3HistoryAction::Restore if history.versions.is_empty() => {
                "Keine ältere Dateiversion gefunden."
            }
            _ if history.versions.is_empty() => "Keine Objektversionen gefunden.",
            S3HistoryAction::Restore => {
                "Wähle die Version, die als neuer Stand veröffentlicht werden soll."
            }
            S3HistoryAction::Delete => {
                "Wähle eine Dateiversion oder Löschmarkierung mit unveränderlicher Versions-ID."
            }
            S3HistoryAction::Download if !local => {
                "Öffne im anderen Dateifenster einen lokalen Zielordner zum Herunterladen."
            }
            S3HistoryAction::Download => "Wähle eine Dateiversion zum Herunterladen.",
        });
        let description = format!("{} · {status}", history.name);
        let title = history.action.title();
        self.dialog(
            "management-review",
            Pending::S3VersionSelect(history),
            Dialog {
                title,
                description: &description,
                ..Default::default()
            },
        );
    }

    pub(crate) fn choose_s3_version(
        &mut self,
        mut history: S3History,
        index: usize,
    ) -> io::Result<()> {
        if !history
            .versions
            .get(index)
            .is_some_and(|version| history.action.can_select(version))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                if history.action == S3HistoryAction::Delete {
                    "Wähle eine unveränderliche S3-Versions-ID."
                } else {
                    "Wähle eine S3-Dateiversion."
                },
            ));
        }
        let version = history.versions.remove(index);
        if history.action == S3HistoryAction::Delete {
            let mode = match history.mfa_delete {
                Some(true) => S3DeleteMode::Mfa,
                Some(false) => S3DeleteMode::Standard,
                None => S3DeleteMode::Choose,
            };
            self.confirm_s3_delete((history.endpoint, history.name, version), mode)?;
            return Ok(());
        }
        if history.action == S3HistoryAction::Restore {
            let document = (history.endpoint, history.name, version);
            let description = format!(
                "{}\n{} · {}\n\n{}",
                document.1,
                document.2.modified,
                document.2.id,
                crate::locale::tr(
                    "Arco erstellt auf dem Server aus diesem Stand eine neue aktuelle Dateiversion."
                ),
            );
            self.dialog(
                "confirm",
                Pending::S3VersionRestore(Box::new(document)),
                Dialog {
                    title: history.action.title(),
                    description: &description,
                    confirm_label: "Wiederherstellen",
                    ..Default::default()
                },
            );
            return Ok(());
        }
        let target = self.panels[1 - self.active].current();
        if !target.is_local() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Öffne im anderen Dateifenster einen lokalen Zielordner zum Herunterladen.",
            ));
        }
        let description = format!("{} · {}", version.modified, display_path(&target.path));
        let name = history.name.clone();
        self.dialog(
            "input",
            Pending::S3VersionSave(Box::new((
                history.endpoint,
                history.name,
                version,
                target.path.clone(),
            ))),
            Dialog {
                title: "S3-Version herunterladen",
                description: &description,
                value: &name,
                placeholder: "Zielname",
                confirm_label: "Herunterladen",
                ..Default::default()
            },
        );
        Ok(())
    }

    pub(crate) fn restore_s3_version(
        &mut self,
        ((session, directory), name, version): (FtpEndpoint, String, ftp::S3Version),
    ) {
        let description = super::JobDescription::new(
            crate::OperationKind::Ftp,
            format!("{name} · {}", version.modified),
            format!("{directory}{name}"),
        );
        self.enqueue_virtual_operation(
            "S3-Version wiederherstellen",
            true,
            description,
            move |job| {
                job.phase("Dateiversion auf dem Server kopieren …");
                session.restore_s3_version_with_progress(
                    &directory,
                    &name,
                    &version,
                    &job.cancel,
                    job.ftp_progress(
                        name.clone(),
                        version.size,
                        "Dateiversion auf dem Server kopieren …",
                        "S3-Version prüfen …",
                    ),
                )?;
                Ok(Outcome::Report(
                    "S3-Version wiederhergestellt".into(),
                    TransferReport {
                        completed: 1,
                        ..Default::default()
                    },
                ))
            },
        );
    }

    pub(crate) fn confirm_s3_delete(
        &mut self,
        document: (FtpEndpoint, String, ftp::S3Version),
        mode: S3DeleteMode,
    ) -> io::Result<()> {
        use crate::locale::tr;
        let effect = if document.2.latest {
            "Das löscht genau diese aktuelle Version dauerhaft. Eine frühere Version kann dadurch sichtbar werden."
        } else {
            "Das löscht die gewählte Version dauerhaft aus der S3-Historie."
        };
        let mut description = format!(
            "{}\n{} · {}\n\n{}",
            document.1,
            document.2.modified,
            document.2.id,
            tr(effect)
        );
        let mut dialog = Dialog {
            title: "S3-Version löschen",
            confirm_label: "Endgültig löschen",
            ..Default::default()
        };
        let kind = match mode {
            S3DeleteMode::Standard => "confirm",
            S3DeleteMode::Choose => {
                description.push_str("\n\n");
                description.push_str(tr(
                    "Der MFA-Schutzstatus des Buckets konnte nicht gelesen werden.",
                ));
                dialog.options = &["Standardlöschung", "Löschen mit MFA-Code"];
                dialog.options_label = "Autorisierung";
                "confirm"
            }
            S3DeleteMode::Mfa => {
                ftp::S3Mfa::require_https(&document.0.1)?;
                dialog.placeholder = "MFA-Gerät (Seriennummer oder ARN)";
                dialog.second_placeholder = "Sechsstelliger MFA-Code";
                dialog.second_secret = true;
                "input"
            }
        };
        dialog.description = &description;
        self.dialog(
            kind,
            Pending::S3VersionDelete(Box::new(document), mode),
            dialog,
        );
        Ok(())
    }

    pub(crate) fn delete_s3_version(
        &mut self,
        ((session, directory), name, version): (FtpEndpoint, String, ftp::S3Version),
        mfa: Option<ftp::S3Mfa>,
    ) {
        let description = super::JobDescription::new(
            crate::OperationKind::Ftp,
            format!("{directory}{name} · {}", version.id),
            String::new(),
        );
        self.enqueue_virtual_operation("S3-Version löschen", true, description, move |job| {
            session.delete_s3_version_authorized(
                &directory,
                &name,
                &version,
                mfa.as_ref(),
                &job.cancel,
            )?;
            Ok(Outcome::Report(
                if version.size.is_none() {
                    "S3-Löschmarkierung entfernt"
                } else {
                    "S3-Dateiversion gelöscht"
                }
                .into(),
                TransferReport {
                    completed: 1,
                    ..Default::default()
                },
            ))
        });
    }

    pub(crate) fn open_s3_versioning(&mut self) {
        let Some(remote) = self
            .current()
            .ftp
            .as_ref()
            .filter(|tab| tab.location.session.security() == ftp::Security::S3)
        else {
            self.status("Öffne ein S3-Dateifenster.");
            return;
        };
        let endpoint = (
            remote.location.session.clone(),
            remote.location.directory.clone(),
        );
        let description = super::JobDescription::new(
            crate::OperationKind::Ftp,
            endpoint.1.to_string(),
            String::new(),
        );
        self.start_operation_job("S3-Versionierung lesen", false, description, move |job| {
            let (bucket, status) = endpoint
                .0
                .s3_bucket_versioning_details(&endpoint.1, &job.cancel)?;
            Ok(Outcome::S3Versioning(endpoint, bucket, status))
        });
    }

    pub(super) fn show_s3_versioning(
        &mut self,
        endpoint: FtpEndpoint,
        bucket: String,
        current: ftp::S3VersioningConfig,
    ) {
        use crate::locale::tr;
        let (status, label) = match current.status {
            ftp::S3Versioning::Enabled => ("Aktiv", "Versionierung aussetzen"),
            ftp::S3Versioning::Unversioned | ftp::S3Versioning::Suspended => (
                if current.status == ftp::S3Versioning::Unversioned {
                    "Noch nie aktiviert"
                } else {
                    "Ausgesetzt"
                },
                "Versionierung aktivieren",
            ),
        };
        let mfa_action = if current.mfa_delete {
            "MFA Delete deaktivieren"
        } else if current.status == ftp::S3Versioning::Unversioned {
            "Versionierung und MFA Delete aktivieren"
        } else {
            "MFA Delete aktivieren"
        };
        let description = format!(
            "{bucket}\n{} · {}\nMFA Delete · {}",
            tr("Versionierung"),
            tr(status),
            tr(if current.mfa_delete {
                "Aktiv"
            } else {
                "Inaktiv"
            })
        );
        self.dialog(
            "confirm",
            Pending::ChooseS3Versioning(endpoint, bucket, current),
            Dialog {
                title: "S3-Bucket-Versionierung",
                description: &description,
                options: &[label, mfa_action],
                options_label: "Änderung",
                confirm_label: "Weiter",
                ..Default::default()
            },
        );
    }

    pub(crate) fn choose_s3_versioning(
        &mut self,
        endpoint: FtpEndpoint,
        bucket: String,
        current: ftp::S3VersioningConfig,
        option: usize,
    ) -> io::Result<()> {
        use crate::locale::tr;
        use ftp::S3Versioning::*;
        let (next, label, effect) = match option {
            0 if current.status == Enabled => (
                ftp::S3VersioningConfig {
                    status: Suspended,
                    ..current
                },
                "Versionierung aussetzen",
                "Neue Uploads erhalten die Versions-ID null. Weitere Uploads können diese Version überschreiben.",
            ),
            0 => (
                ftp::S3VersioningConfig {
                    status: Enabled,
                    ..current
                },
                "Versionierung aktivieren",
                "Neue Uploads erhalten eine eigene Versions-ID.",
            ),
            1 => (
                ftp::S3VersioningConfig {
                    status: if current.status == Unversioned {
                        Enabled
                    } else {
                        current.status
                    },
                    mfa_delete: !current.mfa_delete,
                },
                if current.mfa_delete {
                    "MFA Delete deaktivieren"
                } else if current.status == Unversioned {
                    "Versionierung und MFA Delete aktivieren"
                } else {
                    "MFA Delete aktivieren"
                },
                if current.mfa_delete {
                    "Dauerhafte Versionslöschungen benötigen anschließend keinen MFA-Code mehr."
                } else {
                    "Das dauerhafte Löschen von Versionen und Änderungen der Versionierung benötigen anschließend einen MFA-Code."
                },
            ),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Ungültige S3-Aktion.",
                ));
            }
        };
        let description = format!("{bucket}\n\n{}", tr(effect));
        let protected = current.requires_mfa(next);
        if protected {
            ftp::S3Mfa::require_https(&endpoint.1)?;
        }
        self.dialog(
            if protected { "input" } else { "confirm" },
            Pending::SetS3Versioning(endpoint, current, next),
            Dialog {
                title: "S3-Bucket-Versionierung",
                description: &description,
                confirm_label: label,
                placeholder: "MFA-Gerät (Seriennummer oder ARN)",
                second_placeholder: "Sechsstelliger MFA-Code",
                second_secret: true,
                ..Default::default()
            },
        );
        Ok(())
    }

    pub(crate) fn set_s3_versioning(
        &mut self,
        (session, directory): FtpEndpoint,
        before: ftp::S3VersioningConfig,
        next: ftp::S3VersioningConfig,
        mfa: Option<ftp::S3Mfa>,
    ) {
        let description = super::JobDescription::new(
            crate::OperationKind::Ftp,
            directory.to_string(),
            String::new(),
        );
        self.enqueue_virtual_operation("S3-Bucket-Versionierung", false, description, move |job| {
            session.update_s3_bucket_versioning(
                &directory,
                before,
                next,
                mfa.as_ref(),
                &job.cancel,
            )?;
            Ok(Outcome::Report(
                if before.mfa_delete != next.mfa_delete {
                    if next.mfa_delete {
                        "MFA Delete aktiviert"
                    } else {
                        "MFA Delete deaktiviert"
                    }
                } else if next.status == ftp::S3Versioning::Enabled {
                    "S3-Versionierung aktiviert"
                } else {
                    "S3-Versionierung ausgesetzt"
                }
                .into(),
                TransferReport {
                    completed: 1,
                    ..Default::default()
                },
            ))
        });
    }

    pub(crate) fn ask_s3_discard_upload(&mut self) {
        let Some(remote) = self
            .current()
            .ftp
            .as_ref()
            .filter(|tab| tab.location.session.security() == ftp::Security::S3)
        else {
            self.status("Öffne ein S3-Dateifenster.");
            return;
        };
        let endpoint = (
            remote.location.session.clone(),
            remote.location.directory.clone(),
        );
        self.dialog(
            "input",
            Pending::S3DiscardUpload(endpoint),
            Dialog {
                title: "Fortsetzbaren S3-Upload verwerfen",
                description: "Gib den Zielnamen des begonnenen Uploads ein.",
                placeholder: "Entfernter Dateiname",
                confirm_label: "Weiter",
                ..Default::default()
            },
        );
    }

    pub(crate) fn confirm_s3_discard_upload(
        &mut self,
        (session, directory): FtpEndpoint,
        target: &str,
    ) -> io::Result<()> {
        let (directory, name) = session.download_location(&directory, target)?;
        let description = format!(
            "{}\n\n{}",
            name,
            crate::locale::tr("Arco verwirft die übertragenen Teile dieses begonnenen Uploads."),
        );
        self.dialog(
            "confirm",
            Pending::ConfirmS3DiscardUpload((session, directory), name),
            Dialog {
                title: "Fortsetzbaren S3-Upload verwerfen",
                description: &description,
                confirm_label: "Upload verwerfen",
                ..Default::default()
            },
        );
        Ok(())
    }

    pub(crate) fn discard_s3_upload(&mut self, (session, directory): FtpEndpoint, name: String) {
        let title = "Fortsetzbaren S3-Upload verwerfen";
        let description = super::JobDescription::new(
            crate::OperationKind::Ftp,
            format!("{directory}{name}"),
            String::new(),
        );
        self.enqueue_virtual_operation(title, false, description, move |job| {
            let discarded = session.discard_s3_upload(&directory, &name, &job.cancel)?;
            let result = if discarded {
                "S3-Upload verworfen."
            } else {
                "Kein gespeicherter S3-Upload für dieses Ziel."
            };
            Ok(Outcome::Text(
                title.into(),
                crate::locale::tr(result).into(),
            ))
        });
    }

    pub(crate) fn save_s3_version(
        &mut self,
        ((session, directory), name, version, target): (
            FtpEndpoint,
            String,
            ftp::S3Version,
            PathBuf,
        ),
        output: &str,
    ) -> io::Result<()> {
        let target = target.join(files::leaf_name(OsStr::new(output.trim()))?);
        let description = super::JobDescription::new(
            crate::OperationKind::Ftp,
            format!("{directory}{name}"),
            display_path(&target),
        );
        self.enqueue_virtual_operation("S3-Version herunterladen", true, description, move |job| {
            session.download_s3_version(
                &directory,
                &name,
                &version,
                &target,
                &job.cancel,
                job.ftp_download_progress(name.clone(), version.size),
            )?;
            Ok(Outcome::Report(
                "S3-Version heruntergeladen".into(),
                TransferReport {
                    completed: 1,
                    ..Default::default()
                },
            ))
        });
        Ok(())
    }

    pub(crate) fn open_webdav_locks(&mut self) {
        let tab = self.current();
        let Some(ftp) = tab
            .ftp
            .as_ref()
            .filter(|ftp| ftp.location.session.security() == ftp::Security::WebDav)
        else {
            self.status("Öffne ein WebDAV-Dateifenster.");
            return;
        };
        let session = &ftp.location.session;
        let selected = tab
            .display_row(tab.cursor)
            .and_then(|row| ftp.entry(&row.path));
        let (url, recursive) = match selected {
            Some(entry) => match session.entry_url(&ftp.location.directory, entry) {
                Ok(url) => (url, entry.directory == Some(true)),
                Err(error) => {
                    self.status(error.to_string());
                    return;
                }
            },
            None => (ftp.location.directory.clone(), true),
        };
        use crate::locale::tr;
        let address = url.to_string();
        self.dialog(
            "input",
            Pending::WebDavLocks((session.clone(), url), recursive),
            Dialog {
                title: "WebDAV-Sperren verwalten",
                description: &address,
                value: "600",
                placeholder: "Sperrdauer in Sekunden",
                options_label: "Aktion",
                options: &[
                    tr("Sperrstatus anzeigen"),
                    tr("Schreibsperre anlegen"),
                    tr("Eigene Sperre verlängern"),
                    tr("Eigene Sperre aufheben"),
                ],
                ..Default::default()
            },
        );
    }

    pub(crate) fn run_webdav_lock_action(
        &mut self,
        (session, url): FtpEndpoint,
        recursive: bool,
        action: usize,
        value: &str,
    ) -> io::Result<()> {
        let seconds = if matches!(action, 1 | 2) {
            value
                .trim()
                .parse()
                .ok()
                .filter(|seconds| (30..=3600).contains(seconds))
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "Gib für die WebDAV-Sperre eine Dauer von 30 bis 3600 Sekunden ein.",
                    )
                })?
        } else {
            600
        };
        let description =
            super::JobDescription::new(crate::OperationKind::Ftp, url.to_string(), String::new());
        self.start_operation_job("WebDAV-Sperren verwalten", false, description, move |job| {
            use crate::locale::tr;
            let result = match action {
                0 => {
                    let locks = session.webdav_locks(&url, &job.cancel)?;
                    let mut details = url.to_string();
                    if locks.is_empty() {
                        details.push_str("\n\n");
                        details.push_str(tr("Der WebDAV-Server meldet keine Sperren."));
                    }
                    for lock in locks {
                        use std::fmt::Write as _;
                        let ownership = if lock.owned {
                            tr("Eigene Sperre")
                        } else {
                            tr("Serverseitige Sperre")
                        };
                        let _ = write!(
                            details,
                            "\n\n{ownership}\n{}  {}\n{}  {}\n{}  {}",
                            tr("Besitzer"),
                            lock.owner,
                            tr("Tiefe"),
                            lock.depth,
                            tr("Restdauer"),
                            lock.timeout
                        );
                    }
                    return Ok(Outcome::Text(tr("WebDAV-Sperren").into(), details));
                }
                1 => {
                    session.webdav_lock(&url, recursive, seconds, &job.cancel)?;
                    "WebDAV-Schreibsperre angelegt."
                }
                2 => {
                    session.webdav_refresh_lock(&url, seconds, &job.cancel)?;
                    "WebDAV-Schreibsperre verlängert."
                }
                3 => {
                    session.webdav_unlock(&url, &job.cancel)?;
                    "WebDAV-Schreibsperre aufgehoben."
                }
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "Wähle eine WebDAV-Sperraktion.",
                    ));
                }
            };
            Ok(Outcome::Notice(tr(result).into()))
        });
        Ok(())
    }

    pub(crate) fn open_s3_tags(&mut self) {
        let tab = self.current();
        let selected = tab
            .ftp
            .as_ref()
            .filter(|ftp| ftp.location.session.security() == ftp::Security::S3)
            .and_then(|ftp| {
                Some((
                    ftp.location.clone(),
                    ftp.entry(&tab.display_row(tab.cursor)?.path)?.clone(),
                ))
            })
            .filter(|(_, entry)| entry.directory == Some(false));
        let Some((location, entry)) = selected else {
            self.status("Wähle ein S3-Objekt.");
            return;
        };
        let description = super::JobDescription::new(
            crate::OperationKind::Metadata,
            entry.name.clone(),
            String::new(),
        );
        self.start_operation_job("S3-Objekt-Tags lesen", false, description, move |job| {
            let tags = location
                .session
                .s3_tags(&location.directory, &entry, &job.cancel)?;
            Ok(Outcome::S3Tags(Box::new((location, entry, tags))))
        });
    }

    pub(super) fn edit_s3_tags(&mut self, tags: Box<(FtpLocation, ftp::Entry, ftp::S3Tags)>) {
        let description = tags.1.name.clone();
        let data = serde_json::to_string_pretty(&tags.2.tags)
            .expect("S3 tag strings are JSON-serializable");
        self.dialog(
            "multiline",
            Pending::S3Tags(tags),
            Dialog {
                title: "S3-Objekt-Tags bearbeiten",
                description: &description,
                placeholder: "Tags als JSON-Objekt aus Schlüssel und Wert",
                value: &data,
                ..Default::default()
            },
        );
    }

    pub(crate) fn save_s3_tags(
        &mut self,
        (location, entry, before): (FtpLocation, ftp::Entry, ftp::S3Tags),
        text: &str,
    ) -> io::Result<()> {
        let tags: std::collections::BTreeMap<String, String> =
            serde_json::from_str(text).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("Ungültiges S3-Tag-JSON. {error}"),
                )
            })?;
        let description = super::JobDescription::new(
            crate::OperationKind::Metadata,
            entry.name.clone(),
            String::new(),
        );
        self.start_operation_job("S3-Objekt-Tags speichern", false, description, move |job| {
            location.session.replace_s3_tags(
                &location.directory,
                &entry,
                &before,
                &tags,
                &job.cancel,
            )?;
            Ok(Outcome::Notice(format!(
                "S3-Objekt-Tags gespeichert · {}",
                entry.name
            )))
        });
        Ok(())
    }

    pub(crate) fn ask_remote_checksums(&mut self) {
        use crate::commands::{Dialog, Pending};
        let tab = self.current();
        let Some(ftp) = tab.ftp.as_ref().filter(|ftp| {
            ftp.location.session.security().is_ssh() || ftp.location.session.security().is_ftp()
        }) else {
            self.status(crate::locale::tr(
                "Wähle Dateien im FTP-, FTPS-, SFTP- oder SCP-Dateifenster.",
            ));
            return;
        };
        let entries: Vec<_> = tab
            .selected_paths()
            .iter()
            .filter_map(|path| ftp.entry(path))
            .filter(|entry| entry.directory == Some(false))
            .cloned()
            .collect();
        if entries.is_empty() {
            self.status(crate::locale::tr("Wähle mindestens eine Serverdatei."));
            return;
        }
        let location = ftp.location.clone();
        let mut options = vec![crate::locale::tr("Automatisch")];
        options.extend(
            ftp::ChecksumAlgorithm::ALL
                .into_iter()
                .filter(|algorithm| {
                    location.session.security() != ftp::Security::Scp
                        || *algorithm != ftp::ChecksumAlgorithm::Crc32
                })
                .map(ftp::ChecksumAlgorithm::name),
        );
        self.dialog(
            "confirm",
            Pending::RemoteChecksums(location.clone(), entries),
            Dialog {
                title: "Server-Prüfsummen",
                description: match location.session.security() {
                    ftp::Security::Sftp => "Arco fragt die Prüfsumme beim SFTP-Server ab und berechnet sie bei Bedarf aus den Dateidaten.",
                    ftp::Security::Scp => "Der SCP-Server berechnet die Prüfsumme mit einem GNU-Prüfprogramm.",
                    _ => "Der FTP-Server berechnet die Prüfsumme mit einem von ihm angebotenen Verfahren.",
                },
                options: &options,
                options_label: "Algorithmus",
                confirm_label: "Berechnen",
                ..Dialog::default()
            }
        );
    }

    pub(crate) fn remote_checksums(
        &mut self,
        location: FtpLocation,
        entries: Vec<ftp::Entry>,
        option: usize,
    ) {
        use std::fmt::Write as _;
        let algorithm = option
            .checked_sub(1)
            .and_then(|index| ftp::ChecksumAlgorithm::ALL.get(index))
            .copied();
        let description = super::JobDescription::new(
            crate::OperationKind::Ftp,
            location.directory.to_string(),
            String::new(),
        );
        self.start_operation_job("Server-Prüfsummen", false, description, move |job| {
            let mut result = String::new();
            let total = entries.len();
            let phase = "Prüfsummen berechnen …";
            let method =
                location
                    .session
                    .prepare_checksum(&location.directory, algorithm, &job.cancel)?;
            let mut selected = "";
            for (index, entry) in entries.into_iter().enumerate() {
                job.entry_progress(entry.name.clone(), index, total, phase);
                let progress =
                    job.ftp_progress(entry.name.clone(), entry.size, phase, "Datei prüfen …");
                let (algorithm, hash) = location.session.remote_checksum(
                    &location.directory,
                    &entry,
                    method,
                    &job.cancel,
                    progress,
                )?;
                selected = algorithm;
                let _ = writeln!(result, "{hash} *{}", entry.name);
                job.entry_progress(entry.name, index + 1, total, phase);
            }
            Ok(Outcome::Text(
                format!("{} · {selected}", crate::locale::tr("Server-Prüfsummen")),
                result,
            ))
        });
    }

    pub(crate) fn copy_remote_location(&self, as_url: bool) -> io::Result<()> {
        let tab = self.current();
        let ftp = tab
            .ftp
            .as_ref()
            .ok_or_else(|| io::Error::other("Wähle ein FTP- oder SFTP-Dateifenster."))?;
        let session = &ftp.location.session;
        let directory = &ftp.location.directory;
        let mut addresses = tab
            .selected_paths()
            .iter()
            .filter_map(|path| ftp.entry(path))
            .map(|entry| session.entry_url(directory, entry))
            .collect::<io::Result<Vec<_>>>()?;
        if addresses.is_empty() {
            addresses.push(directory.clone());
        }
        let lines = addresses
            .iter()
            .map(|address| {
                if as_url {
                    Ok(address.to_string())
                } else {
                    session.remote_path(address)
                }
            })
            .collect::<io::Result<Vec<_>>>()?;
        arco_windows::copy_text(&lines.join("\r\n"))
    }

    pub(crate) fn search_ftp(
        &mut self,
        side: usize,
        request: u64,
        location: FtpLocation,
        masks: &str,
    ) -> io::Result<()> {
        if masks.trim().is_empty() {
            return Err(io::Error::other("Gib eine Dateimaske ein."));
        }
        let masks = files::compile_masks(masks)?;
        let ignore = self.ignore_list();
        let description = super::JobDescription::new(
            crate::OperationKind::Search,
            location.directory.to_string(),
            String::new(),
        );
        self.start_operation_job("Server durchsuchen", false, description, move |job| {
            let hits = search_remote(
                &location,
                |name| masks.is_match(name),
                ignore.as_deref(),
                &job.cancel,
                |folder| {
                    job.phase(&display_path(folder));
                },
            )?;
            Ok(Outcome::FtpSearch {
                side,
                request,
                location,
                hits,
            })
        });
        Ok(())
    }

    pub(super) fn show_ftp_search(
        &mut self,
        side: usize,
        request: u64,
        location: FtpLocation,
        hits: Vec<SearchHit>,
    ) {
        if self.panels[side].request != request {
            return;
        }
        let rows = hits
            .iter()
            .map(|hit| crate::ReviewRow {
                primary: display_path(&hit.name).into(),
                secondary: ftp::Session::directory_path(&hit.folder).into(),
                kind: if hit.directory {
                    "folder".into()
                } else {
                    "file".into()
                },
                ..Default::default()
            })
            .collect();
        let description = format!(
            "{} · {} {}",
            location.directory,
            hits.len(),
            crate::locale::tr("Treffer")
        );
        self.management_review(
            crate::OperationKind::Search,
            rows,
            ["Name", "Serverordner"],
            "Zum Treffer",
        );
        self.ui().set_review_can_confirm(!hits.is_empty());
        self.dialog(
            "management-review",
            Pending::FtpSearchHit(side, request, location, hits),
            Dialog {
                title: "Treffer auf dem Server",
                description: &description,
                ..Default::default()
            },
        );
    }

    pub(crate) fn open_ftp_search_hit(
        &mut self,
        side: usize,
        request: u64,
        location: FtpLocation,
        hits: &[SearchHit],
        index: usize,
    ) -> io::Result<()> {
        if self.panels[side].request != request {
            return Err(io::Error::other(
                "Das Dateifenster hat sich seit der Serversuche geändert.",
            ));
        }
        let hit = hits
            .get(index)
            .ok_or_else(|| io::Error::other("Wähle einen Suchtreffer."))?;
        self.load_location(
            side,
            Location::Ftp(location.at(hit.folder.clone())),
            Navigation::Focus(hit.name.clone()),
        );
        Ok(())
    }

    pub(crate) fn ftp_download_list_dialog(&mut self, endpoint: FtpEndpoint) {
        let target = if self.current().ftp.is_some() {
            self.panels[1 - self.active].current()
        } else {
            self.current()
        };
        if !target.is_local() {
            self.status("Wähle einen lokalen Zielordner.");
            return;
        }
        self.dialog("multiline", Pending::FtpDownloadList(endpoint, target.path.clone()), Dialog {
            title: "FTP-Downloadliste",
            description: "Ein FTP-Pfad oder eine URL des verbundenen Servers pro Zeile. Mit @ vor einem absoluten Pfad liest Arco die Liste aus einer Textdatei.",
            placeholder: "Dateiliste",
            confirm_label: "Herunterladen",
            ..Default::default()
        });
    }

    pub(crate) fn ftp_download_list(
        &mut self,
        (session, directory): FtpEndpoint,
        target: PathBuf,
        list: &str,
    ) -> io::Result<()> {
        if list.trim().is_empty() {
            return Err(io::Error::other(
                "Gib mindestens eine Datei für den FTP-Download an.",
            ));
        }
        let list = list.to_owned();
        let job_options = self.job_transfer_options()?;
        let presets = self.profile.network_transfer_presets.clone();
        let description = super::JobDescription::new(
            crate::OperationKind::Ftp,
            directory.to_string(),
            display_path(&target),
        );
        let mut remote_root = directory.clone();
        remote_root.set_path("/");
        let access = vec![
            super::TransferAccess::local(&target, true),
            super::TransferAccess::remote(&session, &remote_root, None, false)?,
        ];
        let transfer = self.new_network_transfer(access);
        let connections = transfer.file_connections();
        let session = Arc::new(transfer.session(&session));
        self.enqueue_network_transfer(
            "FTP-Downloadliste",
            true,
            description,
            transfer,
            move |job| {
                let text = if let Some(path) = list.trim().strip_prefix('@')
                    && !path.contains(['\r', '\n'])
                    && Path::new(path.trim_matches('"')).is_absolute()
                {
                    arco_core::profile::read_tc_text(Path::new(path.trim_matches('"')))?
                } else {
                    list
                };
                let mut downloads = Vec::new();
                let mut names = BTreeSet::new();
                for line in text
                    .lines()
                    .filter(|line| !line.trim().is_empty() && !line.starts_with("# "))
                {
                    files::check_cancel(&job.cancel)?;
                    if downloads.len() >= 1000 {
                        return Err(io::Error::other(
                            "Eine FTP-Downloadliste darf höchstens 1000 Dateien enthalten.",
                        ));
                    }
                    let (parent, name) = session.download_location(&directory, line)?;
                    let transfer =
                        job_options.resolve(&presets, &session, &parent, Some(&target))?;
                    arco_app::network_sync::check_transfer_mask(&transfer, &name)?;
                    let leaf = transfer.transfer_settings().names.download(&name)?;
                    let output = target.join(&leaf);
                    if !names.insert(leaf.to_lowercase()) || output.try_exists()? {
                        return Err(io::Error::new(
                            io::ErrorKind::AlreadyExists,
                            format!("Doppeltes oder vorhandenes Downloadziel: {leaf}"),
                        ));
                    }
                    downloads.push((transfer, parent, name, output));
                }
                if downloads.is_empty() {
                    return Err(io::Error::other(
                        "Gib mindestens eine Datei für den FTP-Download an.",
                    ));
                }
                let total = downloads.len();
                job.entry_progress(String::new(), 0, total, "FTP-Downloadliste");
                let report = arco_app::network_batch::Batch {
                    connections: connections.as_ref(),
                    cancel: &job.cancel,
                }
                .run(
                    &downloads,
                    true,
                    |index, width| session.for_file_worker(index, width),
                    |worker, (transfer, parent, name, output)| {
                        worker
                            .with_transfer_snapshot(transfer.transfer_settings().clone())
                            .download_preferred(parent, name, output, &job.cancel, |_| {})
                            .map_err(|error| {
                                io::Error::new(error.kind(), format!("{name}: {error}"))
                            })
                    },
                    |(_, _, name, _), count| {
                        job.entry_progress(name.clone(), count, total, "FTP-Downloadliste")
                    },
                );
                Ok(Outcome::Report("FTP-Downloadliste".into(), report))
            },
        )
    }

    pub(crate) fn ftp_endpoint(&self) -> Option<FtpEndpoint> {
        let browser = || {
            self.ftp
                .as_ref()
                .map(|view| (view.session.clone(), view.directory.clone()))
        };
        if self.ui().get_dialog_kind().as_str() == "ftp-browser" {
            return browser();
        }
        [self.active, 1 - self.active]
            .into_iter()
            .find_map(|side| {
                self.panels[side]
                    .current()
                    .ftp
                    .as_ref()
                    .map(|ftp| (ftp.location.session.clone(), ftp.location.directory.clone()))
            })
            .or_else(browser)
    }

    pub(crate) fn open_server_terminal(&mut self, in_current_directory: bool) {
        #[cfg(windows)]
        {
            let Some((session, directory)) = self
                .ftp_endpoint()
                .filter(|endpoint| endpoint.0.security().is_ssh())
            else {
                self.status("Öffne ein SFTP- oder SCP-Dateifenster.");
                return;
            };
            let result = (|| -> io::Result<()> {
                let helper = std::env::current_exe()?.with_file_name("arco-net.exe");
                if !helper.is_file() {
                    return Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        "arco-net.exe fehlt neben Arco Commander.",
                    ));
                }
                let sender = self.sender.clone();
                std::thread::Builder::new()
                    .name("arco-ssh-terminal".into())
                    .spawn(move || {
                        if let Err(error) = session.open_ssh_terminal(
                            in_current_directory.then_some(&directory),
                            &helper,
                            &AtomicBool::new(false),
                        ) {
                            let _ = sender.send(super::Event::SshTerminalError(error.to_string()));
                        }
                    })?;
                Ok(())
            })();
            self.report(result);
        }
        #[cfg(not(windows))]
        {
            let _ = in_current_directory;
            self.status("Die SSH-Terminalübergabe benötigt Windows.");
        }
    }

    pub(super) fn navigate_ftp_text(&mut self, mut location: FtpLocation, value: &str) {
        let result = (|| -> io::Result<FtpLocation> {
            if value.starts_with("ftp://")
                || value.starts_with("ftps://")
                || value.starts_with("sftp://")
                || value.starts_with("scp://")
                || value.starts_with("https://")
                || value.starts_with("http://")
            {
                let directory = location.session.directory_url(value)?;
                return Ok(location.at(directory));
            }
            if value.starts_with('/') {
                location.directory = location.session.directory("/")?;
            }
            for part in value.split('/').filter(|part| !part.is_empty()) {
                match part {
                    "." => (),
                    ".." => {
                        if let Some(parent) = location.parent() {
                            location = parent;
                        }
                    }
                    name => {
                        location.directory = location
                            .session
                            .child_directory(&location.directory, name)?;
                    }
                }
            }
            Ok(location)
        })();
        match result {
            Ok(location) => {
                self.load_location(self.active, Location::Ftp(location), Navigation::Jump)
            }
            Err(error) => self.status(error.to_string()),
        }
    }

    pub(crate) fn go_ftp_root(&mut self) {
        if let Some(tab) = &self.current().ftp {
            self.navigate_ftp_text(tab.location.clone(), "/");
        }
    }

    pub(super) fn open_ftp_entry(&mut self, location: FtpLocation, entry: ftp::Entry) {
        if entry.symlink {
            self.open_ftp_link(location, entry.name, None);
        } else if entry.directory == Some(true) {
            match location
                .session
                .child_directory(&location.directory, &entry.name)
            {
                Ok(directory) => self.load_location(
                    self.active,
                    Location::Ftp(location.at(directory)),
                    Navigation::Jump,
                ),
                Err(error) => self.status(error.to_string()),
            }
        } else {
            self.preview_ftp_entry(location, entry, false);
        }
    }

    pub(crate) fn view_ftp_panel_entry(&mut self, separate: bool) {
        let tab = self.current();
        let Some(ftp) = &tab.ftp else {
            return;
        };
        let Some(entry) = tab
            .display_row(tab.cursor)
            .and_then(|entry| ftp.entry(&entry.path))
        else {
            return;
        };
        if entry.symlink {
            self.open_ftp_link(ftp.location.clone(), entry.name.clone(), Some(separate));
        } else {
            self.preview_ftp_entry(ftp.location.clone(), entry.clone(), separate);
        }
    }

    fn open_ftp_link(&mut self, location: FtpLocation, name: String, preview: Option<bool>) {
        let side = self.active;
        let request = self.panels[side].request;
        let description =
            super::JobDescription::new(crate::OperationKind::Ftp, name.clone(), String::new());
        self.start_operation_job("Linkziel öffnen …", false, description, move |job| {
            let target =
                location
                    .session
                    .resolve_remote_link(&location.directory, &name, &job.cancel)?;
            Ok(Outcome::RemoteLinkResolved {
                side,
                request,
                location,
                target: Box::new(target),
                preview,
            })
        });
    }

    pub(super) fn complete_open_remote_link(
        &mut self,
        side: usize,
        request: u64,
        location: FtpLocation,
        target: ftp::ResolvedLink,
        preview: Option<bool>,
    ) {
        let panel = &self.panels[side];
        let unchanged = panel.current().ftp.as_ref().is_some_and(|ftp| {
            Arc::ptr_eq(&ftp.location.session, &location.session)
                && ftp.location.directory == location.directory
        });
        if self.active != side || panel.request != request || !unchanged {
            self.status("Ansicht hat sich während des Öffnens geändert.");
            return;
        }
        match target {
            ftp::ResolvedLink::Directory(directory) if preview.is_none() => {
                self.load_location(side, Location::Ftp(location.at(directory)), Navigation::Jump);
            }
            ftp::ResolvedLink::Directory(_) => self.status("Der Link verweist auf einen Ordner. Öffne ihn mit der Eingabetaste oder per Doppelklick."),
            ftp::ResolvedLink::File(parent, entry) => {
                self.preview_ftp_entry(location.at(parent), entry, preview.unwrap_or(false));
            }
        }
    }

    fn preview_ftp_entry(&mut self, location: FtpLocation, entry: ftp::Entry, separate: bool) {
        if entry.directory == Some(true) {
            self.status("Wähle eine Datei für die FTP-Vorschau.");
            return;
        }
        if entry.size.is_some_and(|size| size > MAX_PREVIEW) {
            self.status("Die FTP-Vorschau unterstützt bis zu 64 MiB pro Datei.");
            return;
        }
        let source = format!("{}{}", location.directory, entry.name);
        let description =
            super::JobDescription::new(crate::OperationKind::Ftp, source.clone(), String::new());
        self.start_operation_job("FTP-Vorschau laden", false, description, move |job| {
            let storage = Arc::new(tempfile::tempdir()?);
            let path = storage
                .path()
                .join(copy_names::encode_remote_name(&entry.name)?);
            location.session.download_bounded_with_progress(
                &location.directory,
                &entry.name,
                &path,
                MAX_PREVIEW,
                &job.cancel,
                job.ftp_download_progress(source, entry.size),
            )?;
            job.phase("Vorschau vorbereiten …");
            let content = preview::read(&path)?;
            let title = format!("{}{}", location.directory, entry.name);
            Ok(Outcome::StagedPreview(
                StagedPreview {
                    storage,
                    path,
                    title,
                    separate,
                },
                content,
            ))
        });
    }

    pub(crate) fn ftp_panel_properties(&mut self) {
        let tab = self.current();
        let Some(ftp) = &tab.ftp else {
            return;
        };
        let entries = tab
            .selected_paths()
            .iter()
            .filter_map(|path| ftp.entry(path).cloned())
            .collect::<Vec<_>>();
        let entry = tab.display_row(tab.cursor);
        let path = entry
            .map(|entry| format!("{}{}", ftp.location.directory, entry.name.to_string_lossy()))
            .unwrap_or_else(|| ftp.location.directory.to_string());
        let mut sheet = super::properties::Sheet::virtual_entry("FTP-Eigenschaften", path, entry);
        sheet.field("Verzeichnisformat", format!("{:?}", ftp.listing.format));
        if let Some(remote) = entry.and_then(|entry| ftp.entry(&entry.path)) {
            sheet.field(
                "Server-Zeitstempel (UTC)",
                remote
                    .modified
                    .clone()
                    .unwrap_or_else(|| crate::locale::tr("Nicht verfügbar").into()),
            );
        }
        if ftp.listing.format.structured()
            && !entries.is_empty()
            && (ftp.location.session.security().is_ftp()
                || ftp.location.session.security().is_ssh())
        {
            sheet.field("Ausgewählte Einträge", entries.len().to_string());
            sheet.ftp = Some((ftp.location.clone(), entries));
        }
        if let Some((location, entries)) = &sheet.ftp
            && let [entry] = entries.as_slice()
            && location.session.security().is_ssh()
        {
            let location = location.clone();
            let entry = entry.clone();
            let description = super::JobDescription::new(
                crate::OperationKind::Metadata,
                sheet.path.clone(),
                String::new(),
            );
            self.start_operation_job("Eigenschaften lesen", false, description, move |job| {
                use crate::locale::tr;
                if entry.symlink {
                    match location.session.remote_link_target(
                        &location.directory,
                        &entry.name,
                        &job.cancel,
                    ) {
                        Ok(target) => sheet.field("Linkziel auf dem Server", target),
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                            return Err(error);
                        }
                        Err(error) => sheet.field(
                            "Linkziel auf dem Server",
                            crate::locale::message(&error.to_string()).into_owned(),
                        ),
                    }
                }
                match location
                    .session
                    .unix_metadata(&location.directory, &entry, &job.cancel)
                {
                    Ok(attrs) => {
                        let missing = || tr("Nicht verfügbar").to_owned();
                        sheet.field(
                            "Unix-Rechte",
                            attrs
                                .mode
                                .map_or_else(missing, |mode| format!("{mode:04o}")),
                        );
                        sheet.field(
                            "Besitzer-ID (UID)",
                            attrs.owner.map_or_else(missing, |owner| owner.to_string()),
                        );
                        sheet.field(
                            "Gruppen-ID (GID)",
                            attrs.group.map_or_else(missing, |group| group.to_string()),
                        );
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => return Err(error),
                    Err(error) => sheet.field(
                        "Unix-Rechte",
                        crate::locale::message(&error.to_string()).into_owned(),
                    ),
                }
                Ok(Outcome::Properties(Box::new(sheet)))
            });
            return;
        }
        self.show_properties(sheet);
    }

    pub(crate) fn disconnect_ftp(&mut self) {
        let Some((session, _)) = self.ftp_endpoint() else {
            return;
        };
        if self.ftp_session_upload_running(&session) {
            self.status("Beende zuerst die aktive FTP-Rückübertragung im Transfermanager.");
            return;
        }
        self.stop_watch_for(&session);
        if self
            .ftp
            .as_ref()
            .is_some_and(|view| Arc::ptr_eq(&view.session, &session))
        {
            self.ftp = None;
        }
        if self
            .fxp_source
            .as_ref()
            .is_some_and(|(source, _)| Arc::ptr_eq(source, &session))
        {
            self.fxp_source = None;
        }
        for side in 0..2 {
            let panel = &mut self.panels[side];
            let mut reload = panel.loading;
            for (index, tab) in panel.tabs.iter_mut().enumerate() {
                let retain = |location: &Location| !matches!(location, Location::Ftp(ftp) if Arc::ptr_eq(&ftp.session, &session));
                tab.back.retain(retain);
                tab.forward.retain(retain);
                if tab.lock_base.as_ref().is_some_and(|base| !retain(base)) {
                    tab.set_lock(TabLock::Unlocked);
                }
                if tab
                    .ftp
                    .as_ref()
                    .is_some_and(|ftp| Arc::ptr_eq(&ftp.location.session, &session))
                {
                    if tab.options.lock == TabLock::Fixed {
                        tab.set_lock(TabLock::Unlocked);
                    }
                    tab.change_location(&Location::Local(tab.path.clone()), None);
                    tab.space = None;
                    tab.populate(Vec::new(), self.profile.show_hidden, String::new());
                    tab.loaded = false;
                    reload |= index == panel.tab;
                }
            }
            if reload {
                self.refresh(side);
            } else {
                self.publish(side);
            }
        }
        self.forget_closed_ftp_tabs(&session);
        let drafts = self.close_ftp_drafts(&session);
        if drafts == 0 {
            self.status("FTP-Verbindung beendet.");
        } else {
            self.status(format!(
                "FTP-Verbindung beendet. {drafts} lokale Arbeitskopien bleiben erhalten."
            ));
        }
    }

    pub(crate) fn execute_ftp_edit(
        &mut self,
        location: FtpLocation,
        edit: FtpEdit,
        name: &str,
    ) -> io::Result<()> {
        match edit {
            FtpEdit::Modified(entries) => {
                let date = chrono::NaiveDateTime::parse_from_str(name.trim(), "%Y-%m-%d %H:%M:%S")
                    .map_err(|_| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "Gib Datum und Uhrzeit als JJJJ-MM-TT HH:MM:SS (UTC) ein.",
                        )
                    })?;
                let stamp = date.format("%Y%m%d%H%M%S").to_string();
                let title = "Server-Änderungszeit setzen";
                let description = super::JobDescription::new(
                    crate::OperationKind::Metadata,
                    location.directory.to_string(),
                    String::new(),
                );
                self.start_operation_job(title, true, description, move |job| {
                    let mut report = TransferReport::default();
                    let total = entries.len();
                    for (index, entry) in entries.into_iter().enumerate() {
                        job.entry_progress(entry.name.clone(), index, total, title);
                        match location.session.set_ssh_modified(
                            &location.directory,
                            &entry,
                            &stamp,
                            &job.cancel,
                        ) {
                            Ok(()) => report.completed += 1,
                            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                                report.cancelled = true;
                                break;
                            }
                            Err(error) => report.errors.push(format!("{}: {error}", entry.name)),
                        }
                        job.entry_progress(entry.name, index + 1, total, title);
                    }
                    Ok(Outcome::MetadataEdited(title, report))
                });
            }
            FtpEdit::Delete(entries, permanent) => {
                let detail = format!(
                    "{} · {}",
                    crate::locale::tr("Ausgewählte Einträge"),
                    entries.len()
                );
                let recycle = !permanent && location.session.will_recycle(&location.directory);
                let title = if recycle {
                    "In den Server-Papierkorb verschieben"
                } else {
                    "Lösche FTP-Auswahl …"
                };
                self.start_delete_job(title, detail, move |job| {
                    let report = delete_selection(
                        location,
                        &entries,
                        recycle,
                        &job.cancel,
                        |path, processed, total| {
                            job.deletion_progress(display_path(path), processed, total);
                        },
                    )?;
                    Ok(Outcome::Report(title.into(), report))
                });
            }
            edit => {
                let name = files::leaf_name(OsStr::new(name))?
                    .to_str()
                    .expect("Unicode name")
                    .to_owned();
                let description = super::JobDescription::new(
                    crate::OperationKind::Ftp,
                    location.directory.to_string(),
                    String::new(),
                );
                self.start_operation_job(
                    "FTP-Verzeichnis bearbeiten",
                    true,
                    description,
                    move |job| {
                        job.entry_progress(name.clone(), 0, 1, "FTP-Verzeichnis bearbeiten …");
                        match edit {
                            FtpEdit::Create => location.session.create_sync_directory(
                                &location.directory,
                                &name,
                                &job.cancel,
                            )?,
                            FtpEdit::Rename(entry) => location.session.rename_sync_entry(
                                &location.directory,
                                &entry,
                                &name,
                                &job.cancel,
                            )?,
                            FtpEdit::Delete(..) | FtpEdit::Modified(_) => {
                                unreachable!()
                            }
                        }
                        Ok(Outcome::Notice(format!(
                            "FTP-Eintrag aktualisiert · {name}"
                        )))
                    },
                );
            }
        }
        Ok(())
    }
}

fn delete_selection(
    location: FtpLocation,
    selected: &[ftp::Entry],
    recycle: bool,
    cancel: &AtomicBool,
    mut progress: impl FnMut(&Path, usize, usize),
) -> io::Result<TransferReport> {
    for entry in selected {
        if location
            .session
            .protects_recycle_directory(&location.directory, entry, cancel)?
        {
            return Err(io::Error::other(
                "Öffne den Server-Papierkorb, um seine Inhalte zu entfernen.",
            ));
        }
    }
    if recycle {
        let mut report = TransferReport::default();
        for (index, entry) in selected.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                report.cancelled = true;
                break;
            }
            let path = Path::new(&entry.name);
            progress(path, index, selected.len());
            match location
                .session
                .recycle_entry(&location.directory, entry, cancel)
            {
                Ok(_) => report.completed += 1,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                    report.cancelled = true;
                    break;
                }
                Err(error) => report.errors.push(format!("{}: {error}", entry.name)),
            }
            progress(path, index + 1, selected.len());
        }
        return Ok(report);
    }
    let paths = selected
        .iter()
        .map(|entry| copy_names::encode_remote_name(&entry.name).map(PathBuf::from))
        .collect::<io::Result<Vec<_>>>()?;
    let scope = Scope::new(true, &paths)?;
    let endpoint = (location.session, location.directory);
    let entries = scan_for_delete(&endpoint, &scope, cancel)?;
    for (path, before) in paths.iter().zip(selected) {
        if entries.get(path) != Some(before) {
            return Err(io::Error::other(format!(
                "Der FTP-Eintrag „{}“ hat sich seit dem Anzeigen geändert.",
                before.name
            )));
        }
    }
    let mut report = TransferReport::default();
    for (index, (path, entry)) in entries.iter().rev().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
            break;
        }
        let display = copy_names::reported_path(path, true)?;
        progress(&display, index, entries.len());
        let result = copy_names::remote_parent(&endpoint, path, true)
            .and_then(|parent| endpoint.0.delete_sync_entry(&parent, entry, cancel));
        match result {
            Ok(()) => report.completed += 1,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                report.cancelled = true;
                break;
            }
            Err(error) => report
                .errors
                .push(format!("{}: {error}", display.display())),
        }
        progress(&display, index + 1, entries.len());
    }
    Ok(report)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    #[ignore = "requires the dedicated SFTP link fixture"]
    fn mounted_sftp_skips_link_targets_in_mirror_and_recursive_delete() {
        use arco_app::network_sync::FtpPlan;
        use arco_core::sync::{Action, Mode, Options};

        let session = ftp::Connection::new(
            &std::env::var("ARCO_SFTP_LINK_URL").unwrap(),
            "arco",
            ftp::Security::Sftp,
        )
        .unwrap()
        .with_ssh(&std::env::var("ARCO_SFTP_LINK_FINGERPRINT").unwrap(), None)
        .unwrap()
        .authenticate("fixture-only")
        .unwrap();
        let mut settings = session.transfer_settings().clone();
        settings.names.windows = true;
        let session = Arc::new(session.with_transfer_snapshot(settings));
        let cancel = AtomicBool::new(false);
        let root = session.directory("arco-link-fixture").unwrap();
        session
            .create_sync_directory(&root, "panel operations", &cancel)
            .unwrap();
        let directory = session.child_directory(&root, "panel operations").unwrap();
        let location = FtpLocation {
            directory: directory.clone(),
            session: session.clone(),
            profile: None,
        };
        session
            .create_sync_directory(&directory, "container", &cancel)
            .unwrap();
        let container = session.child_directory(&directory, "container").unwrap();
        session
            .create_remote_link(
                &container,
                "nested",
                "../../target folder",
                ftp::RemoteLinkKind::Symbolic,
                &cancel,
            )
            .unwrap();
        session
            .create_remote_link(
                &directory,
                "alias?.txt",
                "../source file.txt",
                ftp::RemoteLinkKind::Symbolic,
                &cancel,
            )
            .unwrap();

        let local = tempfile::tempdir().unwrap();
        let counterpart = local
            .path()
            .join(copy_names::encode_remote_name("alias?.txt").unwrap());
        fs::write(&counterpart, b"local counterpart").unwrap();
        let plan = FtpPlan::compare(
            local.path(),
            (session.clone(), directory.clone()),
            &Options {
                mode: Mode::Mirror,
                ..Options::default()
            },
            &[],
            &|_, _| false,
            &cancel,
            |_| {},
        )
        .unwrap();
        assert!(
            plan.rows
                .iter()
                .all(|row| row.action != Action::DeleteRight)
        );
        assert!(
            plan.rows
                .iter()
                .all(|row| !row.relative.ends_with(counterpart.file_name().unwrap()))
        );
        let report = plan.apply(&cancel, |_| {});
        assert_eq!(report.completed, 0);
        assert!(report.errors.is_empty(), "{:?}", report.errors);

        let pane = read_panel(&location, &cancel).unwrap();
        assert!(
            pane.directory
                .entries
                .iter()
                .any(|row| row.kind == Kind::Link && row.name == "alias?.txt")
        );
        let selected = pane.ftp.unwrap().listing.entries;
        let report = delete_selection(location, &selected, false, &cancel, |_, _, _| {}).unwrap();
        assert_eq!(report.completed, 3);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(
            session
                .list_sync(&directory, &cancel)
                .unwrap()
                .entries
                .is_empty()
        );
        let outside = session.child_directory(&root, "target folder").unwrap();
        assert_eq!(
            session.list_sync(&outside, &cancel).unwrap().entries[0].name,
            "preserved.txt"
        );
        assert!(
            session
                .list_sync(&root, &cancel)
                .unwrap()
                .entries
                .iter()
                .any(|entry| entry.name == "source file.txt")
        );
    }

    #[test]
    #[ignore = "requires a disposable writable local FTP server"]
    fn mounted_ftp_reads_and_edits_nested_directories() {
        let address = std::env::var("ARCO_TEST_FTP").unwrap();
        let session = Arc::new(
            ftp::Connection::new(&address, "arco", ftp::Security::Plain)
                .unwrap()
                .authenticate("test-only")
                .unwrap(),
        );
        let cancel = AtomicBool::new(false);
        let root = session.initial_directory();
        session
            .create_sync_directory(&root, "panel", &cancel)
            .unwrap();
        let panel = FtpLocation {
            directory: session
                .relative_directory(&root, Path::new("panel"))
                .unwrap(),
            session,
            profile: None,
        };
        panel
            .session
            .create_sync_directory(&panel.directory, "erster Ordner", &cancel)
            .unwrap();
        let listing = read_panel(&panel, &cancel).unwrap();
        assert_eq!(listing.directory.entries[0].kind, Kind::Directory);
        let entry = &listing.ftp.as_ref().unwrap().listing.entries[0];
        panel
            .session
            .rename_sync_entry(&panel.directory, entry, "Zweiter Ordner", &cancel)
            .unwrap();
        let child = panel
            .session
            .relative_directory(&panel.directory, Path::new("Zweiter Ordner"))
            .unwrap();
        assert_eq!(panel.at(child.clone()).parent(), Some(panel.clone()));
        let fixture = tempfile::tempdir().unwrap();
        let file = fixture.path().join("file.txt");
        fs::write(&file, b"remote content").unwrap();
        panel
            .session
            .upload(&child, &file, "file.txt", false, &cancel)
            .unwrap();
        let fresh = read_panel(&panel, &cancel).unwrap();
        let selected = &fresh.ftp.as_ref().unwrap().listing.entries[0];
        let mut progress = Vec::new();
        let report = delete_selection(
            panel.clone(),
            std::slice::from_ref(selected),
            false,
            &cancel,
            |_, processed, total| progress.push((processed, total)),
        )
        .unwrap();
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.completed, 2);
        assert_eq!(progress.first(), Some(&(0, 2)));
        assert_eq!(progress.last(), Some(&(2, 2)));
        assert!(
            read_panel(&panel, &cancel)
                .unwrap()
                .directory
                .entries
                .is_empty()
        );
    }
}
