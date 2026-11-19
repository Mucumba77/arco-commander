use super::{child, coordinates, etag, http, text};
use crate::ftp::{
    DownloadPhase, DownloadProgress, Security, Session, Url, check_cancel, invalid_input,
};
use std::collections::BTreeSet;
use std::io;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use tempfile::NamedTempFile;

pub struct S3Version {
    pub id: String,
    pub modified: String,
    pub latest: bool,
    pub size: Option<u64>,
    tag: Option<String>,
}

impl Session {
    pub fn list_s3_versions(
        &self,
        directory: &Url,
        name: &str,
        cancel: &AtomicBool,
    ) -> io::Result<Vec<S3Version>> {
        if self.security() != Security::S3 {
            return Err(invalid_input("Dieser Auftrag benötigt S3."));
        }
        let object = self.file(directory, name)?;
        let (bucket, key) = coordinates(&object)?;
        if key.is_empty() {
            return Err(invalid_input(
                "Öffne einen S3-Bucket oder ein Präfix für die Versionshistorie.",
            ));
        }
        let mut bucket = self.directory(&bucket)?;
        bucket
            .path_segments_mut()
            .map_err(|_| invalid_input("Ungültiger S3-Bucket."))?
            .pop_if_empty();
        let mut versions = Vec::new();
        let mut ids = BTreeSet::new();
        let mut markers: Option<(String, String)> = None;
        for _ in 0..256 {
            let mut request = self.http(&bucket, "GET")?;
            request
                .query("versions", "")
                .query("prefix", &key)
                .query("encoding-type", "url");
            if let Some((key, id)) = &markers {
                request.query("key-marker", key);
                if !id.is_empty() {
                    request.query("version-id-marker", id);
                }
            }
            let response = request.send(cancel)?.expect(&[200])?;
            let xml = http::xml(&response.body)?;
            let root = xml.root_element();
            if !root.has_tag_name("ListVersionsResult") {
                return Err(invalid_input("Ungültige S3-Versionsliste."));
            }
            let encoded = text(root, "EncodingType") == Some("url");
            for node in root.children().filter(roxmltree::Node::is_element) {
                let deleted = node.has_tag_name("DeleteMarker");
                if !deleted && !node.has_tag_name("Version") {
                    continue;
                }
                check_cancel(cancel)?;
                if decoded(text(node, "Key").unwrap_or_default(), encoded)? != key {
                    continue;
                }
                let id = text(node, "VersionId")
                    .filter(|id| valid_id(id))
                    .ok_or_else(|| invalid_input("Ungültige S3-Versions-ID."))?;
                if !ids.insert(id.to_owned()) {
                    return Err(invalid_input(
                        "Doppelte S3-Versions-ID in der Serverantwort.",
                    ));
                }
                let size = if deleted {
                    None
                } else {
                    Some(
                        text(node, "Size")
                            .and_then(|value| value.parse().ok())
                            .ok_or_else(|| {
                                invalid_input("In der S3-Versionsliste fehlt eine Objektgröße.")
                            })?,
                    )
                };
                versions.push(S3Version {
                    id: id.to_owned(),
                    modified: text(node, "LastModified")
                        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                        .map(|time| time.to_utc().format("%Y-%m-%d %H:%M:%S UTC").to_string())
                        .unwrap_or_default(),
                    latest: text(node, "IsLatest") == Some("true"),
                    size,
                    tag: text(node, "ETag").and_then(etag).map(str::to_owned),
                });
                if versions.len() > 20_000 {
                    return Err(invalid_input("Die S3-Datei hat zu viele Versionen."));
                }
            }
            if text(root, "IsTruncated") != Some("true") {
                return Ok(versions);
            }
            let marker = text(root, "NextKeyMarker")
                .filter(|value| !value.is_empty())
                .ok_or_else(|| invalid_input("S3-Folgeseite für Objektversionen fehlt."))?;
            let id = child(root, "NextVersionIdMarker")
                .and_then(|node| node.text())
                .unwrap_or_default();
            if !id.is_empty() && !valid_id(id) {
                return Err(invalid_input(
                    "Ungültige S3-Versions-ID auf der Folgeseite.",
                ));
            }
            let next = (decoded(marker, encoded)?.into_owned(), id.to_owned());
            if markers.as_ref() == Some(&next) {
                return Err(invalid_input(
                    "Die S3-Versionsliste enthält eine wiederholte Folgeseite.",
                ));
            }
            markers = Some(next);
        }
        Err(invalid_input(
            "Zu viele Folgeseiten in der S3-Versionsliste.",
        ))
    }

    pub fn download_s3_version(
        &self,
        directory: &Url,
        name: &str,
        version: &S3Version,
        destination: &Path,
        cancel: &AtomicBool,
        mut progress: impl FnMut(DownloadProgress),
    ) -> io::Result<()> {
        if self.security() != Security::S3 || !valid_id(&version.id) {
            return Err(invalid_input("Wähle eine gültige S3-Objektversion."));
        }
        let size = version
            .size
            .ok_or_else(|| invalid_input("S3-Löschmarkierungen enthalten keine Datei."))?;
        if destination.try_exists()? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                destination.display().to_string(),
            ));
        }
        let url = self.file(directory, name)?;
        let mut request = self.http(&url, "GET")?;
        request.query("versionId", &version.id);
        if let Some(tag) = &version.tag {
            request.header("If-Match", tag)?;
        } else if version.id == "null" {
            return Err(invalid_input(
                "Für diese S3-Version fehlt eine eindeutige Objektkennung.",
            ));
        }
        let parent = destination
            .parent()
            .ok_or_else(|| invalid_input("Der lokale Zielordner fehlt."))?;
        let temporary = NamedTempFile::new_in(parent)?;
        let output = temporary.as_file();
        let response = request.receive_file(output, Some(size), cancel, || {
            progress(DownloadProgress {
                received: output.metadata()?.len(),
                total: Some(size),
                phase: DownloadPhase::Receiving,
            });
            Ok(())
        })?;
        if version.id != "null" && response.headers.get("x-amz-version-id") != Some(&version.id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Der Server hat die angeforderte S3-Version nicht bestätigt.",
            ));
        }
        if output.metadata()?.len() != size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "S3-Objektversion hat eine unerwartete Größe.",
            ));
        }
        progress(DownloadProgress {
            received: size,
            total: Some(size),
            phase: DownloadPhase::Verifying,
        });
        output.sync_all()?;
        check_cancel(cancel)?;
        temporary
            .persist_noclobber(destination)
            .map_err(|error| error.error)?;
        Ok(())
    }
}

fn decoded(value: &str, encoded: bool) -> io::Result<std::borrow::Cow<'_, str>> {
    if encoded {
        percent_encoding::percent_decode_str(value)
            .decode_utf8()
            .map_err(io::Error::other)
    } else {
        Ok(value.into())
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 1024 && !id.chars().any(char::is_control)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::ftp::Connection;

    #[test]
    #[ignore = "requires scripts/test-native-http-reference.py"]
    fn reference_native_http_s3_versions() {
        let cancel = AtomicBool::new(false);
        let session = Connection::new(
            &std::env::var("ARCO_TEST_S3").unwrap(),
            "ARCOACCESS",
            Security::S3,
        )
        .unwrap()
        .with_s3_region("eu-central-1")
        .unwrap()
        .authenticate("fixture-only")
        .unwrap();
        let root = session.initial_directory();
        session
            .create_sync_directory(&root, "arco-object-versions", &cancel)
            .unwrap();
        let mut bucket = session
            .relative_directory(&root, Path::new("arco-object-versions"))
            .unwrap();
        bucket.path_segments_mut().unwrap().pop_if_empty();
        let mut enable = session.http(&bucket, "PUT").unwrap();
        enable.query("versioning", "").header("Content-Type", "application/xml").unwrap()
            .body(r#"<VersioningConfiguration xmlns="http://s3.amazonaws.com/doc/2006-03-01/"><Status>Enabled</Status></VersioningConfiguration>"#).unwrap();
        enable.send(&cancel).unwrap().expect(&[200]).unwrap();
        let bucket = session
            .relative_directory(&root, Path::new("arco-object-versions"))
            .unwrap();
        let local = tempfile::tempdir().unwrap();
        let source = local.path().join("source");
        let output = local.path().join("downloaded.txt");
        let name = "Büro & Team.txt";
        for (index, data) in ["first revision", "second revision", "third revision"]
            .into_iter()
            .enumerate()
        {
            std::fs::write(&source, data).unwrap();
            session
                .upload(&bucket, &source, name, index != 0, &cancel)
                .unwrap();
        }
        let similarly_named = format!("{name}.backup");
        session
            .upload(&bucket, &source, &similarly_named, false, &cancel)
            .unwrap();
        let latest = session
            .list_sync(&bucket, &cancel)
            .unwrap()
            .entries
            .into_iter()
            .find(|entry| entry.name == name)
            .unwrap();
        session
            .delete_sync_entry(&bucket, &latest, &cancel)
            .unwrap();
        let versions = session.list_s3_versions(&bucket, name, &cancel).unwrap();
        assert_eq!(versions.len(), 4);
        let marker = versions.iter().find(|version| version.latest).unwrap();
        assert!(marker.size.is_none());
        assert!(
            session
                .download_s3_version(&bucket, name, marker, &output, &cancel, |_| {})
                .is_err()
        );
        let oldest = versions
            .iter()
            .filter(|version| version.size.is_some())
            .last()
            .unwrap();
        session
            .download_s3_version(&bucket, name, oldest, &output, &cancel, |_| {})
            .unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), b"first revision");
        assert_eq!(
            session
                .download_s3_version(&bucket, name, oldest, &output, &cancel, |_| {})
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
    }
}
