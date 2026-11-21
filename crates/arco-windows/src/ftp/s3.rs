use super::{
    Connection, Entry, ListFormat, Listing, Security, Session, Url, check_cancel, check_password,
    check_segment, http, invalid_input, webdav::etag,
};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use tempfile::NamedTempFile;
use zeroize::Zeroizing;

mod multipart;
mod versions;
pub use versions::{S3Mfa, S3Version, S3Versioning, S3VersioningConfig};

const PART: u64 = 16 * 1024 * 1024;
const MAX_OBJECT: u64 = 5 * 1024 * 1024 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct S3Tags {
    pub tags: BTreeMap<String, String>,
    version: Option<String>,
}

impl Connection {
    pub fn with_s3_request_options(
        mut self,
        virtual_host: bool,
        requester_pays: bool,
    ) -> io::Result<Self> {
        if self.security != Security::S3 {
            return Err(invalid_input(
                "Die gewählten Verbindungsoptionen benötigen S3.",
            ));
        }
        if virtual_host && !matches!(self.url.host(), Some(url::Host::Domain(_))) {
            return Err(invalid_input(
                "S3-Virtual-Host-Adressierung benötigt einen DNS-Namen.",
            ));
        }
        self.s3_virtual_host = virtual_host;
        self.s3_requester_pays = requester_pays;
        Ok(self)
    }

    pub(super) fn s3_wire_url(&self, url: &Url) -> io::Result<Url> {
        let mut wire = url.clone();
        if !self.s3_virtual_host || url.path() == "/" {
            wire.set_path(&encode_s3_path(url.path()));
            return Ok(wire);
        }
        let path = url.path().trim_start_matches('/');
        let (bucket, key) = path.split_once('/').unwrap_or((path, ""));
        if bucket.is_empty() {
            return Ok(wire);
        }
        let (valid, _) = coordinates(url)?;
        if !valid.split('.').all(|label| {
            label
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
        }) {
            return Err(invalid_input(
                "Der Bucket-Name eignet sich nicht für einen S3-Virtual-Host.",
            ));
        }
        let host = url
            .host_str()
            .ok_or_else(|| invalid_input("S3-Servername fehlt."))?;
        wire.set_host(Some(&format!("{bucket}.{host}")))
            .map_err(|_| invalid_input("Ungültiger S3-Virtual-Host."))?;
        wire.set_path(&encode_s3_path(&format!("/{key}")));
        Ok(wire)
    }

    pub fn with_s3_region(mut self, region: &str) -> io::Result<Self> {
        let region = region.trim();
        if self.security != Security::S3
            || region.len() > 64
            || !region
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(invalid_input("Ungültige S3-Region."));
        }
        if !region.is_empty() {
            self.s3_region = region.to_owned();
        }
        Ok(self)
    }
}

impl Session {
    pub fn s3_tags(
        &self,
        directory: &Url,
        entry: &Entry,
        cancel: &AtomicBool,
    ) -> io::Result<S3Tags> {
        if self.security() != Security::S3 || entry.directory != Some(false) {
            return Err(invalid_input("Wähle ein S3-Objekt."));
        }
        self.verify_sync_entry(directory, entry, cancel)?;
        let url = self.file(directory, &entry.name)?;
        let tags = self.read_s3_tags(&url, None, cancel)?;
        self.verify_sync_entry(directory, entry, cancel)?;
        Ok(tags)
    }

    fn read_s3_tags(
        &self,
        url: &Url,
        version: Option<&str>,
        cancel: &AtomicBool,
    ) -> io::Result<S3Tags> {
        let mut request = self.http(url, "GET")?;
        request.query("tagging", "");
        if let Some(id) = version {
            request.query("versionId", id);
        }
        let response = request.send(cancel)?.expect(&[200])?;
        let xml = http::xml(&response.body)?;
        let root = xml.root_element();
        let set = child(root, "TagSet")
            .filter(|_| root.has_tag_name("Tagging"))
            .ok_or_else(|| invalid_input("Ungültige S3-Tag-Liste."))?;
        let mut tags = BTreeMap::new();
        for tag in set.children().filter(|node| node.has_tag_name("Tag")) {
            let key = text(tag, "Key").ok_or_else(|| invalid_input("S3-Tag ohne Schlüssel."))?;
            let value = child(tag, "Value").ok_or_else(|| invalid_input("S3-Tag ohne Wert."))?;
            if tags
                .insert(key.to_owned(), value.text().unwrap_or_default().to_owned())
                .is_some()
            {
                return Err(invalid_input("Doppelter S3-Tag-Schlüssel."));
            }
        }
        Ok(S3Tags {
            tags,
            version: response.headers.get("x-amz-version-id").cloned(),
        })
    }

    pub fn replace_s3_tags(
        &self,
        directory: &Url,
        entry: &Entry,
        before: &S3Tags,
        tags: &BTreeMap<String, String>,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        if tags.len() > 10
            || tags.iter().any(|(key, value)| {
                key.trim().is_empty()
                    || key.starts_with("aws:")
                    || key.encode_utf16().count() > 128
                    || value.encode_utf16().count() > 256
                    || key.chars().chain(value.chars()).any(char::is_control)
            })
        {
            return Err(invalid_input(
                "S3 erlaubt bis zu zehn Tags mit gültigen Schlüsseln und Werten.",
            ));
        }
        if &self.s3_tags(directory, entry, cancel)? != before {
            return Err(io::Error::other(
                "Die S3-Tags wurden seit dem Öffnen geändert.",
            ));
        }
        let url = self.file(directory, &entry.name)?;
        let mut request = self.http(&url, if tags.is_empty() { "DELETE" } else { "PUT" })?;
        request.query("tagging", "");
        if let Some(version) = &before.version {
            request.query("versionId", version);
        }
        if !tags.is_empty() {
            use base64::{Engine, engine::general_purpose::STANDARD};
            use md5::{Digest, Md5};
            use std::fmt::Write as _;
            let mut body =
                "<Tagging xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><TagSet>".to_owned();
            for (key, value) in tags {
                write!(
                    body,
                    "<Tag><Key>{}</Key><Value>{}</Value></Tag>",
                    escape_xml(key),
                    escape_xml(value)
                )
                .expect("format tags XML");
            }
            body.push_str("</TagSet></Tagging>");
            let digest = STANDARD.encode(Md5::digest(body.as_bytes()));
            request
                .header("Content-Type", "application/xml")?
                .header("Content-MD5", &digest)?
                .body(&body)?;
        }
        request.send(cancel)?.expect(&[200, 204]).map(|_| ())
    }

    pub(super) fn load_s3_credentials(&mut self) -> io::Result<()> {
        if self.connection.s3_aws_profile.is_some() {
            return Ok(());
        }
        if self.connection.user.is_empty() {
            let connection = std::sync::Arc::get_mut(&mut self.connection)
                .ok_or_else(|| io::Error::other("Die S3-Sitzung wurde bereits geteilt."))?;
            connection.user = std::env::var("AWS_ACCESS_KEY_ID")
                .map_err(|_| invalid_input("Der S3-Zugriffsschlüssel fehlt."))?;
            self.password = std::sync::Arc::new(Zeroizing::new(
                std::env::var("AWS_SECRET_ACCESS_KEY")
                    .map_err(|_| invalid_input("Der geheime S3-Schlüssel fehlt."))?,
            ));
            self.s3_token = std::env::var("AWS_SESSION_TOKEN")
                .ok()
                .filter(|token| !token.is_empty())
                .map(|token| std::sync::Arc::new(Zeroizing::new(token)));
        }
        if self.connection.user.is_empty()
            || !self
                .connection
                .user
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
            || self.password.is_empty()
        {
            return Err(invalid_input("Ungültige oder fehlende S3-Zugangsdaten."));
        }
        check_password(&self.password)
    }

    pub(super) fn list_s3(&self, directory: &Url, cancel: &AtomicBool) -> io::Result<Listing> {
        let (bucket, prefix) = coordinates(directory)?;
        if bucket.is_empty() {
            let response = self.http(directory, "GET")?.send(cancel)?.expect(&[200])?;
            let xml = http::xml(&response.body)?;
            let root = xml.root_element();
            let root = child(root, "Buckets")
                .ok_or_else(|| invalid_input("Die S3-Antwort enthält keine Bucket-Liste."))?;
            let mut entries = Vec::new();
            for item in root.children().filter(|item| item.has_tag_name("Bucket")) {
                let name =
                    text(item, "Name").ok_or_else(|| invalid_input("S3-Bucket ohne Namen."))?;
                check_segment(name)?;
                entries.push(directory_entry(name));
            }
            return Ok(Listing {
                entries,
                format: ListFormat::S3,
            });
        }
        let mut bucket_url = self.directory(&bucket)?;
        bucket_url
            .path_segments_mut()
            .map_err(|_| invalid_input("Ungültiger S3-Bucket."))?
            .pop_if_empty();
        let mut continuation: Option<String> = None;
        let mut entries = BTreeMap::<String, Entry>::new();
        for _ in 0..256 {
            let mut request = self.http(&bucket_url, "GET")?;
            request
                .query("list-type", "2")
                .query("delimiter", "/")
                .query("prefix", &prefix)
                .query("encoding-type", "url");
            if let Some(token) = &continuation {
                request.query("continuation-token", token);
            }
            let response = request.send(cancel)?.expect(&[200])?;
            let xml = http::xml(&response.body)?;
            let root = xml.root_element();
            if !root.has_tag_name("ListBucketResult") {
                return Err(invalid_input("Ungültige S3-Verzeichnisliste."));
            }
            let encoded = text(root, "EncodingType") == Some("url");
            for item in root.children().filter(roxmltree::Node::is_element) {
                check_cancel(cancel)?;
                let directory = item.has_tag_name("CommonPrefixes");
                if !directory && !item.has_tag_name("Contents") {
                    continue;
                }
                let key = text(item, if directory { "Prefix" } else { "Key" })
                    .ok_or_else(|| invalid_input("S3-Eintrag ohne Namen."))?;
                let key = if encoded {
                    percent_encoding::percent_decode_str(key)
                        .decode_utf8()
                        .map_err(io::Error::other)?
                } else {
                    key.into()
                };
                if key == prefix {
                    continue;
                }
                let name = key
                    .strip_prefix(&prefix)
                    .ok_or_else(|| invalid_input("S3-Eintrag außerhalb des Präfixes."))?;
                let name = if directory {
                    name.strip_suffix('/')
                        .ok_or_else(|| invalid_input("Ungültiger S3-Ordner."))?
                } else {
                    name
                };
                check_segment(name)?;
                let entry = if directory {
                    directory_entry(name)
                } else {
                    Entry {
                        name: name.to_owned(),
                        directory: Some(false),
                        symlink: false,
                        size: Some(
                            text(item, "Size")
                                .and_then(|value| value.parse().ok())
                                .ok_or_else(|| invalid_input("S3-Objekt ohne Größe."))?,
                        ),
                        modified: text(item, "LastModified")
                            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                            .map(|value| value.to_utc().format("%Y%m%d%H%M%S").to_string()),
                        unique: text(item, "ETag").and_then(etag).map(str::to_owned),
                    }
                };
                if let Some(previous) = entries.insert(name.to_owned(), entry.clone())
                    && previous != entry
                {
                    return Err(invalid_input(
                        "S3-Objekt und Präfix haben denselben sichtbaren Namen.",
                    ));
                }
                if entries.len() > 200_000 {
                    return Err(invalid_input("Das S3-Präfix enthält zu viele Einträge."));
                }
            }
            if text(root, "IsTruncated") != Some("true") {
                return Ok(Listing {
                    entries: entries.into_values().collect(),
                    format: ListFormat::S3,
                });
            }
            let next = text(root, "NextContinuationToken")
                .ok_or_else(|| invalid_input("S3-Folgeseite fehlt."))?;
            if next.is_empty() || Some(next) == continuation.as_deref() {
                return Err(invalid_input("Ungültige S3-Folgeseite."));
            }
            continuation = Some(next.to_owned());
        }
        Err(invalid_input("Zu viele Seiten in der S3-Verzeichnisliste."))
    }

    pub(super) fn s3_mkdir(&self, mut url: Url, cancel: &AtomicBool) -> io::Result<()> {
        let (_, key) = coordinates(&url)?;
        if !key.is_empty() {
            url.path_segments_mut()
                .map_err(|_| invalid_input("Ungültiges S3-Präfix."))?
                .push("");
            return self.http_empty_put(&url, cancel);
        }
        let mut request = self.http(&url, "PUT")?;
        request.header("Content-Type", "application/xml")?;
        if self.connection.s3_region == "us-east-1" {
            request.header("Content-Length", "0")?;
        } else {
            request.body(&format!("<CreateBucketConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><LocationConstraint>{}</LocationConstraint></CreateBucketConfiguration>", self.connection.s3_region))?;
        }
        request.send(cancel)?.expect(&[200, 201, 204]).map(|_| ())
    }

    fn http_empty_put(&self, url: &Url, cancel: &AtomicBool) -> io::Result<()> {
        let mut request = self.http(url, "PUT")?;
        request
            .header("If-None-Match", "*")?
            .header("Content-Length", "0")?;
        request.send(cancel)?.expect(&[200, 201, 204]).map(|_| ())
    }

    pub(super) fn s3_remove(
        &self,
        mut url: Url,
        entry: &Entry,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let (_, key) = coordinates(&url)?;
        if entry.directory == Some(true) {
            url.path_segments_mut()
                .map_err(|_| invalid_input("Ungültiges S3-Präfix."))?
                .push("");
            if !self.list_s3(&url, cancel)?.entries.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::DirectoryNotEmpty,
                    "Das S3-Präfix ist noch belegt.",
                ));
            }
            if key.is_empty() {
                url.path_segments_mut()
                    .map_err(|_| invalid_input("Ungültiger S3-Bucket."))?
                    .pop_if_empty();
            }
        }
        let mut request = self.http(&url, "DELETE")?;
        if entry.directory == Some(false) {
            request.header("If-Match", expected_tag(entry)?)?;
        }
        request.send(cancel)?.expect(&[200, 204]).map(|_| ())
    }

    pub(super) fn s3_rename(
        &self,
        parent: &Url,
        entry: &Entry,
        target: &str,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        if entry.directory != Some(false) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "S3-Präfixe lassen sich über Kopieren und Löschen verschieben.",
            ));
        }
        let temporary = NamedTempFile::new()?;
        let tags = self.s3_tags(parent, entry, cancel)?;
        self.fetch_sync(parent, entry, temporary.as_file(), cancel)?;
        let destination = self.file(parent, target)?;
        self.s3_upload(
            &destination,
            temporary.path(),
            None,
            || self.verify_sync_entry(parent, entry, cancel),
            cancel,
        )?;
        if !tags.tags.is_empty() {
            let copied = self
                .list_s3(parent, cancel)?
                .entries
                .into_iter()
                .find(|entry| entry.name == target && entry.directory == Some(false))
                .ok_or_else(|| io::Error::other("Das S3-Ziel wurde nicht bestätigt."))?;
            let empty = self.s3_tags(parent, &copied, cancel)?;
            self.replace_s3_tags(parent, &copied, &empty, &tags.tags, cancel).map_err(|error| {
                io::Error::new(error.kind(), format!("S3-Ziel wurde erstellt. Die Quelle bleibt erhalten, da die Tags nicht kopiert wurden. {error}"))
            })?;
        }
        if self.s3_tags(parent, entry, cancel)? != tags {
            return Err(io::Error::other(
                "S3-Ziel wurde erstellt. Die Quelle bleibt erhalten, da sich ihre Tags geändert haben.",
            ));
        }
        self.s3_remove(self.file(parent, &entry.name)?, entry, cancel).map_err(|error| {
            io::Error::new(error.kind(), format!("Das S3-Ziel wurde erstellt. Die Quelle konnte nicht sicher entfernt werden. {error}"))
        })
    }

    pub(super) fn s3_upload(
        &self,
        url: &Url,
        source: &Path,
        expected: Option<&Entry>,
        verify: impl FnOnce() -> io::Result<()>,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let (bucket, key) = coordinates(url)?;
        if bucket.is_empty() || key.is_empty() || key.ends_with('/') {
            return Err(invalid_input("Wähle einen S3-Bucket mit einem Objektziel."));
        }
        let mut file = File::open(source)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() > MAX_OBJECT {
            return Err(invalid_input(
                "Die Quelle ist keine reguläre S3-Datei in zulässiger Größe.",
            ));
        }
        let size = metadata.len();
        let modified = metadata.modified()?;
        let condition = match expected {
            Some(entry) => ("If-Match", expected_tag(entry)?),
            None => ("If-None-Match", "*"),
        };
        if size <= PART {
            let part = stage_part(&mut file, size, cancel)?;
            verify_local(&file, size, modified)?;
            verify()?;
            let mut request = self.http(url, "PUT")?;
            request
                .header(condition.0, condition.1)?
                .upload(part.path());
            return request
                .execute(None, cancel, || Ok(()))?
                .expect(&[200, 201, 204])
                .map(|_| ());
        }
        let multipart = multipart::Multipart::start(self, url, cancel)?;
        let layout = multipart::Layout::new(size);
        let mut tags = Vec::new();
        let result = (|| {
            for number in 1..=layout.count() {
                let part = stage_part(&mut file, layout.size_of(number), cancel)?;
                tags.push(multipart.put(number, part.path(), cancel)?);
            }
            verify_local(&file, size, modified)?;
            verify()?;
            multipart
                .complete(tags.iter().map(String::as_str), condition, cancel)
                .map(|_| ())
        })();
        if result.is_err() && multipart.abort(&AtomicBool::new(false)).is_err() {
            return Err(io::Error::other(
                "Der S3-Multipart-Auftrag wurde unterbrochen. Prüfe offene Uploads im Ziel-Bucket.",
            ));
        }
        result
    }
}

fn directory_entry(name: &str) -> Entry {
    Entry {
        name: name.to_owned(),
        directory: Some(true),
        symlink: false,
        size: None,
        modified: None,
        unique: None,
    }
}

fn stage_part(
    source: &mut File,
    mut remaining: u64,
    cancel: &AtomicBool,
) -> io::Result<NamedTempFile> {
    let mut part = NamedTempFile::new()?;
    let mut buffer = [0; 64 * 1024];
    while remaining > 0 {
        check_cancel(cancel)?;
        let count = remaining.min(buffer.len() as u64) as usize;
        source.read_exact(&mut buffer[..count])?;
        part.write_all(&buffer[..count])?;
        remaining -= count as u64;
    }
    Ok(part)
}

fn verify_local(source: &File, size: u64, modified: std::time::SystemTime) -> io::Result<()> {
    let current = source.metadata()?;
    if current.len() != size || current.modified()? != modified {
        return Err(invalid_input("Die lokale S3-Quelldatei hat sich geändert."));
    }
    Ok(())
}

fn encode_s3_path(path: &str) -> String {
    use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, percent_encode};
    use std::fmt::Write as _;

    const SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'_')
        .remove(b'.')
        .remove(b'~');
    let mut encoded = String::with_capacity(path.len());
    for (index, segment) in path.split('/').enumerate() {
        if index > 0 {
            encoded.push('/');
        }
        let bytes: Vec<_> = percent_decode_str(segment).collect();
        write!(encoded, "{}", percent_encode(&bytes, SEGMENT)).expect("format S3 path");
    }
    encoded
}

fn coordinates(url: &Url) -> io::Result<(String, String)> {
    let segments = url.path().trim_start_matches('/');
    let (bucket, key) = segments.split_once('/').unwrap_or((segments, ""));
    if !bucket.is_empty()
        && (!bucket.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'.')
        }) || bucket.len() > 63)
    {
        return Err(invalid_input("Ungültiger S3-Bucket-Name."));
    }
    let key = percent_encoding::percent_decode_str(key)
        .decode_utf8()
        .map_err(io::Error::other)?
        .into_owned();
    Ok((bucket.to_owned(), key))
}

fn expected_tag(entry: &Entry) -> io::Result<&str> {
    entry.unique.as_deref().and_then(etag).ok_or_else(|| {
        invalid_input("Der Server liefert keinen eindeutigen ETag für das S3-Objekt.")
    })
}

fn child<'a, 'i>(node: roxmltree::Node<'a, 'i>, name: &str) -> Option<roxmltree::Node<'a, 'i>> {
    node.children().find(|item| item.has_tag_name(name))
}

fn text<'a, 'i>(node: roxmltree::Node<'a, 'i>, name: &str) -> Option<&'a str> {
    child(node, name)?.text()
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::ftp::SyncUpload;
    use std::fs;

    #[test]
    #[ignore = "requires scripts/test-native-http-reference.py"]
    fn reference_native_http_s3() {
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
        let bucket_name = "arco-fixture-native";
        session
            .create_sync_directory(&root, bucket_name, &cancel)
            .unwrap();
        let bucket = session
            .relative_directory(&root, Path::new(bucket_name))
            .unwrap();
        session
            .create_sync_directory(&bucket, "Büro #1", &cancel)
            .unwrap();
        let folder = session
            .relative_directory(&bucket, Path::new("Büro #1"))
            .unwrap();
        crate::ftp::rate::tests::check_live_download(&session, &folder);
        let local = tempfile::tempdir().unwrap();
        let input = local.path().join("input");
        let output = local.path().join("output");
        let body: Vec<u8> = (0..PART as usize + 57)
            .map(|index| (index % 251) as u8)
            .collect();
        fs::write(&input, &body).unwrap();
        let name = "Unterlagen & Bilder.bin";
        let aborted = SyncUpload {
            source: &input,
            name,
            expected: None,
            size: body.len() as u64,
            modified_utc: None,
        };
        let error = session
            .upload_sync(
                &folder,
                aborted,
                || Err(io::ErrorKind::Interrupted.into()),
                &cancel,
            )
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(
            session
                .list_s3(&folder, &cancel)
                .unwrap()
                .entries
                .is_empty()
        );
        let mut check = session.http(&bucket, "GET").unwrap();
        check.query("uploads", "");
        let response = check.send(&cancel).unwrap().expect(&[200]).unwrap();
        assert!(child(http::xml(&response.body).unwrap().root_element(), "Upload").is_none());
        session
            .upload(&folder, &input, name, false, &cancel)
            .unwrap();
        session.download(&folder, name, &output, &cancel).unwrap();
        assert_eq!(fs::read(&output).unwrap(), body);
        let old = session
            .list_s3(&folder, &cancel)
            .unwrap()
            .entries
            .pop()
            .unwrap();
        assert_eq!(old.size, Some(body.len() as u64));
        fs::write(&input, b"small").unwrap();
        session
            .upload(&folder, &input, name, true, &cancel)
            .unwrap();
        assert!(session.delete_sync_entry(&folder, &old, &cancel).is_err());
        for another in ["A.txt", "B.txt", "C.txt"] {
            session
                .upload(&folder, &input, another, false, &cancel)
                .unwrap();
        }
        let entries = session.list_s3(&folder, &cancel).unwrap().entries;
        assert_eq!(entries.len(), 4);
        let current = entries.iter().find(|entry| entry.name == name).unwrap();
        let snapshot = session.s3_tags(&folder, current, &cancel).unwrap();
        assert!(snapshot.tags.is_empty());
        let tags = BTreeMap::from([
            ("Team".to_owned(), "Arco".to_owned()),
            ("Status".to_owned(), "Test".to_owned()),
        ]);
        session
            .replace_s3_tags(&folder, current, &snapshot, &tags, &cancel)
            .unwrap();
        let changed = session.s3_tags(&folder, current, &cancel).unwrap();
        assert_eq!(changed.tags, tags);
        assert!(
            session
                .replace_s3_tags(&folder, current, &snapshot, &BTreeMap::new(), &cancel)
                .is_err()
        );
        session
            .rename_sync_entry(&folder, current, "Neu.txt", &cancel)
            .unwrap();
        let entries = session.list_s3(&folder, &cancel).unwrap().entries;
        assert_eq!(entries.len(), 4);
        let renamed = entries
            .iter()
            .find(|entry| entry.name == "Neu.txt")
            .unwrap();
        let copied_tags = session.s3_tags(&folder, renamed, &cancel).unwrap();
        assert_eq!(copied_tags.tags, tags);
        session
            .replace_s3_tags(&folder, renamed, &copied_tags, &BTreeMap::new(), &cancel)
            .unwrap();
        for entry in entries {
            session.delete_sync_entry(&folder, &entry, &cancel).unwrap();
        }
        let marker = session
            .list_s3(&bucket, &cancel)
            .unwrap()
            .entries
            .pop()
            .unwrap();
        session
            .delete_sync_entry(&bucket, &marker, &cancel)
            .unwrap();
        let prefix = session
            .relative_directory(&bucket, Path::new("without marker"))
            .unwrap();
        session
            .upload(&prefix, &input, "last.txt", false, &cancel)
            .unwrap();
        let implicit = session
            .list_s3(&bucket, &cancel)
            .unwrap()
            .entries
            .pop()
            .unwrap();
        let last = session
            .list_s3(&prefix, &cancel)
            .unwrap()
            .entries
            .pop()
            .unwrap();
        session.delete_sync_entry(&prefix, &last, &cancel).unwrap();
        session
            .delete_sync_entry(&bucket, &implicit, &cancel)
            .unwrap();
        assert!(
            session
                .list_s3(&bucket, &cancel)
                .unwrap()
                .entries
                .is_empty()
        );
        let removable = session
            .list_s3(&root, &cancel)
            .unwrap()
            .entries
            .pop()
            .unwrap();
        session
            .delete_sync_entry(&root, &removable, &cancel)
            .unwrap();
        assert!(session.list_s3(&root, &cancel).unwrap().entries.is_empty());
        if let Ok(profile) = std::env::var("ARCO_TEST_AWS_PROFILE") {
            let sdk = Connection::new(&std::env::var("ARCO_TEST_S3").unwrap(), "", Security::S3)
                .unwrap()
                .with_s3_region("eu-central-1")
                .unwrap()
                .with_s3_aws_profile(Some(&profile))
                .unwrap()
                .authenticate("")
                .unwrap();
            assert!(sdk.list_s3(&root, &cancel).unwrap().entries.is_empty());
        }
    }
}
