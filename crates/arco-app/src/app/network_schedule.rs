use super::{App, JobDescription, Outcome};
use crate::commands::{Dialog, Pending};
use arco_windows::task_schedule::{self, ScheduledScript, Task, Trigger};
use std::fmt::Write as _;
use std::io;
use std::path::PathBuf;
use zeroize::Zeroizing;

impl App {
    pub(crate) fn open_network_schedule(&mut self) {
        match arco_windows::sfx::file(crate::locale::tr("Netzwerkskript auswählen"), false, "") {
            Ok(Some(script)) => {
                if let Err(error) = self.configure_network_schedule(script) {
                    self.error("Netzwerkskript planen", &error.to_string());
                }
            }
            Ok(None) => (),
            Err(_) => self.dialog(
                "input",
                Pending::NetworkSchedulePath,
                Dialog {
                    title: "Netzwerkskript auswählen",
                    placeholder: "Absoluter Pfad zum UTF-8-Skript",
                    confirm_label: "Weiter",
                    ..Dialog::default()
                },
            ),
        }
    }

    pub(crate) fn configure_network_schedule(&mut self, script: PathBuf) -> io::Result<()> {
        if !script.is_absolute() {
            return Err(io::Error::other("Gib einen absoluten Skriptpfad ein."));
        }
        arco_core::files::regular_metadata(&script)?;
        self.dialog(
            "confirm",
            Pending::NetworkScheduleTrigger(script),
            Dialog {
                title: "Netzwerkskript planen",
                description: "Wähle, wann Windows das Skript starten soll.",
                options_label: "Aufgabenauslöser",
                options: &[
                    crate::locale::tr("Täglich"),
                    crate::locale::tr("Einmalig"),
                    crate::locale::tr("Wöchentlich"),
                    crate::locale::tr("Monatlich"),
                    crate::locale::tr("Bei Windows-Anmeldung"),
                    crate::locale::tr("Beim Systemstart"),
                ],
                confirm_label: "Weiter",
                ..Dialog::default()
            },
        );
        Ok(())
    }

    pub(crate) fn configure_schedule_trigger(
        &mut self,
        script: PathBuf,
        index: usize,
    ) -> io::Result<()> {
        let (kind, description, placeholder) = match index {
            0 => ("daily", "Gib die tägliche Uhrzeit ein.", "HH:MM"),
            1 => (
                "once",
                "Wähle einen einzelnen Zeitpunkt.",
                "JJJJ-MM-TT HH:MM",
            ),
            2 => (
                "weekly",
                "Gib Uhrzeit und gewünschte Wochentage ein.",
                "HH:MM Mo,Fr",
            ),
            3 => (
                "monthly",
                "Gib Uhrzeit und die Monatstage als Zahlen ein.",
                "HH:MM 1,15",
            ),
            4 => (
                "logon",
                "Windows startet die Aufgabe bei deiner Anmeldung.",
                "",
            ),
            5 => (
                "startup",
                "Der Systemstart benötigt eine Registrierung mit Administratorrechten und das Passwort des Windows-Kontos.",
                "",
            ),
            _ => return Err(io::Error::other("Wähle einen Aufgabenauslöser.")),
        };
        let logon_modes = [
            crate::locale::tr("Nur während der Windows-Anmeldung"),
            crate::locale::tr("Auch ohne Windows-Anmeldung"),
        ];
        self.dialog(
            "input",
            Pending::NetworkScheduleCreate(script.clone(), kind),
            Dialog {
                title: "Netzwerkskript planen",
                description,
                value: script
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or_default(),
                placeholder: "Aufgabenname",
                second_placeholder: placeholder,
                options: if kind == "startup" { &[] } else { &logon_modes },
                confirm_label: "Weiter",
                ..Dialog::default()
            },
        );
        Ok(())
    }

    pub(crate) fn ask_schedule_password(&mut self, script: PathBuf, name: &str, trigger: Trigger) {
        self.dialog("input", Pending::NetworkScheduleBackground(script, name.to_owned(), trigger), Dialog {
            title: "Auch ohne Windows-Anmeldung ausführen",
            description: "Die Windows-Aufgabenplanung benötigt das Passwort deines aktuellen Windows-Kontos.",
            placeholder: "Windows-Kontopasswort",
            secret: true,
            confirm_label: "Aufgabe anlegen",
            ..Dialog::default()
        });
    }

    pub(crate) fn create_network_schedule(
        &mut self,
        script: PathBuf,
        name: &str,
        trigger: Trigger,
        windows_password: Option<Zeroizing<String>>,
    ) -> io::Result<()> {
        let executable = std::env::current_exe()?.with_file_name("arco-net.exe");
        let profile = std::path::absolute(&self.profile_path)?;
        let log = profile
            .parent()
            .map(|directory| directory.join("Netzwerkprotokolle"));
        let name = name.to_owned();
        let description = JobDescription::new(
            crate::OperationKind::Ftp,
            crate::display_path(&script),
            name.clone(),
        );
        self.start_operation_job("Netzwerkskript planen", false, description, move |job| {
            arco_core::files::check_cancel(&job.cancel)?;
            let task = ScheduledScript {
                name: &name,
                trigger,
                executable: &executable,
                script: &script,
                profile: &profile,
                log: log.as_deref(),
            };
            if let Some(password) = windows_password {
                task.register_background(&password)?;
            } else {
                task.register()?;
            }
            Ok(Outcome::ViewerNotice(format!(
                "Windows-Aufgabe „{name}“ angelegt."
            )))
        });
        Ok(())
    }

    pub(crate) fn list_network_schedule(&mut self) {
        let description = JobDescription::new(
            crate::OperationKind::Ftp,
            "Windows-Aufgabenplanung".into(),
            String::new(),
        );
        self.start_operation_job("Geplante Netzwerkskripte", false, description, move |job| {
            arco_core::files::check_cancel(&job.cancel)?;
            Ok(Outcome::ScheduledNetworkTasks(task_schedule::list()?))
        });
    }

    pub(super) fn review_network_schedule(&mut self, tasks: Vec<Task>) {
        if tasks.is_empty() {
            self.remember_information(
                "Geplante Netzwerkskripte",
                "Für Arco Commander sind keine Windows-Aufgaben vorhanden.",
            );
            return;
        }
        let mut description = String::new();
        let names: Vec<_> = tasks
            .into_iter()
            .map(|task| {
                let state = match task.state {
                    "disabled" => "Deaktiviert",
                    "queued" => "Wartet",
                    "ready" => "Bereit",
                    "running" => "Läuft",
                    _ => "Unbekannt",
                };
                let _ = writeln!(
                    description,
                    "{} · {} · {} · 0x{:08X}",
                    task.name,
                    crate::locale::tr(if task.background {
                        "Auch ohne Windows-Anmeldung"
                    } else {
                        "Nur während der Windows-Anmeldung"
                    }),
                    crate::locale::tr(state),
                    task.last_result as u32
                );
                task.name
            })
            .collect();
        let options: Vec<_> = names.iter().map(String::as_str).collect();
        self.dialog(
            "confirm",
            Pending::NetworkScheduleChoose(names.clone()),
            Dialog {
                title: "Geplante Netzwerkskripte",
                description: &description,
                options: &options,
                options_label: "Windows-Aufgabe",
                confirm_label: "Weiter",
                ..Dialog::default()
            },
        );
    }

    pub(crate) fn choose_network_schedule_action(&mut self, name: &str) {
        self.dialog(
            "confirm",
            Pending::NetworkScheduleAction(name.to_owned()),
            Dialog {
                title: "Windows-Aufgabe verwalten",
                description: name,
                options: &[
                    crate::locale::tr("Jetzt ausführen"),
                    crate::locale::tr("Aufgabe entfernen"),
                ],
                confirm_label: "Weiter",
                ..Dialog::default()
            },
        );
    }

    pub(crate) fn confirm_network_schedule_action(
        &mut self,
        name: String,
        option: usize,
    ) -> io::Result<()> {
        if option == 0 {
            return self.operate_network_schedule(name, false);
        }
        let message = format!("Soll die Windows-Aufgabe „{name}“ entfernt werden?");
        self.dialog(
            "confirm",
            Pending::NetworkScheduleRemove(name),
            Dialog {
                title: "Aufgabe entfernen",
                description: &message,
                confirm_label: "Entfernen",
                ..Dialog::default()
            },
        );
        Ok(())
    }

    pub(crate) fn operate_network_schedule(
        &mut self,
        name: String,
        remove: bool,
    ) -> io::Result<()> {
        let label = if remove {
            "Aufgabe entfernen"
        } else {
            "Jetzt ausführen"
        };
        let description =
            JobDescription::new(crate::OperationKind::Ftp, name.clone(), String::new());
        self.start_operation_job(label, false, description, move |job| {
            arco_core::files::check_cancel(&job.cancel)?;
            if remove {
                task_schedule::remove(&name)?;
            } else {
                task_schedule::run(&name)?;
            }
            Ok(Outcome::ViewerNotice(if remove {
                format!("Windows-Aufgabe „{name}“ entfernt.")
            } else {
                format!("Windows-Aufgabe „{name}“ übergeben.")
            }))
        });
        Ok(())
    }
}
