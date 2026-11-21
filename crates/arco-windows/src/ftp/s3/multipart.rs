use super::{
    MAX_OBJECT, PART, child, coordinates, escape_xml, etag, http, stage_part, text, verify_local,
};
use crate::ftp::{
    DownloadPhase, DownloadProgress, Security, Session, TransferMode, Url, check_cancel,
    invalid_input,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Seek, SeekFrom};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use tempfile::NamedTempFile;

pub(super) const MAX_PARTS: u32 = 10_000;
const RECEIPT_VERSION: u8 = 1;
const RECEIPT_LIMIT: u64 = 8 * 1024 * 1024;

pub(super) struct Layout {
    size: u64,
    part_size: u64,
}

impl Layout {
    pub(super) fn new(size: u64) -> Self {
        Self::with_minimum(size, PART)
    }

    pub(super) fn with_minimum(size: u64, minimum: u64) -> Self {
        Self {
            size,
            part_size: minimum.max(size.div_ceil(u64::from(MAX_PARTS))),
        }
    }

    pub(super) fn count(&self) -> u32 {
        self.size.div_ceil(self.part_size) as u32
    }

    pub(super) fn size_of(&self, number: u32) -> u64 {
        self.part_size.min(self.size - self.offset(number))
    }

    pub(super) fn range(&self, number: u32) -> Range<u64> {
        let start = self.offset(number);
        start..start + self.size_of(number)
    }

    fn offset(&self, number: u32) -> u64 {
        u64::from(number - 1) * self.part_size
    }
}

pub(super) struct Multipart<'a> {
    session: &'a Session,
    url: &'a Url,
    id: String,
}

struct UploadedPart {
    size: u64,
    tag: String,
}

impl<'a> Multipart<'a> {
    pub(super) fn start(
        session: &'a Session,
        url: &'a Url,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        Self::start_with_headers(session, url, &BTreeMap::new(), cancel)
    }

    pub(super) fn start_with_headers(
        session: &'a Session,
        url: &'a Url,
        headers: &BTreeMap<String, String>,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        let mut request = session.http(url, "POST")?;
        request.query("uploads", "").header("Content-Length", "0")?;
        for (key, value) in headers {
            request.header(key, value)?;
        }
        let response = request.send(cancel)?.expect(&[200, 201])?;
        let xml = http::xml(&response.body)?;
        let id = text(xml.root_element(), "UploadId")
            .filter(|id| valid_id(id))
            .ok_or_else(|| invalid_input("Der Server hat keine gültige Multipart-ID bestätigt."))?;
        Ok(Self {
            session,
            url,
            id: id.to_owned(),
        })
    }

    pub(super) fn copy_part(
        &self,
        number: u32,
        source: &str,
        version_id: &str,
        tag: &str,
        range: Range<u64>,
        cancel: &AtomicBool,
    ) -> io::Result<String> {
        let mut request = self.session.http(self.url, "PUT")?;
        request
            .query("partNumber", &number.to_string())
            .query("uploadId", &self.id)
            .header("x-amz-copy-source", source)?
            .header("x-amz-copy-source-if-match", tag)?
            .header(
                "x-amz-copy-source-range",
                &format!("bytes={}-{}", range.start, range.end - 1),
            )?;
        let response = request.send(cancel)?.expect(&[200])?;
        let xml = http::xml(&response.body)?;
        let result = xml.root_element();
        if response
            .headers
            .get("x-amz-copy-source-version-id")
            .is_some_and(|id| id != version_id)
        {
            return Err(io::Error::other("S3 hat eine andere Quellversion kopiert."));
        }
        text(result, "ETag")
            .filter(|tag| result.has_tag_name("CopyPartResult") && valid_tag(tag))
            .map(str::to_owned)
            .ok_or_else(|| {
                io::Error::other(format!(
                    "S3 hat die Kopie von Teil {number} nicht bestätigt. {}",
                    text(result, "Code").unwrap_or_default()
                ))
            })
    }

    pub(super) fn put(&self, number: u32, part: &Path, cancel: &AtomicBool) -> io::Result<String> {
        let mut request = self.session.http(self.url, "PUT")?;
        request
            .query("partNumber", &number.to_string())
            .query("uploadId", &self.id)
            .upload(part);
        let response = request.execute(None, cancel, || Ok(()))?.expect(&[200])?;
        response
            .headers
            .get("etag")
            .filter(|tag| valid_tag(tag))
            .cloned()
            .ok_or_else(|| {
                invalid_input("Der Server bestätigt keinen gültigen ETag für den S3-Teil.")
            })
    }

    pub(super) fn complete<'b>(
        &self,
        tags: impl IntoIterator<Item = &'b str>,
        condition: (&str, &str),
        cancel: &AtomicBool,
    ) -> io::Result<http::Response> {
        use std::fmt::Write as _;
        let mut body = "<CompleteMultipartUpload>".to_owned();
        for (index, tag) in tags.into_iter().enumerate() {
            write!(
                body,
                "<Part><PartNumber>{}</PartNumber><ETag>{}</ETag></Part>",
                index + 1,
                escape_xml(tag),
            )
            .expect("format multipart XML");
        }
        body.push_str("</CompleteMultipartUpload>");
        let mut request = self.session.http(self.url, "POST")?;
        request
            .query("uploadId", &self.id)
            .header(condition.0, condition.1)?
            .header("Content-Type", "application/xml")?
            .body(&body)?;
        let response = request.send(cancel)?.expect(&[200])?;
        let reply = http::xml(&response.body)?;
        if !reply
            .root_element()
            .has_tag_name("CompleteMultipartUploadResult")
        {
            return Err(io::Error::other(
                "Der Server hat den S3-Multipart-Abschluss nicht bestätigt.",
            ));
        }
        Ok(response)
    }

    pub(super) fn abort(&self, cancel: &AtomicBool) -> io::Result<()> {
        let mut request = self.session.http(self.url, "DELETE")?;
        request.query("uploadId", &self.id);
        request.send(cancel)?.expect(&[200, 204, 404]).map(|_| ())
    }

    fn parts(
        &self,
        layout: &Layout,
        cancel: &AtomicBool,
    ) -> io::Result<BTreeMap<u32, UploadedPart>> {
        let (bucket, key) = coordinates(self.url)?;
        let mut parts = BTreeMap::new();
        let mut marker = 0;
        loop {
            let mut request = self.session.http(self.url, "GET")?;
            request.query("uploadId", &self.id);
            if marker > 0 {
                request.query("part-number-marker", &marker.to_string());
            }
            let response = request.send(cancel)?.expect(&[200])?;
            let xml = http::xml(&response.body)?;
            let root = xml.root_element();
            if !matches!(
                root.tag_name().name(),
                "ListPartsResult" | "ListPartsResponse"
            ) || text(root, "UploadId") != Some(self.id.as_str())
                || text(root, "Bucket") != Some(bucket.as_str())
                || text(root, "Key") != Some(key.as_str())
            {
                return Err(invalid_input(
                    "S3 bestätigt die Identität des Multipart-Auftrags nicht.",
                ));
            }
            let invalid = || invalid_input("Ungültige S3-Multipart-Teileliste.");
            let mut last = marker;
            for part in root.children().filter(|node| node.has_tag_name("Part")) {
                let number: u32 = text(part, "PartNumber")
                    .and_then(|number| number.parse().ok())
                    .filter(|number| *number > last && *number <= layout.count())
                    .ok_or_else(invalid)?;
                let size = text(part, "Size")
                    .and_then(|size| size.parse().ok())
                    .ok_or_else(invalid)?;
                let tag = text(part, "ETag")
                    .filter(|tag| valid_tag(tag))
                    .ok_or_else(invalid)?;
                parts.insert(
                    number,
                    UploadedPart {
                        size,
                        tag: tag.to_owned(),
                    },
                );
                last = number;
            }
            match text(root, "IsTruncated") {
                Some("false") => return Ok(parts),
                Some("true") => {
                    let next: u32 = child(root, "NextPartNumberMarker")
                        .and_then(|node| node.text())
                        .and_then(|value| value.parse().ok())
                        .filter(|next| *next == last && *next > marker && *next < layout.count())
                        .ok_or_else(invalid)?;
                    marker = next;
                }
                _ => return Err(invalid()),
            }
        }
    }
}

#[derive(Deserialize, Serialize)]
struct Receipt {
    version: u8,
    id: String,
    source_hash: String,
    size: u64,
    parts: Vec<Option<String>>,
}

struct Draft {
    record: PathBuf,
    _lock: File,
}

impl Draft {
    fn open(session: &Session, url: &Url) -> io::Result<Self> {
        let root = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or_else(|| io::Error::other("Der lokale Ordner für S3-Upload-Aufträge fehlt."))?
            .join("Arco Commander")
            .join("s3-multipart");
        let mut identity = serde_json::to_vec(&(
            url.as_str(),
            &session.connection.s3_region,
            &session.connection.user,
            &session.connection.s3_aws_profile,
        ))
        .map_err(io::Error::other)?;
        if let Some(role) = &session.connection.s3_role_arn {
            identity.push(0);
            identity.extend_from_slice(role.as_bytes());
        }
        let key = blake3::hash(&identity).to_hex();
        fs::create_dir_all(&root)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join(format!("{key}.lock")))?;
        lock.try_lock().map_err(|error| match error {
            TryLockError::WouldBlock => io::Error::new(
                io::ErrorKind::WouldBlock,
                "Arco bearbeitet diesen S3-Upload bereits.",
            ),
            TryLockError::Error(error) => error,
        })?;
        Ok(Self {
            record: root.join(format!("{key}.json")),
            _lock: lock,
        })
    }

    fn load(&self) -> io::Result<Option<Receipt>> {
        let file = match File::open(&self.record) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let mut data = Vec::new();
        file.take(RECEIPT_LIMIT + 1).read_to_end(&mut data)?;
        if data.len() as u64 > RECEIPT_LIMIT {
            return Err(invalid_input("Die S3-Upload-Quittung ist zu groß."));
        }
        let receipt: Receipt = serde_json::from_slice(&data).map_err(io::Error::other)?;
        if receipt.version != RECEIPT_VERSION
            || !valid_id(&receipt.id)
            || receipt.source_hash.parse::<blake3::Hash>().is_err()
            || !(PART + 1..=MAX_OBJECT).contains(&receipt.size)
            || receipt.parts.len() != Layout::new(receipt.size).count() as usize
            || receipt.parts.iter().flatten().any(|tag| !valid_tag(tag))
        {
            return Err(invalid_input("Ungültige lokale S3-Upload-Quittung."));
        }
        Ok(Some(receipt))
    }

    fn store(&self, receipt: &Receipt) -> io::Result<()> {
        let mut staging = NamedTempFile::new_in(self.record.parent().expect("draft directory"))?;
        serde_json::to_writer(&mut staging, receipt).map_err(io::Error::other)?;
        staging.as_file().sync_all()?;
        staging.persist(&self.record).map_err(|error| error.error)?;
        Ok(())
    }

    fn remove(&self) -> io::Result<()> {
        match fs::remove_file(&self.record) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

impl Session {
    pub fn upload_s3_resuming(
        &self,
        directory: &Url,
        source: &Path,
        name: &str,
        cancel: &AtomicBool,
        mut progress: impl FnMut(DownloadProgress),
    ) -> io::Result<()> {
        let url = self.s3_draft_target(directory, name)?;
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
            options.share_mode(FILE_SHARE_READ);
        }
        let mut file = options.open(source)?;
        let original = file.metadata()?;
        let size = original.len();
        if !original.is_file() || size > MAX_OBJECT {
            return Err(invalid_input(
                "Die Quelle ist keine reguläre S3-Datei in zulässiger Größe.",
            ));
        }
        let modified = original.modified()?;
        let draft = Draft::open(self, &url)?;
        let prior = draft.load()?;
        if prior.as_ref().is_some_and(|receipt| receipt.size != size) {
            return Err(source_changed());
        }
        if size <= PART {
            self.upload_with_mode(directory, source, name, false, TransferMode::Binary, cancel)?;
            progress(DownloadProgress {
                received: size,
                total: Some(size),
                phase: DownloadPhase::Verifying,
            });
            return Ok(());
        }
        self.s3_require_new_object(&url, cancel)?;
        progress(DownloadProgress {
            received: 0,
            total: Some(size),
            phase: DownloadPhase::Verifying,
        });
        let digest = source_hash(&mut file, cancel)?;
        verify_local(&file, size, modified)?;
        let layout = Layout::new(size);
        let (multipart, mut receipt, mut existing) =
            self.open_s3_draft(&url, &draft, prior, digest, &layout, cancel)?;
        existing.retain(|number, part| {
            part.size == layout.size_of(*number)
                && receipt.parts[(*number - 1) as usize].as_ref() == Some(&part.tag)
        });
        let mut received = existing.values().map(|part| part.size).sum();
        progress(DownloadProgress {
            received,
            total: Some(size),
            phase: DownloadPhase::Receiving,
        });
        for number in 1..=layout.count() {
            check_cancel(cancel)?;
            if existing.contains_key(&number) {
                continue;
            }
            let offset = layout.offset(number);
            let count = layout.size_of(number);
            let index = (number - 1) as usize;
            file.seek(SeekFrom::Start(offset))?;
            let part = stage_part(&mut file, count, cancel)?;
            receipt.parts[index] = Some(multipart.put(number, part.path(), cancel)?);
            draft.store(&receipt)?;
            received += count;
            progress(DownloadProgress {
                received,
                total: Some(size),
                phase: DownloadPhase::Receiving,
            });
        }
        progress(DownloadProgress {
            received: size,
            total: Some(size),
            phase: DownloadPhase::Verifying,
        });
        verify_local(&file, size, modified)?;
        self.s3_require_new_object(&url, cancel)?;
        let tags = receipt
            .parts
            .iter()
            .map(Option::as_deref)
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| invalid_input("Ein bestätigter S3-Multipart-Teil fehlt."))?;
        multipart.complete(tags, ("If-None-Match", "*"), cancel)?;
        draft.remove().map_err(|error| io::Error::other(format!(
            "Das S3-Objekt wurde veröffentlicht. Die lokale Upload-Quittung konnte nicht entfernt werden. {error}"
        )))
    }

    fn open_s3_draft<'a>(
        &'a self,
        url: &'a Url,
        draft: &Draft,
        prior: Option<Receipt>,
        digest: String,
        layout: &Layout,
        cancel: &AtomicBool,
    ) -> io::Result<(Multipart<'a>, Receipt, BTreeMap<u32, UploadedPart>)> {
        if let Some(receipt) = prior {
            if receipt.source_hash != digest {
                return Err(source_changed());
            }
            let multipart = Multipart {
                session: self,
                url,
                id: receipt.id.clone(),
            };
            match multipart.parts(layout, cancel) {
                Ok(parts) => return Ok((multipart, receipt, parts)),
                Err(error) if error.kind() == io::ErrorKind::NotFound => draft.remove()?,
                Err(error) => return Err(error),
            }
        }
        let multipart = Multipart::start(self, url, cancel)?;
        let receipt = Receipt {
            version: RECEIPT_VERSION,
            id: multipart.id.clone(),
            size: layout.size,
            source_hash: digest,
            parts: vec![None; layout.count() as usize],
        };
        if let Err(error) = draft.store(&receipt) {
            multipart.abort(&AtomicBool::new(false)).map_err(|abort_error| io::Error::other(format!(
                "S3-Upload-Quittung konnte nicht gespeichert werden. Prüfe offene Uploads im Bucket. {error} {abort_error}"
            )))?;
            return Err(error);
        }
        Ok((multipart, receipt, BTreeMap::new()))
    }

    pub fn discard_s3_upload(
        &self,
        directory: &Url,
        name: &str,
        cancel: &AtomicBool,
    ) -> io::Result<bool> {
        let url = self.s3_draft_target(directory, name)?;
        let draft = Draft::open(self, &url)?;
        let Some(receipt) = draft.load()? else {
            return Ok(false);
        };
        let multipart = Multipart {
            session: self,
            url: &url,
            id: receipt.id,
        };
        multipart.abort(cancel)?;
        draft.remove()?;
        Ok(true)
    }

    fn s3_draft_target(&self, directory: &Url, name: &str) -> io::Result<Url> {
        if self.security() != Security::S3 {
            return Err(invalid_input(
                "Ein fortsetzbarer Multipart-Upload benötigt S3.",
            ));
        }
        self.check_write_name(name)?;
        let url = self.file(directory, name)?;
        let (bucket, key) = coordinates(&url)?;
        if bucket.is_empty() || key.is_empty() || key.ends_with('/') {
            return Err(invalid_input("Wähle einen S3-Bucket mit einem Objektziel."));
        }
        Ok(url)
    }

    fn s3_require_new_object(&self, url: &Url, cancel: &AtomicBool) -> io::Result<()> {
        let response = self.http(url, "HEAD")?.send(cancel)?;
        match response.status {
            404 => Ok(()),
            200 => Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Das S3-Ziel ist bereits vorhanden.",
            )),
            _ => response.expect(&[404]).map(|_| ()),
        }
    }
}

fn source_hash(file: &mut File, cancel: &AtomicBool) -> io::Result<String> {
    let mut hash = blake3::Hasher::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        check_cancel(cancel)?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            return Ok(hash.finalize().to_hex().to_string());
        }
        hash.update(&buffer[..count]);
    }
}

fn source_changed() -> io::Error {
    invalid_input(
        "Die Quelldatei weicht vom begonnenen S3-Upload ab. Verwende die ursprüngliche Datei oder verwirf den begonnenen Auftrag.",
    )
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 16 * 1024 && !id.chars().any(char::is_control)
}

fn valid_tag(tag: &str) -> bool {
    tag.len() <= 256 && etag(tag).is_some()
}
