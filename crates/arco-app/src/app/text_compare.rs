use super::{App, JobContext, JobDescription, Outcome, job_progress::Sample};
use crate::commands::{Dialog, Pending};
use crate::{DiffData, OperationKind, display_path, human_size, locale::tr};
use arco_core::text_compare::{Options, Session, Stage};
use arco_core::tools::{BinaryComparison, BinaryPage};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

const EDIT_LIMIT: usize = 120_000;

impl App {
    pub(crate) fn compare_binary_interactive(&mut self, paths: [PathBuf; 2]) {
        self.load_binary_comparison(paths, false);
    }

    fn load_binary_comparison(&mut self, paths: [PathBuf; 2], must_be_open: bool) {
        self.ui().set_diff_notice("".into());
        self.start_operation_job(
            "Binärvergleich",
            false,
            description(paths.each_ref().map(PathBuf::as_path)),
            move |job| {
                let session = Arc::new(BinaryComparison::open(paths)?);
                let page = binary_page(&job, &session, 0, false)?;
                Ok(Outcome::BinaryCompared(session, page, must_be_open))
            },
        );
    }

    pub(super) fn show_binary_comparison(
        &mut self,
        session: Arc<BinaryComparison>,
        page: BinaryPage,
    ) {
        let paths = session.paths.each_ref().map(|path| display_path(path));
        let first = page.differences.first().map(|row| row.0);
        let last = page.differences.last().map(|row| row.0);
        let range = first.zip(last);
        let render = |side: usize| {
            let mut text = String::new();
            for (offset, bytes) in &page.differences {
                let byte = bytes[side].map_or_else(|| "--".into(), |value| format!("{value:02X}"));
                let character = bytes[side].map_or('—', |byte| {
                    if byte.is_ascii_graphic() {
                        char::from(byte)
                    } else {
                        '·'
                    }
                });
                writeln!(&mut text, "{offset:016X}   {byte}    {character}").unwrap();
            }
            text
        };
        let ui = self.ui();
        ui.set_diff_notice("".into());
        let info = session.lengths().map(human_size);
        ui.set_diff_data(DiffData {
            binary: true,
            left_path: paths[0].as_str().into(),
            right_path: paths[1].as_str().into(),
            left_range: tr("Offset (hex) · Byte · Zeichen").into(),
            right_range: tr("Offset (hex) · Byte · Zeichen").into(),
            left_info: info[0].as_str().into(),
            right_info: info[1].as_str().into(),
            left_text: render(0).into(),
            right_text: render(1).into(),
            summary: range
                .map_or_else(
                    || {
                        tr(if page.has_previous || page.has_next {
                            "Keine weiteren Byteabweichungen in diesem Suchbereich."
                        } else {
                            "Dateien sind bytegleich."
                        })
                        .into()
                    },
                    |(first, last)| {
                        format!(
                            "{}: {} · 0x{:X} – 0x{:X}",
                            tr("Abweichungen auf dieser Seite"),
                            page.differences.len(),
                            first,
                            last
                        )
                    },
                )
                .into(),
            has_previous: page.has_previous,
            has_next: page.has_next,
            ..Default::default()
        });
        ui.set_diff_revision(ui.get_diff_revision().wrapping_add(1));
        self.dialog(
            "text-comparison",
            Pending::BinaryCompare(session, range),
            Dialog {
                title: "Binärvergleich",
                ..Default::default()
            },
        );
    }

    pub(crate) fn compare_interactive(&mut self, paths: [PathBuf; 2]) {
        self.start_operation_job(
            "Dateien nach Inhalt vergleichen",
            false,
            description(paths.each_ref().map(PathBuf::as_path)),
            move |job| {
                let session = load_text(
                    &job,
                    paths.each_ref().map(PathBuf::as_path),
                    Options::default(),
                )?;
                Ok(Outcome::TextCompared(Arc::new(session), 0, false))
            },
        );
    }

    pub(super) fn show_text_comparison(&mut self, session: Arc<Session>, index: usize) {
        let index = index.min(session.changes.len().saturating_sub(1));
        let paths = session.paths().map(display_path);
        let mut data = DiffData {
            left_path: paths[0].as_str().into(),
            right_path: paths[1].as_str().into(),
            left_info: document_info(&session, 0).into(),
            right_info: document_info(&session, 1).into(),
            has_previous: index > 0,
            has_next: index + 1 < session.changes.len(),
            has_change: !session.changes.is_empty(),
            ignore_case: session.options.ignore_case,
            ignore_whitespace: session.options.ignore_whitespace,
            ignore_line_endings: session.options.ignore_line_endings,
            ..DiffData::default()
        };
        if let Some(change) = session.changes.get(index) {
            data.summary = format!(
                "{} {} / {}",
                tr("Unterschied"),
                index + 1,
                session.changes.len()
            )
            .into();
            data.left_range = line_range(&change[0]).into();
            data.right_range = line_range(&change[1]).into();
            let (left, editable_left) = excerpt(session.change_text(index, 0).unwrap_or_default());
            let (right, editable_right) =
                excerpt(session.change_text(index, 1).unwrap_or_default());
            data.left_text = left.into();
            data.right_text = right.into();
            data.editable_left = editable_left;
            data.editable_right = editable_right;
        } else {
            data.summary = tr("Keine Textunterschiede nach den gewählten Vergleichsregeln.").into();
        }
        let ui = self.ui();
        ui.set_diff_notice("".into());
        ui.set_diff_data(data);
        ui.set_diff_revision(ui.get_diff_revision().wrapping_add(1));
        self.dialog(
            "text-comparison",
            Pending::TextCompare(session, index),
            Dialog {
                title: "Dateien nach Inhalt vergleichen",
                ..Default::default()
            },
        );
    }

    pub(super) fn navigate_comparison(&mut self, delta: i32) {
        if self.job.is_some() {
            return;
        }
        if let Some(Pending::TextCompare(session, index)) = &self.pending {
            self.show_text_comparison(session.clone(), index.saturating_add_signed(delta as isize));
        }
        if let Some(Pending::BinaryCompare(session, Some((first, last)))) = &self.pending {
            let session = session.clone();
            let backward = delta < 0;
            let cursor = if backward {
                *first
            } else {
                last.saturating_add(1)
            };
            self.ui().set_diff_notice("".into());
            self.start_operation_job(
                "Binärvergleich",
                false,
                description(session.paths.each_ref().map(PathBuf::as_path)),
                move |job| {
                    let page = binary_page(&job, &session, cursor, backward)?;
                    Ok(Outcome::BinaryCompared(session, page, true))
                },
            );
        }
    }

    pub(super) fn update_comparison(&mut self, options: Options) {
        if self.job.is_some() {
            return;
        }
        if let Some(Pending::BinaryCompare(session, _)) = &self.pending {
            let paths = session.paths.clone();
            self.load_binary_comparison(paths, true);
            return;
        }
        let Some(Pending::TextCompare(session, index)) = &self.pending else {
            return;
        };
        let paths = session.paths().map(|path| path.to_path_buf());
        let index = *index;
        self.ui().set_diff_notice("".into());
        self.start_operation_job(
            "Dateien nach Inhalt vergleichen",
            false,
            description(paths.each_ref().map(PathBuf::as_path)),
            move |job| {
                let session = load_text(&job, paths.each_ref().map(PathBuf::as_path), options)?;
                Ok(Outcome::TextCompared(Arc::new(session), index, true))
            },
        );
    }

    pub(super) fn save_comparison_change(&mut self, side: i32, is_edit: bool, text: String) {
        if self.job.is_some() || !(0..=1).contains(&side) {
            return;
        }
        let Some(Pending::TextCompare(session, index)) = &self.pending else {
            return;
        };
        let side = side as usize;
        let index = *index;
        if is_edit
            && !session
                .change_text(index, side)
                .is_some_and(|value| value.len() <= EDIT_LIMIT)
        {
            self.status("Dieser Ausschnitt ist zu groß für die direkte Bearbeitung.");
            return;
        }
        let session = session.clone();
        if self.ui().get_diff_data().stale {
            return;
        }
        self.ui().set_diff_notice("".into());
        self.start_operation_job(
            "Änderung speichern",
            true,
            description(session.paths()),
            move |job| {
                job.phase("Zieldatei prüfen und Änderung speichern …");
                session.save_change(
                    index,
                    side,
                    is_edit.then_some(text.as_str()),
                    &job.cancel,
                    arco_windows::replace_preserving_attributes,
                )?;
                // A published write remains successful even if refreshing is cancelled or fails.
                match load_text(&job, session.paths(), session.options) {
                    Ok(refreshed) => Ok(Outcome::TextCompared(Arc::new(refreshed), index, true)),
                    Err(error) => Ok(Outcome::ComparisonSaved(
                        if error.kind() == std::io::ErrorKind::Interrupted {
                            "Auftrag abgebrochen.".into()
                        } else {
                            error.to_string()
                        },
                    )),
                }
            },
        );
    }

    pub(super) fn comparison_saved_without_refresh(&mut self, error: &str) {
        let ui = self.ui();
        if !matches!(self.pending, Some(Pending::TextCompare(..))) {
            return;
        }
        let mut data = ui.get_diff_data();
        data.stale = true;
        data.editable_left = false;
        data.editable_right = false;
        data.has_change = false;
        data.has_previous = false;
        data.has_next = false;
        ui.set_diff_data(data);
        ui.set_diff_notice(
            format!(
                "{} {}",
                tr("Änderung gespeichert. Die Vorschau ist veraltet; bitte neu vergleichen."),
                tr(error)
            )
            .into(),
        );
    }
}

fn description(paths: [&std::path::Path; 2]) -> JobDescription {
    JobDescription::new(
        OperationKind::Compare,
        display_path(paths[0]),
        display_path(paths[1]),
    )
}

fn document_info(session: &Session, side: usize) -> String {
    let info = session.document_info(side);
    format!(
        "{} · {} {} · {}",
        human_size(info.bytes),
        info.lines,
        tr("Zeilen"),
        info.encoding
    )
}

fn load_text(
    job: &JobContext,
    paths: [&std::path::Path; 2],
    options: Options,
) -> std::io::Result<Session> {
    Session::load_observed(paths, options, &job.cancel, |stage| {
        let (completed, phase, source) = match stage {
            Stage::ReadLeft => (0, "Linke Datei lesen …", display_path(paths[0])),
            Stage::ReadRight => (1, "Rechte Datei lesen …", display_path(paths[1])),
            Stage::Prepare => (2, "Vergleichsregeln anwenden …", String::new()),
            Stage::Compare => (3, "Textunterschiede ermitteln …", String::new()),
            Stage::Complete => (4, "Vergleich abgeschlossen", String::new()),
        };
        job.entry_progress(source, completed, 4, phase);
    })
}

fn binary_page(
    job: &JobContext,
    session: &BinaryComparison,
    cursor: u64,
    backward: bool,
) -> std::io::Result<BinaryPage> {
    let mut last = Instant::now() - Duration::from_secs(1);
    session.page_observed(cursor, backward, &job.cancel, |progress| {
        if progress.completed == 0
            || progress.completed == progress.total
            || last.elapsed() >= Duration::from_millis(80)
        {
            last = Instant::now();
            job.measured_progress(
                String::new(),
                Sample {
                    phase: if backward {
                        "Vorherige Byteabweichungen suchen …"
                    } else {
                        "Nächste Byteabweichungen suchen …"
                    }
                    .into(),
                    completed: progress.completed,
                    total: Some(progress.total),
                    ..Default::default()
                },
            );
        }
    })
}

fn line_range(lines: &std::ops::Range<usize>) -> String {
    if lines.is_empty() {
        format!("{} {}", tr("Einfügeposition vor Zeile"), lines.start + 1)
    } else {
        format!("{} {} – {}", tr("Zeilen"), lines.start + 1, lines.end)
    }
}

fn excerpt(text: &str) -> (String, bool) {
    if text.len() <= EDIT_LIMIT {
        return (text.to_owned(), true);
    }
    let mut end = EDIT_LIMIT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (
        format!("{}\n{}", &text[..end], tr("… Vorschau gekürzt")),
        false,
    )
}
