use super::{App, Outcome};
use crate::commands::{Dialog, Pending};
use crate::{DiffData, display_path};
use arco_core::text_compare::{Options, Session};
use arco_core::tools::{BinaryComparison, BinaryPage};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

const EDIT_LIMIT: usize = 120_000;

impl App {
    pub(crate) fn compare_binary_interactive(&mut self, paths: [PathBuf; 2]) {
        self.start_job("Vergleiche Bytes …", false, move |job| {
            let session = Arc::new(BinaryComparison::open(paths)?);
            let page = session.page(0, false, &job.cancel)?;
            Ok(Outcome::BinaryCompared(session, page, false))
        });
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
                writeln!(&mut text, "{offset:016X}  {byte}").unwrap();
            }
            text
        };
        let ui = self.ui();
        ui.set_diff_data(DiffData {
            binary: true,
            left_path: paths[0].as_str().into(),
            right_path: paths[1].as_str().into(),
            left_range: "Offset (hex) · Byte".into(),
            right_range: "Offset (hex) · Byte".into(),
            left_text: render(0).into(),
            right_text: render(1).into(),
            summary: range
                .map_or_else(
                    || "Dateien sind bytegleich.".into(),
                    |(first, last)| {
                        format!(
                            "{} Byteabweichungen · {:X} bis {:X}",
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
        self.start_job("Vergleiche Textdateien …", false, move |job| {
            let session = Session::load(
                paths.each_ref().map(PathBuf::as_path),
                Options::default(),
                &job.cancel,
            )?;
            Ok(Outcome::TextCompared(Arc::new(session), 0, false))
        });
    }

    pub(super) fn show_text_comparison(&mut self, session: Arc<Session>, index: usize) {
        let index = index.min(session.changes.len().saturating_sub(1));
        let paths = session.paths().map(display_path);
        let mut data = DiffData {
            left_path: paths[0].as_str().into(),
            right_path: paths[1].as_str().into(),
            has_previous: index > 0,
            has_next: index + 1 < session.changes.len(),
            has_change: !session.changes.is_empty(),
            ignore_case: session.options.ignore_case,
            ignore_whitespace: session.options.ignore_whitespace,
            ignore_line_endings: session.options.ignore_line_endings,
            ..DiffData::default()
        };
        if let Some(change) = session.changes.get(index) {
            data.summary =
                format!("Unterschied {} von {}", index + 1, session.changes.len()).into();
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
            data.summary = "Keine Textunterschiede nach den gewählten Vergleichsregeln.".into();
        }
        let ui = self.ui();
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
            self.start_job("Suche Byteabweichungen …", false, move |job| {
                let page = session.page(cursor, backward, &job.cancel)?;
                Ok(Outcome::BinaryCompared(session, page, true))
            });
        }
    }

    pub(super) fn update_comparison(&mut self, options: Options) {
        if self.job.is_some() {
            return;
        }
        let Some(Pending::TextCompare(session, index)) = &self.pending else {
            return;
        };
        let paths = session.paths().map(|path| path.to_path_buf());
        let index = *index;
        self.start_job("Vergleiche Textdateien …", false, move |job| {
            let session =
                Session::load(paths.each_ref().map(PathBuf::as_path), options, &job.cancel)?;
            Ok(Outcome::TextCompared(Arc::new(session), index, true))
        });
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
        self.start_job(
            "Speichere die ausgewählte Änderung …",
            true,
            move |job| {
                session.save_change(
                    index,
                    side,
                    is_edit.then_some(text.as_str()),
                    &job.cancel,
                    arco_windows::replace_preserving_attributes,
                )?;
                let refreshed = Session::load(session.paths(), session.options, &job.cancel)?;
                Ok(Outcome::TextCompared(Arc::new(refreshed), index, true))
            },
        );
    }
}

fn line_range(lines: &std::ops::Range<usize>) -> String {
    if lines.is_empty() {
        format!("Einfügeposition vor Zeile {}", lines.start + 1)
    } else {
        format!("Zeilen {} bis {}", lines.start + 1, lines.end)
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
    (format!("{}\n… Vorschau gekürzt", &text[..end]), false)
}
