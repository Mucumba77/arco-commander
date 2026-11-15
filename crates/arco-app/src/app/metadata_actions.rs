use super::{App, Outcome};
use crate::commands::{Dialog, Pending};
use crate::display_path;
use arco_core::{advanced_search, descriptions, files};
use arco_windows::MetadataEdit;
use std::fs;
use std::io;
use std::path::PathBuf;

pub(super) fn apply_description_attributes(
    file: &fs::File,
    existing: Option<&fs::Metadata>,
) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FLAGS: u32 = 2 | 4 | 32;
        let set = existing.map_or(2 | 32, |meta| meta.file_attributes() & FLAGS);
        arco_windows::edit_file_metadata(
            file,
            MetadataEdit {
                set,
                clear: FLAGS & !set,
                ..Default::default()
            },
        )
    }
    #[cfg(not(windows))]
    {
        let _ = (file, existing);
        Ok(())
    }
}

#[cfg(all(test, windows))]
#[test]
fn comment_transfer_updates_a_hidden_windows_sidecar() {
    use std::os::windows::fs::MetadataExt;
    use std::sync::atomic::AtomicBool;
    let fixture = tempfile::tempdir().unwrap();
    let source = fixture.path().join("source");
    let target = fixture.path().join("target");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&target).unwrap();
    let paths = [source.join("first.txt"), source.join("second.txt")];
    for path in &paths {
        fs::write(path, "input").unwrap();
        descriptions::set(path, None, "Beispiel", apply_description_attributes).unwrap();
    }
    let report = files::transfer(
        &paths,
        &target,
        files::Transfer::Copy,
        files::TransferPolicy {
            descriptions: Some(apply_description_attributes),
            ..files::Conflict::Fail.into()
        },
        &AtomicBool::new(false),
        None,
        |_| {},
    )
    .unwrap();
    assert!(report.errors.is_empty(), "{report:?}");
    assert_eq!(report.completed, 2);
    assert_eq!(descriptions::read(&target).unwrap().len(), 2);
    assert_ne!(
        fs::metadata(target.join("descript.ion"))
            .unwrap()
            .file_attributes()
            & 2,
        0
    );
}

impl App {
    pub(crate) fn open_comment(&mut self) {
        let Some(entry) = self
            .current()
            .current()
            .filter(|entry| matches!(entry.kind, files::Kind::File | files::Kind::Directory))
        else {
            self.status("Wähle eine lokale Datei oder einen Ordner für den Kommentar.");
            return;
        };
        let path = entry.path.clone();
        self.start_job("Lese Dateikommentar …", false, move |_| {
            let parent = path
                .parent()
                .ok_or_else(|| io::Error::other("Der Elternordner fehlt."))?;
            let comments = descriptions::read(parent)?;
            let comment = comments
                .get(&descriptions::key(path.file_name().unwrap_or_default()))
                .cloned();
            Ok(Outcome::FileComment(path, comment))
        });
    }

    pub(super) fn show_comment(&mut self, path: PathBuf, comment: Option<String>) {
        let description = format!(
            "{}\nEin leeres Feld entfernt den Kommentar.",
            display_path(&path)
        );
        let text = comment.clone().unwrap_or_default();
        self.dialog(
            "multiline",
            Pending::EditComment(path, comment),
            Dialog {
                title: "Dateikommentar bearbeiten",
                description: &description,
                value: &text,
                ..Default::default()
            },
        );
    }

    pub(crate) fn write_comment(
        &mut self,
        path: PathBuf,
        previous: Option<String>,
        comment: String,
    ) {
        self.start_job("Speichere Dateikommentar …", true, move |_| {
            descriptions::set(
                &path,
                previous.as_deref(),
                &comment,
                apply_description_attributes,
            )?;
            Ok(Outcome::Notice("Dateikommentar gespeichert.".into()))
        });
    }

    pub(crate) fn open_metadata_edit(&mut self) {
        let paths = self.current().paths();
        if paths.is_empty() {
            self.status("Wähle lokale Dateien oder Ordner.");
            return;
        }
        self.open_metadata_paths(paths);
    }

    pub(crate) fn apply_metadata_edit(
        &mut self,
        roots: Vec<PathBuf>,
        value: &str,
    ) -> io::Result<()> {
        let (edit, recursive) = metadata_options(value)?;
        if edit.set | edit.clear == 0
            && edit.created.is_none()
            && edit.modified.is_none()
            && edit.accessed.is_none()
        {
            return Err(io::Error::other("Gib mindestens eine Änderung an."));
        }
        self.start_metadata_job(roots, edit, recursive);
        Ok(())
    }
}

fn metadata_options(text: &str) -> io::Result<(MetadataEdit, bool)> {
    let mut edit = MetadataEdit::default();
    let mut recursive = false;
    let parse_time =
        |value: &str| -> io::Result<_> { Ok(Some(super::metadata_edit::parse_time(value)?)) };
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let (key, value) = line.split_once('=').ok_or_else(|| {
            io::Error::other("Verwende eine Einstellung pro Zeile in der Form Name=Wert.")
        })?;
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        match key.trim().to_lowercase().as_str() {
            "attribute" => (edit.set, edit.clear) = advanced_search::attributes(value)?,
            "geändert" => edit.modified = parse_time(value)?,
            "erstellt" => edit.created = parse_time(value)?,
            "zugriff" => edit.accessed = parse_time(value)?,
            "rekursiv" if value.eq_ignore_ascii_case("ja") => recursive = true,
            "rekursiv" if value.eq_ignore_ascii_case("nein") => recursive = false,
            _ => return Err(io::Error::other(format!("Unbekannte Einstellung: {key}"))),
        }
    }
    Ok((edit, recursive))
}
