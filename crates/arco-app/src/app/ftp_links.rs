use super::{App, JobDescription, Outcome};
use crate::{
    OperationKind,
    commands::{Dialog, Pending},
    locale::tr,
};
use arco_core::files::TransferReport;
use arco_windows::ftp::RemoteLinkKind;

impl App {
    pub(crate) fn open_remote_link(&mut self, edit: bool) {
        let tab = self.current();
        let Some(ftp) = &tab.ftp else {
            self.status("Öffne zuerst ein verbundenes Server-Dateifenster.");
            return;
        };
        let session = &ftp.location.session;
        let security = session.security();
        let ssh = security.is_ssh();
        if !(ssh || !edit && security.is_ftp() && !session.is_http_forward_ftp()) {
            self.status(if edit {
                "Symbolische Serverlinks können mit SFTP oder SCP bearbeitet werden."
            } else {
                "Serverlinks benötigen SFTP, SCP oder FTP mit SITE SYMLINK."
            });
            return;
        }
        let name = tab
            .display_row(tab.cursor)
            .and_then(|row| ftp.entry(&row.path))
            .filter(|entry| !edit || entry.symlink)
            .map(|entry| entry.name.clone());
        let endpoint = (session.clone(), ftp.location.directory.clone());
        if edit {
            let Some(name) = name else {
                self.status("Wähle einen symbolischen Serverlink aus.");
                return;
            };
            self.start_operation_job(
                "Linkziel lesen …",
                false,
                JobDescription::new(OperationKind::Ftp, name.clone(), String::new()),
                move |job| {
                    let target = endpoint
                        .0
                        .remote_link_target(&endpoint.1, &name, &job.cancel)?;
                    Ok(Outcome::RemoteLinkLoaded(endpoint, name, target))
                },
            );
        } else {
            let folder = endpoint.1.to_string();
            self.dialog(
                "input",
                Pending::MakeRemoteLink(endpoint),
                Dialog {
                    title: "Serverlink anlegen",
                    description: &folder,
                    placeholder: "Name des neuen Links",
                    second: name.as_deref().unwrap_or_default(),
                    second_placeholder: "Linkziel auf dem Server",
                    options_label: "Linktyp",
                    options: if ssh {
                        &["Symbolischer Link", "Harter Link"]
                    } else {
                        &[]
                    },
                    confirm_label: "Anlegen",
                    ..Default::default()
                },
            );
        }
    }

    pub(super) fn show_remote_link_target(
        &mut self,
        endpoint: super::FtpEndpoint,
        name: String,
        target: String,
    ) {
        let description = format!("{} · {name}", endpoint.1);
        let previous = target.clone();
        self.dialog(
            "input",
            Pending::ChangeRemoteLink(endpoint, name, previous),
            Dialog {
                title: "Symbolischen Serverlink bearbeiten",
                description: &description,
                value: &target,
                placeholder: "Linkziel auf dem Server",
                confirm_label: "Speichern",
                ..Default::default()
            },
        );
    }

    pub(crate) fn make_remote_link(
        &mut self,
        endpoint: super::FtpEndpoint,
        name: &str,
        target: &str,
        option: usize,
    ) {
        let name = name.to_owned();
        let target = target.to_owned();
        let kind = if option == 1 {
            RemoteLinkKind::Hard
        } else {
            RemoteLinkKind::Symbolic
        };
        self.start_operation_job(
            "Serverlink anlegen",
            true,
            JobDescription::new(OperationKind::Ftp, target.clone(), name.clone()),
            move |job| {
                endpoint
                    .0
                    .create_remote_link(&endpoint.1, &name, &target, kind, &job.cancel)?;
                Ok(link_report("Serverlink angelegt"))
            },
        );
    }

    pub(crate) fn change_remote_link(
        &mut self,
        endpoint: super::FtpEndpoint,
        name: String,
        previous: String,
        target: &str,
    ) {
        if target == previous {
            return;
        }
        let target = target.to_owned();
        self.start_operation_job(
            "Linkziel ändern",
            true,
            JobDescription::new(OperationKind::Ftp, name.clone(), target.clone()),
            move |job| {
                endpoint.0.update_remote_link(
                    &endpoint.1,
                    &name,
                    &previous,
                    &target,
                    &job.cancel,
                )?;
                Ok(link_report("Linkziel gespeichert"))
            },
        );
    }
}

fn link_report(title: &str) -> Outcome {
    Outcome::Report(
        tr(title).into(),
        TransferReport {
            completed: 1,
            ..Default::default()
        },
    )
}
