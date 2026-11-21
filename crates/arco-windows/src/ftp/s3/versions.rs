use super::{child, coordinates, encode_s3_path, etag, http, text};
use crate::ftp::{
    DownloadPhase, DownloadProgress, Security, Session, Url, check_cancel, invalid_input,
};
use std::collections::BTreeSet;
use std::io;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use tempfile::NamedTempFile;
use zeroize::Zeroizing;

mod restore;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct S3Version {
    pub id: String,
    pub modified: String,
    pub latest: bool,
    pub size: Option<u64>,
    tag: Option<String>,
}

impl S3Version {
    pub fn can_delete(&self) -> bool {
        // The null identifier can be reused by an upload while versioning is suspended.
        self.id != "null"
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum S3Versioning {
    Unversioned,
    Enabled,
    Suspended,
}

impl S3Versioning {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unversioned => "unversioned",
            Self::Enabled => "enabled",
            Self::Suspended => "suspended",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct S3VersioningConfig {
    pub status: S3Versioning,
    pub mfa_delete: bool,
}

impl S3VersioningConfig {
    pub fn requires_mfa(self, desired: Self) -> bool {
        self != desired && (self.mfa_delete || desired.mfa_delete)
    }
}

pub struct S3Mfa(Zeroizing<String>);

impl S3Mfa {
    pub fn new(serial: &str, code: &str) -> io::Result<Self> {
        let serial = serial.trim();
        let code = code.trim();
        if serial.is_empty() || serial.len() > 2048 || !serial.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err(invalid_input(
                "Gib die Seriennummer oder ARN des MFA-Geräts ein.",
            ));
        }
        if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid_input("Gib den sechsstelligen MFA-Code ein."));
        }
        Ok(Self(Zeroizing::new(format!("{serial} {code}"))))
    }

    pub fn require_https(url: &Url) -> io::Result<()> {
        if url.scheme() != "https" {
            return Err(invalid_input(
                "S3 MFA Delete benötigt eine HTTPS-Verbindung.",
            ));
        }
        Ok(())
    }

    fn authenticate(&self, request: &mut http::HttpRequest<'_>, url: &Url) -> io::Result<()> {
        Self::require_https(url)?;
        request.header("x-amz-mfa", &self.0)?;
        Ok(())
    }
}

impl Session {
    fn s3_version_bucket_url(&self, bucket: &str) -> io::Result<Url> {
        let mut url = self.directory(bucket)?;
        url.path_segments_mut()
            .map_err(|_| invalid_input("Ungültiger S3-Bucket."))?
            .pop_if_empty();
        Ok(url)
    }

    pub fn s3_bucket_versioning(
        &self,
        directory: &Url,
        cancel: &AtomicBool,
    ) -> io::Result<(String, S3Versioning)> {
        let (bucket, config) = self.s3_bucket_versioning_details(directory, cancel)?;
        Ok((bucket, config.status))
    }

    pub fn s3_bucket_versioning_details(
        &self,
        directory: &Url,
        cancel: &AtomicBool,
    ) -> io::Result<(String, S3VersioningConfig)> {
        let (bucket, url) = self.versioning_bucket(directory)?;
        Ok((bucket, self.read_s3_versioning_details(&url, cancel)?))
    }

    pub fn set_s3_bucket_versioning(
        &self,
        directory: &Url,
        expected: S3Versioning,
        status: S3Versioning,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let (_, url) = self.versioning_bucket(directory)?;
        let current = self.read_s3_versioning_details(&url, cancel)?;
        if current.status != expected {
            return Err(versioning_changed());
        }
        self.apply_s3_versioning(
            &url,
            current,
            S3VersioningConfig { status, ..current },
            None,
            cancel,
        )
    }

    pub fn update_s3_bucket_versioning(
        &self,
        directory: &Url,
        expected: S3VersioningConfig,
        desired: S3VersioningConfig,
        mfa: Option<&S3Mfa>,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let (_, url) = self.versioning_bucket(directory)?;
        if self.read_s3_versioning_details(&url, cancel)? != expected {
            return Err(versioning_changed());
        }
        self.apply_s3_versioning(&url, expected, desired, mfa, cancel)
    }

    fn apply_s3_versioning(
        &self,
        url: &Url,
        expected: S3VersioningConfig,
        desired: S3VersioningConfig,
        mfa: Option<&S3Mfa>,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        use base64::{Engine, engine::general_purpose::STANDARD};
        use md5::{Digest, Md5};

        let xml_status = match desired.status {
            S3Versioning::Enabled => "Enabled",
            S3Versioning::Suspended => "Suspended",
            S3Versioning::Unversioned => {
                return Err(invalid_input(
                    "S3 unterstützt Aktivieren und Aussetzen der Versionierung.",
                ));
            }
        };
        if expected == desired {
            return Ok(());
        }
        let mut request = self.http(url, "PUT")?;
        if let Some(mfa) = mfa {
            mfa.authenticate(&mut request, url)?;
        } else if expected.requires_mfa(desired) {
            return Err(invalid_input("Die Änderung benötigt einen S3-MFA-Code."));
        }
        let mfa_xml = match (expected.mfa_delete, desired.mfa_delete) {
            (false, true) => "<MfaDelete>Enabled</MfaDelete>",
            (true, false) => "<MfaDelete>Disabled</MfaDelete>",
            _ => "",
        };
        let body = format!(
            "<VersioningConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Status>{xml_status}</Status>{mfa_xml}</VersioningConfiguration>"
        );
        let digest = STANDARD.encode(Md5::digest(body.as_bytes()));
        request
            .query("versioning", "")
            .header("Content-Type", "application/xml")?
            .header("Content-MD5", &digest)?
            .body(&body)?;
        request.send(cancel)?.expect(&[200, 204])?;
        let current = self
            .read_s3_versioning_details(url, cancel)
            .map_err(|error| {
                io::Error::other(format!(
                    "S3 nahm die Änderung an. Der neue Status ließ sich nicht prüfen. {error}"
                ))
            })?;
        if current != desired {
            return Err(io::Error::other(
                "S3 hat den neuen Versionierungsstatus noch nicht bestätigt.",
            ));
        }
        Ok(())
    }

    fn versioning_bucket(&self, directory: &Url) -> io::Result<(String, Url)> {
        if self.security() != Security::S3 {
            return Err(invalid_input("Dieser Auftrag benötigt S3."));
        }
        self.validate(directory)?;
        let (bucket, _) = coordinates(directory)?;
        if bucket.is_empty() {
            return Err(invalid_input("Öffne einen S3-Bucket."));
        }
        let url = self.s3_version_bucket_url(&bucket)?;
        Ok((bucket, url))
    }

    fn read_s3_versioning(&self, url: &Url, cancel: &AtomicBool) -> io::Result<S3Versioning> {
        Ok(self.read_s3_versioning_details(url, cancel)?.status)
    }

    fn read_s3_versioning_details(
        &self,
        url: &Url,
        cancel: &AtomicBool,
    ) -> io::Result<S3VersioningConfig> {
        let mut request = self.http(url, "GET")?;
        request.query("versioning", "");
        let response = request.send(cancel)?.expect(&[200])?;
        let xml = http::xml(&response.body)?;
        let root = xml.root_element();
        let invalid = || invalid_input("Ungültiger Versionierungsstatus des S3-Buckets.");
        if !matches!(
            root.tag_name().name(),
            "VersioningConfiguration" | "GetBucketVersioningResponse"
        ) {
            return Err(invalid());
        }
        let status = match text(root, "Status") {
            Some("Enabled") => S3Versioning::Enabled,
            Some("Suspended") => S3Versioning::Suspended,
            None => S3Versioning::Unversioned,
            Some(_) => return Err(invalid()),
        };
        let mfa_delete = match text(root, "MfaDelete").or_else(|| text(root, "MFADelete")) {
            Some("Enabled") => true,
            Some("Disabled") | None => false,
            Some(_) => return Err(invalid()),
        };
        Ok(S3VersioningConfig { status, mfa_delete })
    }

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
        let bucket = self.s3_version_bucket_url(&bucket)?;
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
                let latest = match text(node, "IsLatest") {
                    Some("true") => true,
                    Some("false") => false,
                    _ => return Err(invalid_input("Der S3-Version fehlt ein gültiger Status.")),
                };
                versions.push(S3Version {
                    id: id.to_owned(),
                    modified: text(node, "LastModified")
                        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                        .map(|time| time.to_utc().format("%Y-%m-%d %H:%M:%S UTC").to_string())
                        .unwrap_or_default(),
                    latest,
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

    pub fn restore_s3_version(
        &self,
        directory: &Url,
        name: &str,
        version: &S3Version,
        cancel: &AtomicBool,
    ) -> io::Result<String> {
        self.restore_s3_version_with_progress(directory, name, version, cancel, |_| {})
    }

    pub fn restore_s3_version_with_progress(
        &self,
        directory: &Url,
        name: &str,
        version: &S3Version,
        cancel: &AtomicBool,
        mut progress: impl FnMut(DownloadProgress),
    ) -> io::Result<String> {
        use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};

        if self.security() != Security::S3 || version.latest || !valid_id(&version.id) {
            return Err(invalid_input("Wähle eine ältere S3-Dateiversion."));
        }
        let size = version
            .size
            .ok_or_else(|| invalid_input("S3-Löschmarkierungen enthalten keine Datei."))?;
        if version.id == "null" && version.tag.is_none() {
            return Err(invalid_input(
                "Für diese S3-Version fehlt eine eindeutige Objektkennung.",
            ));
        }
        let object = self.file(directory, name)?;
        let (bucket, key) = coordinates(&object)?;
        if key.is_empty() {
            return Err(invalid_input(
                "Die Wiederherstellung benötigt einen S3-Objektschlüssel.",
            ));
        }
        let bucket = self.s3_version_bucket_url(&bucket)?;
        if self.read_s3_versioning(&bucket, cancel)? != S3Versioning::Enabled {
            return Err(invalid_input(
                "Für die Wiederherstellung muss die S3-Versionierung aktiv sein.",
            ));
        }
        let source = format!(
            "{}?versionId={}",
            encode_s3_path(object.path()),
            utf8_percent_encode(&version.id, NON_ALPHANUMERIC)
        );
        if size > restore::MAX_COPY_PART {
            return restore::VersionCopy::new(self, &bucket, &object, version, size, &source)
                .run(cancel, &mut progress);
        }
        let mut report = |received| {
            progress(DownloadProgress {
                received,
                total: Some(size),
                phase: DownloadPhase::Receiving,
            });
        };
        report(0);
        let mut copy = self.http(&object, "PUT")?;
        copy.header("x-amz-copy-source", &source)?;
        if let Some(tag) = &version.tag {
            copy.header("x-amz-copy-source-if-match", tag)?;
        }
        let response = copy.send(cancel)?.expect(&[200])?;
        let xml = http::xml(&response.body)?;
        let result = xml.root_element();
        if result.has_tag_name("Error") {
            return Err(io::Error::other(format!(
                "S3 konnte die Version nicht kopieren. {}",
                text(result, "Code").unwrap_or("Unbekannter Fehler")
            )));
        }
        let confirmed_source = response.headers.get("x-amz-copy-source-version-id");
        if !result.has_tag_name("CopyObjectResult")
            || text(result, "ETag").and_then(etag).is_none()
            || confirmed_source.is_some_and(|source| source != &version.id)
        {
            return Err(io::Error::other(
                "Der Server hat die S3-Wiederherstellung nicht bestätigt.",
            ));
        }
        let id = restored_version_id(&response, &version.id)?;
        report(size);
        Ok(id)
    }

    pub fn delete_s3_version(
        &self,
        directory: &Url,
        name: &str,
        selected: &S3Version,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        self.delete_s3_version_authorized(directory, name, selected, None, cancel)
    }

    pub fn delete_s3_version_authorized(
        &self,
        directory: &Url,
        name: &str,
        selected: &S3Version,
        mfa: Option<&S3Mfa>,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        if mfa.is_some() {
            S3Mfa::require_https(directory)?;
        }
        if !selected.can_delete() || !valid_id(&selected.id) {
            return Err(invalid_input(
                "Das gezielte Löschen benötigt eine unveränderliche S3-Versions-ID.",
            ));
        }
        let versions = self.list_s3_versions(directory, name, cancel)?;
        let current = versions
            .iter()
            .find(|version| version.id == selected.id)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "Die gewählte S3-Version wurde nicht gefunden.",
                )
            })?;
        if current != selected {
            return Err(io::Error::other(
                "Die gewählte S3-Version hat sich seit der Auswahl geändert.",
            ));
        }
        let object = self.file(directory, name)?;
        let mut request = self.http(&object, "DELETE")?;
        request.query("versionId", &selected.id);
        if let Some(mfa) = mfa {
            mfa.authenticate(&mut request, &object)?;
        }
        let response = request.send(cancel)?.expect(&[204])?;
        if response
            .headers
            .get("x-amz-version-id")
            .is_some_and(|id| id != &selected.id)
            || response
                .headers
                .get("x-amz-delete-marker")
                .is_some_and(|marker| {
                    marker
                        != if selected.size.is_none() {
                            "true"
                        } else {
                            "false"
                        }
                })
        {
            return Err(io::Error::other(
                "S3 meldet eine andere Version als in der Löschanfrage.",
            ));
        }
        let after = self
            .list_s3_versions(directory, name, cancel)
            .map_err(|error| {
                io::Error::other(format!(
                    "S3 nahm die Löschanfrage an. Die Versionsliste ließ sich nicht prüfen. {error}"
                ))
            })?;
        if after.iter().any(|version| version.id == selected.id) {
            return Err(io::Error::other(
                "Die gewählte S3-Version ist nach dem Löschen noch vorhanden.",
            ));
        }
        Ok(())
    }
}

fn versioning_changed() -> io::Error {
    io::Error::other("Die S3-Versionierung wurde zwischenzeitlich geändert.")
}

fn restored_version_id(response: &http::Response, source_id: &str) -> io::Result<String> {
    response
        .headers
        .get("x-amz-version-id")
        .filter(|id| valid_id(id) && *id != "null" && *id != source_id)
        .cloned()
        .ok_or_else(|| io::Error::other("Der Server hat keine neue S3-Version bestätigt."))
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
        let bucket = session
            .relative_directory(&root, Path::new("arco-object-versions"))
            .unwrap();
        let local = tempfile::tempdir().unwrap();
        let source = local.path().join("source");
        let output = local.path().join("downloaded.txt");
        assert_eq!(
            session.s3_bucket_versioning(&bucket, &cancel).unwrap(),
            ("arco-object-versions".into(), S3Versioning::Unversioned)
        );
        std::fs::write(&source, b"legacy object").unwrap();
        session
            .upload(&bucket, &source, "legacy.txt", false, &cancel)
            .unwrap();
        session
            .set_s3_bucket_versioning(
                &bucket,
                S3Versioning::Unversioned,
                S3Versioning::Enabled,
                &cancel,
            )
            .unwrap();
        assert!(
            session
                .set_s3_bucket_versioning(
                    &bucket,
                    S3Versioning::Unversioned,
                    S3Versioning::Suspended,
                    &cancel
                )
                .is_err()
        );
        let legacy = session
            .list_s3_versions(&bucket, "legacy.txt", &cancel)
            .unwrap();
        assert_eq!(legacy[0].id, "null");
        assert!(
            session
                .delete_s3_version(&bucket, "legacy.txt", &legacy[0], &cancel)
                .is_err()
        );
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
        assert!(
            session
                .restore_s3_version(&bucket, name, marker, &cancel)
                .is_err()
        );
        let restored = session
            .restore_s3_version(&bucket, name, oldest, &cancel)
            .unwrap();
        let updated = session.list_s3_versions(&bucket, name, &cancel).unwrap();
        assert_eq!(updated.len(), versions.len() + 1);
        assert!(
            versions
                .iter()
                .all(|prior| updated.iter().any(|current| current.id == prior.id))
        );
        let current = updated.iter().find(|version| version.latest).unwrap();
        assert_eq!(current.id, restored);
        std::fs::remove_file(&output).unwrap();
        session.download(&bucket, name, &output, &cancel).unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), b"first revision");
        let visible = session
            .list_sync(&bucket, &cancel)
            .unwrap()
            .entries
            .into_iter()
            .find(|entry| entry.name == name)
            .unwrap();
        session
            .delete_sync_entry(&bucket, &visible, &cancel)
            .unwrap();
        let stale = session
            .delete_s3_version(&bucket, name, current, &cancel)
            .unwrap_err();
        assert!(stale.to_string().contains("seit der Auswahl"), "{stale}");
        let with_marker = session.list_s3_versions(&bucket, name, &cancel).unwrap();
        let latest_marker = with_marker.iter().find(|version| version.latest).unwrap();
        assert!(latest_marker.size.is_none());
        session
            .delete_s3_version(&bucket, name, latest_marker, &cancel)
            .unwrap();
        assert_eq!(
            session.list_s3_versions(&bucket, name, &cancel).unwrap(),
            updated
        );
        assert_eq!(
            session
                .delete_s3_version(&bucket, &similarly_named, oldest, &cancel)
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound,
        );
        session
            .delete_s3_version(&bucket, name, oldest, &cancel)
            .unwrap();
        let remaining = session.list_s3_versions(&bucket, name, &cancel).unwrap();
        assert_eq!(
            remaining,
            updated
                .into_iter()
                .filter(|version| version.id != oldest.id)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            session
                .delete_s3_version(&bucket, name, oldest, &cancel)
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound,
        );
        session
            .set_s3_bucket_versioning(
                &bucket,
                S3Versioning::Enabled,
                S3Versioning::Suspended,
                &cancel,
            )
            .unwrap();
        assert_eq!(
            session.s3_bucket_versioning(&bucket, &cancel).unwrap().1,
            S3Versioning::Suspended
        );
        let old_version = remaining
            .iter()
            .find(|version| !version.latest && version.size.is_some())
            .unwrap();
        let error = session
            .restore_s3_version(&bucket, name, old_version, &cancel)
            .unwrap_err();
        assert!(error.to_string().contains("Versionierung aktiv"), "{error}");
        assert_eq!(
            session.list_s3_versions(&bucket, name, &cancel).unwrap(),
            remaining
        );
        session
            .set_s3_bucket_versioning(
                &bucket,
                S3Versioning::Suspended,
                S3Versioning::Enabled,
                &cancel,
            )
            .unwrap();
    }
}
