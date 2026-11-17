use super::{App, JobContext, JobDescription, Outcome, job_progress::Sample};
use crate::{
    ReviewRow, UtilityData, UtilityForm, UtilityKind, UtilityState,
    commands::{Dialog, HASHES, Pending},
    display_path,
    locale::tr,
};
use arco_core::{
    files,
    io_progress::{Phase, Progress},
    tc_split,
    tools::{self, Hash},
    transport_encoding::{self as codec, Format},
};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{self, Write},
    path::{Path, PathBuf},
};

#[derive(Clone)]
pub(crate) enum Task {
    Hash {
        paths: Vec<PathBuf>,
        base: PathBuf,
        verify: bool,
    },
    Expected {
        path: PathBuf,
        expected: String,
        methods: Vec<Hash>,
    },
    Split {
        source: PathBuf,
        target: PathBuf,
    },
    Join {
        paths: Vec<PathBuf>,
        target: PathBuf,
        manifest: Option<tc_split::Manifest>,
    },
    Codec {
        paths: Vec<PathBuf>,
        target: PathBuf,
        decode: bool,
    },
}

pub(crate) struct Setup {
    pub task: Task,
    report: String,
}
pub(crate) struct Report {
    rows: Vec<Row>,
    text: String,
    completed: usize,
    failed: usize,
    cancelled: bool,
}
struct Row {
    source: String,
    value: String,
    detail: String,
    error: bool,
}
impl Default for Report {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            text: String::new(),
            completed: 0,
            failed: 0,
            cancelled: false,
        }
    }
}
impl Report {
    fn success(&mut self, source: &Path, value: String, detail: String) {
        self.completed += 1;
        self.rows.push(Row {
            source: display_path(source),
            value,
            detail,
            error: false,
        });
    }
    fn error(&mut self, source: &Path, error: io::Error) {
        if error.kind() == io::ErrorKind::Interrupted {
            self.cancelled = true;
        } else {
            self.failed += 1;
            self.rows.push(Row {
                source: display_path(source),
                value: "Fehler".into(),
                detail: error.to_string(),
                error: true,
            });
        }
    }
}

impl Task {
    fn paths(&self) -> Vec<PathBuf> {
        match self {
            Self::Hash { paths, .. } | Self::Join { paths, .. } | Self::Codec { paths, .. } => {
                paths.clone()
            }
            Self::Expected { path, .. } => vec![path.clone()],
            Self::Split { source, .. } => vec![source.clone()],
        }
    }
    fn kind(&self) -> UtilityKind {
        match self {
            Self::Hash { verify: true, .. } => UtilityKind::Verify,
            Self::Hash { .. } => UtilityKind::Hash,
            Self::Expected { .. } => UtilityKind::Expected,
            Self::Split { .. } => UtilityKind::Split,
            Self::Join { .. } => UtilityKind::Join,
            Self::Codec { decode: true, .. } => UtilityKind::Decode,
            Self::Codec { .. } => UtilityKind::Encode,
        }
    }
    fn title(&self) -> &'static str {
        match self.kind() {
            UtilityKind::Hash => "Prüfsummen erzeugen",
            UtilityKind::Verify => "Prüfsummendatei prüfen",
            UtilityKind::Expected => "Prüfsumme aus Zwischenablage prüfen",
            UtilityKind::Split => "Datei teilen",
            UtilityKind::Join => "Dateien zusammenfügen",
            UtilityKind::Encode => "Dateien kodieren",
            UtilityKind::Decode => "Dateien dekodieren",
        }
    }
}

impl App {
    pub(crate) fn open_utility(&mut self, task: Task) {
        if self.job.is_some() {
            self.status("Ein Auftrag läuft bereits.");
            return;
        }
        let paths = task.paths();
        if paths.is_empty() {
            self.status("Wähle Dateien.");
            return;
        }
        let mut initial = UtilityForm {
            part_size: "64".into(),
            ..Default::default()
        };
        let (options, description) = match &task {
            Task::Hash {
                base,
                verify,
                paths,
            } => {
                initial.target = display_path(base).into();
                if *verify {
                    initial.option = paths[0]
                        .extension()
                        .and_then(|e| e.to_str())
                        .and_then(Hash::parse)
                        .and_then(|method| {
                            HASHES
                                .iter()
                                .position(|name| Hash::parse(name) == Some(method))
                        })
                        .unwrap_or(0) as i32;
                }
                (
                    HASHES.to_vec(),
                    if *verify {
                        "Die Dateien einer Prüfsummenliste prüfen. Dateipfade außerhalb ihres Ordners werden abgewiesen."
                    } else {
                        "Ausgewählte Dateien berechnen und optional eine neue Prüfsummenliste speichern."
                    },
                )
            }
            Task::Expected {
                expected, methods, ..
            } => {
                initial.expected = expected.as_str().into();
                (
                    methods.iter().map(hash_name).collect(),
                    "Die erwartete Prüfsumme bleibt vor dem Start sichtbar und bearbeitbar.",
                )
            }
            Task::Split { target, .. } => {
                initial.target = display_path(target).into();
                (
                    vec!["Total Commander (.crc)", "Nur Dateiteile"],
                    "TC erzeugt Basisname.001 und eine CRC-Begleitdatei; reine Dateiteile verwenden Originalname.001. Der Satz wird erst nach vollständigem Lesen veröffentlicht.",
                )
            }
            Task::Join {
                target, manifest, ..
            } => {
                initial.target = display_path(target).into();
                initial.output = manifest
                    .as_ref()
                    .map(|m| m.name.as_str())
                    .unwrap_or("joined.bin")
                    .into();
                if manifest.is_some() {
                    (
                        vec!["CRC-Satz automatisch vervollständigen"],
                        "Die CRC-Begleitdatei bestimmt Originalname, vollständigen Teilesatz, Größe und Prüfsumme. Erst nach erfolgreicher Prüfung wird gespeichert.",
                    )
                } else {
                    (
                        vec!["Lexikografische Reihenfolge"],
                        "Die angezeigten Quellen werden in lexikografischer Reihenfolge verbunden. Vorhandene Ausgaben bleiben erhalten.",
                    )
                }
            }
            Task::Codec { target, decode, .. } => {
                initial.target = display_path(target).into();
                (
                    if *decode {
                        vec![
                            "Automatisch nach Dateiendung",
                            "UUE",
                            "XXE",
                            "MIME",
                            "BinHex",
                        ]
                    } else {
                        vec!["UUE", "XXE", "MIME"]
                    },
                    "Transportkodierungen für Dateien. Zusammengehörige UUE-/XXE-Teile und MIME-Nachrichtenteile werden gemeinsam verarbeitet. BinHex ist zum Dekodieren verfügbar.",
                )
            }
        };
        let sources = paths
            .iter()
            .enumerate()
            .map(|(index, path)| ReviewRow {
                primary: display_path(path).into(),
                secondary: (index + 1).to_string().into(),
                detail: "".into(),
                kind: "file".into(),
            })
            .collect::<Vec<_>>();
        self.ui().set_utility_data(UtilityData {
            kind: task.kind(),
            title: tr(task.title()).into(),
            description: tr(description).into(),
            sources: slint::ModelRc::new(slint::VecModel::from(sources)),
            options: super::strings(options),
            initial,
        });
        self.ui().set_utility_state(UtilityState::default());
        self.dialog(
            "utility",
            Pending::Utility(Box::new(Setup {
                task,
                report: String::new(),
            })),
            Dialog::default(),
        );
    }

    pub(crate) fn prepare_utility_join(&mut self, mut paths: Vec<PathBuf>, target: PathBuf) {
        if paths.is_empty() {
            self.status("Wähle die zu verbindenden Dateien.");
            return;
        }
        paths.sort();
        self.start_operation_job(
            "Dateiteile erfassen",
            false,
            JobDescription::new(
                crate::OperationKind::Utility,
                super::properties::selection(&paths),
                display_path(&target),
            ),
            move |job| {
                job.phase("CRC-Begleitdatei lesen …");
                files::check_cancel(&job.cancel)?;
                let manifest = tc_split::selected_manifest(&paths)?;
                files::check_cancel(&job.cancel)?;
                Ok(Outcome::UtilityReady(Task::Join {
                    paths,
                    target,
                    manifest,
                }))
            },
        );
    }

    pub(super) fn submit_utility(&mut self, form: UtilityForm) {
        if self.job.is_some() {
            return;
        }
        let Some(Pending::Utility(setup)) = &self.pending else {
            return;
        };
        let task = setup.task.clone();
        if let Err((field, message)) = validate(&task, &form) {
            self.ui().set_utility_state(UtilityState {
                notice: tr(&message).into(),
                error_field: field,
                ..Default::default()
            });
            return;
        }
        let target = PathBuf::from(form.target.as_str());
        let output = form.output.to_string();
        let expected = form.expected.to_string();
        let option = form.option as usize;
        let part_size = parse_size(&form.part_size).unwrap_or(0);
        let refresh = matches!(
            task,
            Task::Split { .. } | Task::Join { .. } | Task::Codec { .. }
        ) || matches!(task, Task::Hash { verify: false, .. }) && !output.is_empty();
        let description = JobDescription::new(
            crate::OperationKind::Utility,
            super::properties::selection(&task.paths()),
            if refresh {
                display_path(&target)
            } else {
                String::new()
            },
        );
        self.ui().set_utility_state(UtilityState::default());
        self.start_operation_job(task.title(), refresh, description, move |job| {
            let throttle = ToolProgress::new(&job, &task);
            let result = run(
                &task, &target, &output, &expected, option, part_size, &job, &throttle,
            );
            let report = result.unwrap_or_else(|error| {
                let mut report = Report::default();
                report.error(&task.paths()[0], error);
                report
            });
            Ok(Outcome::UtilityReport(report))
        });
    }

    pub(super) fn show_utility_report(&mut self, report: Report) {
        let Some(Pending::Utility(setup)) = &mut self.pending else {
            return;
        };
        let summary = format!(
            "{} · {}: {} · {}: {}",
            tr(if report.cancelled {
                "Abgebrochen"
            } else {
                "Abgeschlossen"
            }),
            tr("Erfolgreich"),
            report.completed,
            tr("Fehler"),
            report.failed
        );
        setup.report = if report.text.is_empty() {
            format!(
                "{summary}\n{}",
                report
                    .rows
                    .iter()
                    .map(|r| format!("{} · {}\n{}", r.source, tr(&r.value), r.detail))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        } else {
            report.text
        };
        let rows = report
            .rows
            .into_iter()
            .map(|r| ReviewRow {
                primary: r.source.into(),
                secondary: tr(&r.value).into(),
                detail: r.detail.into(),
                kind: if r.error { "error" } else { "file" }.into(),
            })
            .collect::<Vec<_>>();
        self.ui().set_utility_state(UtilityState {
            summary: summary.into(),
            rows: slint::ModelRc::new(slint::VecModel::from(rows)),
            ..Default::default()
        });
    }

    pub(super) fn utility_action(&self, action: i32) {
        let Some(Pending::Utility(setup)) = &self.pending else {
            return;
        };
        if action == 1 {
            self.report(arco_windows::copy_text(&setup.report));
        }
    }
}

fn hash_name(method: &Hash) -> &'static str {
    HASHES
        .iter()
        .copied()
        .find(|name| Hash::parse(name).as_ref() == Some(method))
        .unwrap_or("SHA256")
}
fn parse_size(value: &str) -> Option<u64> {
    value
        .trim()
        .parse::<u64>()
        .ok()?
        .checked_mul(1024 * 1024)
        .filter(|v| *v > 0)
}
fn validate(task: &Task, form: &UtilityForm) -> Result<(), (i32, String)> {
    let options = match task {
        Task::Hash { .. } => HASHES.len(),
        Task::Expected { methods, .. } => methods.len(),
        Task::Split { .. } => 2,
        Task::Join { .. } => 1,
        Task::Codec { decode, .. } => {
            if *decode {
                5
            } else {
                3
            }
        }
    };
    if form.option < 0 || form.option as usize >= options {
        return Err((4, "Wähle ein gültiges Verfahren.".into()));
    }
    if matches!(task, Task::Split { .. }) && parse_size(&form.part_size).is_none() {
        return Err((
            3,
            "Teilgröße muss eine positive ganze Zahl in MiB sein.".into(),
        ));
    }
    if matches!(task, Task::Join { .. })
        || matches!(task, Task::Hash { verify: false, .. }) && !form.output.is_empty()
    {
        tools::output_name(&form.output).map_err(|e| (2, e.to_string()))?;
    }
    let writes = matches!(
        task,
        Task::Split { .. } | Task::Join { .. } | Task::Codec { .. }
    ) || matches!(task, Task::Hash { verify: false, .. }) && !form.output.is_empty();
    if writes && form.target.trim().is_empty() {
        return Err((1, "Gib einen Zielordner an.".into()));
    }
    if let Task::Expected { methods, .. } = task
        && !methods[form.option as usize].matches_text(form.expected.trim())
    {
        return Err((
            5,
            "Die erwartete Prüfsumme passt nicht zum gewählten Algorithmus.".into(),
        ));
    }
    Ok(())
}

struct ToolProgress {
    latest: std::sync::Arc<std::sync::Mutex<Option<Sample>>>,
    order: BTreeMap<PathBuf, (usize, usize)>,
}
impl ToolProgress {
    fn new(job: &JobContext, task: &Task) -> Self {
        let paths = task.paths();
        let count_known = matches!(
            task,
            Task::Hash { verify: false, .. }
                | Task::Expected { .. }
                | Task::Split { .. }
                | Task::Join { manifest: None, .. }
        );
        let order = if count_known {
            paths
                .iter()
                .enumerate()
                .map(|(index, path)| (path.clone(), (index + 1, paths.len())))
                .collect()
        } else {
            BTreeMap::new()
        };
        Self {
            latest: job.progress_feed(),
            order,
        }
    }
    fn observe(&self, p: Progress<'_>) {
        if let Ok(mut latest) = self.latest.lock() {
            *latest = Some(Sample {
                source: display_path(p.source),
                phase: match p.phase {
                    Phase::Reading => "Quelldaten lesen …",
                    Phase::Processing => "Daten prüfen und verarbeiten …",
                    Phase::Publishing => "Ausgaben prüfen und speichern …",
                }
                .into(),
                completed: p.completed,
                total: p.total,
                items: self
                    .order
                    .get(p.source)
                    .map(|&(index, total)| (index, Some(total))),
                processed_items: self
                    .order
                    .get(p.source)
                    .map(|&(index, _)| index.saturating_sub(1)),
                ..Default::default()
            });
        }
    }
}

fn run(
    task: &Task,
    target: &Path,
    output: &str,
    expected: &str,
    option: usize,
    part_size: u64,
    job: &JobContext,
    progress: &ToolProgress,
) -> io::Result<Report> {
    let observe = |p: Progress<'_>| progress.observe(p);
    let mut report = Report::default();
    match task {
        Task::Hash {
            paths,
            base,
            verify: true,
        } => {
            let method = Hash::parse(HASHES[option]).unwrap();
            let body = tools::verify_checksums_observed(&paths[0], method, &job.cancel, &observe)?;
            for line in body.lines() {
                let (status, rest) = line.split_once(' ').unwrap_or(("FEHLER", line));
                let okay = status == "OK";
                if okay {
                    report.completed += 1;
                } else {
                    report.failed += 1;
                }
                report.rows.push(Row {
                    source: rest.trim().into(),
                    value: if okay {
                        "OK"
                    } else if status == "ABWEICHUNG" {
                        "Abweichung"
                    } else {
                        "Fehler"
                    }
                    .into(),
                    detail: line.into(),
                    error: !okay,
                });
            }
            if report.rows.is_empty() {
                report.error(
                    &paths[0],
                    io::Error::other("Die Liste enthält keine prüfbaren Einträge."),
                );
            }
            let _ = base;
            report.text = body;
        }
        Task::Hash {
            paths,
            base,
            verify: false,
        } => {
            let method = Hash::parse(HASHES[option]).unwrap();
            for path in paths {
                if files::check_cancel(&job.cancel).is_err() {
                    report.cancelled = true;
                    break;
                }
                let result = (|| {
                    let name = path
                        .strip_prefix(base)
                        .map_err(io::Error::other)?
                        .to_string_lossy();
                    if name.contains(['\n', '\r']) {
                        return Err(io::Error::other(
                            "Ungültiger Dateiname für eine Prüfsummenliste.",
                        ));
                    }
                    let hash = tools::checksum_file_observed(
                        File::open(path)?,
                        method,
                        &job.cancel,
                        &|p| observe(Progress { source: path, ..p }),
                    )?;
                    if method == Hash::Crc32 {
                        report.text.push_str(&format!("{name} {hash}\n"));
                    } else {
                        report.text.push_str(&format!("{hash} *{name}\n"));
                    }
                    report.success(path, hash, HASHES[option].into());
                    Ok(())
                })();
                if let Err(error) = result {
                    report.error(path, error);
                }
                if report.cancelled {
                    break;
                }
            }
            if !output.is_empty() && !report.cancelled && report.failed == 0 {
                let path = target.join(tools::output_name(output)?);
                observe(Progress {
                    source: &path,
                    completed: 0,
                    total: None,
                    phase: Phase::Publishing,
                });
                let saved = (|| -> io::Result<()> {
                    let mut staged = tempfile::NamedTempFile::new_in(target)?;
                    staged.write_all(report.text.as_bytes())?;
                    staged.as_file().sync_all()?;
                    files::check_cancel(&job.cancel)?;
                    staged.persist_noclobber(&path).map_err(|e| e.error)?;
                    Ok(())
                })();
                match saved {
                    Ok(()) => report.rows.push(Row {
                        source: display_path(&path),
                        value: "Gespeichert".into(),
                        detail: "".into(),
                        error: false,
                    }),
                    Err(error) => report.error(&path, error),
                }
            }
            if report.cancelled || report.failed > 0 {
                report.text = format!(
                    "{}\n{}",
                    "Unvollständiges Ergebnis – keine Prüfsummenliste gespeichert.", report.text
                );
            }
        }
        Task::Expected { path, methods, .. } => {
            let actual = tools::checksum_file_observed(
                File::open(path)?,
                methods[option],
                &job.cancel,
                &|p| observe(Progress { source: path, ..p }),
            )?;
            let okay = actual.eq_ignore_ascii_case(expected.trim());
            report.completed = usize::from(okay);
            report.failed = usize::from(!okay);
            report.rows.push(Row {
                source: display_path(path),
                value: if okay {
                    "Prüfsumme stimmt"
                } else {
                    "Prüfsumme weicht ab"
                }
                .into(),
                detail: format!(
                    "{}: {}\n{}: {actual}",
                    "Erwartet",
                    expected.trim(),
                    "Berechnet"
                ),
                error: !okay,
            });
        }
        Task::Split { source, .. } => {
            let count = if option == 0 {
                tc_split::split_observed(source, target, part_size, &job.cancel, &observe)?
            } else {
                tools::split_observed(source, target, part_size, &job.cancel, &observe)?.len()
            };
            report.success(
                source,
                "Erstellt".into(),
                format!("{} · {count} {}", display_path(target), "Dateiteile"),
            );
        }
        Task::Join {
            paths, manifest, ..
        } => {
            let path = target.join(tools::output_name(output)?);
            if let Some(manifest) = manifest {
                manifest.join_observed(&path, &job.cancel, &observe)?;
            } else {
                tools::join_observed(paths, &path, &job.cancel, &observe)?;
            }
            report.success(&path, "Erstellt".into(), String::new());
        }
        Task::Codec { paths, decode, .. } => {
            let selected = match (*decode, option) {
                (true, 0) => None,
                (true, 2) | (false, 1) => Some(Format::Xxe),
                (true, 3) | (false, 2) => Some(Format::Mime),
                (true, 4) => Some(Format::BinHex),
                _ => Some(Format::Uue),
            };
            let mut armored = Vec::new();
            let mut mime = Vec::new();
            for path in paths {
                if files::check_cancel(&job.cancel).is_err() {
                    report.cancelled = true;
                    break;
                }
                let format = selected.or_else(|| Format::from_path(path));
                if *decode && let Some(format @ (Format::Uue | Format::Xxe)) = format {
                    armored.push((path.clone(), format));
                    continue;
                }
                if *decode && matches!(format, Some(Format::Mime)) {
                    mime.push(path.clone());
                    continue;
                }
                let result = format
                    .ok_or_else(|| io::Error::other("Wähle die Kodierung für diese Dateiendung."))
                    .and_then(|format| {
                        if *decode {
                            codec::decode_observed(path, target, format, &job.cancel, &observe)
                        } else {
                            codec::encode_observed(path, target, format, &job.cancel, &observe)
                                .map(|p| vec![p])
                        }
                    });
                match result {
                    Ok(outputs) => report.success(
                        path,
                        "Erstellt".into(),
                        outputs
                            .iter()
                            .map(|p| display_path(p))
                            .collect::<Vec<_>>()
                            .join("\n"),
                    ),
                    Err(error) => report.error(path, error),
                }
                if report.cancelled {
                    break;
                }
            }
            if !report.cancelled {
                for (source, result) in
                    codec::decode_armored_batch_observed(&armored, target, &job.cancel, &observe)
                {
                    match result {
                        Ok(()) => report.success(&source, "Dekodiert".into(), display_path(target)),
                        Err(error) => report.error(&source, error),
                    }
                }
            }
            if !report.cancelled {
                for (source, result) in
                    codec::decode_mime_batch_observed(&mime, target, &job.cancel, &observe)
                {
                    match result {
                        Ok(outputs) => report.success(
                            &source,
                            "Erstellt".into(),
                            outputs
                                .iter()
                                .map(|p| display_path(p))
                                .collect::<Vec<_>>()
                                .join("\n"),
                        ),
                        Err(error) => report.error(&source, error),
                    }
                }
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn utility_progress_coalesces_chunks_and_keeps_hashes_after_output_collision() {
        use std::sync::{Arc, atomic::AtomicBool, mpsc};
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("input.txt");
        let output = fixture.path().join("list.sha256");
        std::fs::write(&source, b"abc").unwrap();
        std::fs::write(&output, b"existing").unwrap();
        let task = Task::Hash {
            paths: vec![source.clone()],
            base: fixture.path().to_path_buf(),
            verify: false,
        };
        let (sender, receiver) = mpsc::channel();
        let job = JobContext {
            archives: Default::default(),
            sender,
            cancel: Arc::new(AtomicBool::new(false)),
        };
        let progress = ToolProgress::new(&job, &task);
        for completed in 0..10_000 {
            progress.observe(Progress {
                source: &source,
                completed,
                total: Some(10_000),
                phase: Phase::Reading,
            });
        }
        let events = receiver.try_iter().collect::<Vec<_>>();
        assert_eq!(events.len(), 1);
        let super::super::Event::ProgressFeed(feed) = &events[0] else {
            panic!("expected a coalesced feed");
        };
        assert_eq!(feed.lock().unwrap().as_ref().unwrap().completed, 9999);
        let report = run(
            &task,
            fixture.path(),
            "list.sha256",
            "",
            0,
            0,
            &job,
            &progress,
        )
        .unwrap();
        assert_eq!(report.completed, 1);
        assert_eq!(report.failed, 1);
        assert_eq!(std::fs::read(output).unwrap(), b"existing");
        assert_eq!(std::fs::read_dir(fixture.path()).unwrap().count(), 2);
        assert!(
            report.rows.iter().any(|row| row.value
                == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
    }
    #[test]
    fn utility_validation_preserves_optional_hash_output_and_rejects_bad_targets() {
        let task = Task::Hash {
            paths: vec![PathBuf::from("file")],
            base: PathBuf::from("."),
            verify: false,
        };
        assert!(validate(&task, &UtilityForm::default()).is_ok());
        assert_eq!(
            validate(
                &task,
                &UtilityForm {
                    output: "../escape".into(),
                    ..Default::default()
                }
            )
            .unwrap_err()
            .0,
            2
        );
        let split = Task::Split {
            source: "file".into(),
            target: ".".into(),
        };
        for size in ["0", "-1", "1.5", "18446744073709551615"] {
            assert_eq!(
                validate(
                    &split,
                    &UtilityForm {
                        part_size: size.into(),
                        ..Default::default()
                    }
                )
                .unwrap_err()
                .0,
                3
            );
        }
        assert_eq!(parse_size("64"), Some(64 * 1024 * 1024));
        let expected = Task::Expected {
            path: "file".into(),
            expected: String::new(),
            methods: vec![Hash::Sha256],
        };
        assert_eq!(
            validate(
                &expected,
                &UtilityForm {
                    expected: "bad".into(),
                    ..Default::default()
                }
            )
            .unwrap_err()
            .0,
            5
        );
    }
}
