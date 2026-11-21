use super::{S3Version, S3Versioning, restored_version_id};
use crate::ftp::s3::{
    etag, http,
    multipart::{Layout, MAX_PARTS, Multipart},
};
use crate::ftp::{DownloadPhase, DownloadProgress, Session, Url, check_cancel, invalid_input};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use std::collections::BTreeMap;
use std::io;
use std::sync::atomic::AtomicBool;

const GIB: u64 = 1024 * 1024 * 1024;
pub(super) const MAX_COPY_PART: u64 = 5 * GIB;

pub(super) struct VersionCopy<'a> {
    session: &'a Session,
    bucket: &'a Url,
    object: &'a Url,
    version: &'a S3Version,
    size: u64,
    source: &'a str,
}

#[derive(PartialEq, Eq)]
struct CopySource {
    tag: String,
    headers: BTreeMap<String, String>,
}

#[derive(PartialEq, Eq)]
struct Destination {
    version: Option<String>,
    tag: Option<String>,
}

impl<'a> VersionCopy<'a> {
    pub(super) fn new(
        session: &'a Session,
        bucket: &'a Url,
        object: &'a Url,
        version: &'a S3Version,
        size: u64,
        source: &'a str,
    ) -> Self {
        Self {
            session,
            bucket,
            object,
            version,
            size,
            source,
        }
    }

    pub(super) fn run(
        &self,
        cancel: &AtomicBool,
        progress: &mut impl FnMut(DownloadProgress),
    ) -> io::Result<String> {
        if self.size > MAX_COPY_PART * u64::from(MAX_PARTS) {
            return Err(invalid_input(
                "Die S3-Version überschreitet die maximale Multipart-Größe.",
            ));
        }
        let layout = Layout::with_minimum(self.size, GIB);
        let source = self.read_source(cancel)?;
        let destination = self.read_destination(cancel)?;
        check_cancel(cancel)?;
        let upload =
            Multipart::start_with_headers(self.session, self.object, &source.headers, cancel)?;
        let result = (|| {
            let mut tags = Vec::with_capacity(layout.count() as usize);
            let mut report = |received, phase| {
                progress(DownloadProgress {
                    received,
                    total: Some(self.size),
                    phase,
                })
            };
            report(0, DownloadPhase::Receiving);
            for number in 1..=layout.count() {
                check_cancel(cancel)?;
                let range = layout.range(number);
                let received = range.end;
                tags.push(upload.copy_part(
                    number,
                    self.source,
                    &self.version.id,
                    &source.tag,
                    range,
                    cancel,
                )?);
                report(received, DownloadPhase::Receiving);
            }
            report(self.size, DownloadPhase::Verifying);
            if self.read_source(cancel)? != source {
                return Err(io::Error::other(
                    "Die S3-Quellversion wurde während der Kopie geändert.",
                ));
            }
            if self.read_destination(cancel)? != destination
                || self.session.read_s3_versioning(self.bucket, cancel)? != S3Versioning::Enabled
            {
                return Err(io::Error::other(
                    "Das S3-Ziel wurde während der Kopie geändert.",
                ));
            }
            let condition = match destination.tag.as_deref() {
                Some(tag) => ("If-Match", tag),
                None => ("If-None-Match", "*"),
            };
            upload.complete(tags.iter().map(String::as_str), condition, cancel)
        })();
        match result {
            Ok(response) => restored_version_id(&response, &self.version.id),
            Err(error) => match upload.abort(&AtomicBool::new(false)) {
                Ok(()) => Err(error),
                Err(abort) => Err(io::Error::other(format!(
                    "{error} Der S3-Multipart-Auftrag konnte nicht verworfen werden. {abort}"
                ))),
            },
        }
    }

    fn read_source(&self, cancel: &AtomicBool) -> io::Result<CopySource> {
        let mut request = self.session.http(self.object, "HEAD")?;
        request.query("versionId", &self.version.id);
        if let Some(tag) = &self.version.tag {
            request.header("If-Match", tag)?;
        }
        let response = request.send(cancel)?.expect(&[200])?;
        let headers = &response.headers;
        let tag = object_tag(&response)?;
        if version_header(&response) != self.version.id
            || self
                .version
                .tag
                .as_ref()
                .is_some_and(|expected| expected != &tag)
            || headers
                .get("content-length")
                .and_then(|length| length.parse().ok())
                != Some(self.size)
        {
            return Err(io::Error::other(
                "Der Server hat die S3-Quellversion nicht bestätigt.",
            ));
        }
        let tags = self
            .session
            .read_s3_tags(self.object, Some(&self.version.id), cancel)?;
        if tags
            .version
            .as_ref()
            .is_some_and(|id| id != &self.version.id)
        {
            return Err(io::Error::other(
                "Der Server hat Tags einer anderen S3-Version geliefert.",
            ));
        }
        let mut headers: BTreeMap<_, _> = headers
            .iter()
            .filter(|(name, _)| {
                name.starts_with("x-amz-meta-")
                    || matches!(
                        name.as_str(),
                        "cache-control"
                            | "content-disposition"
                            | "content-encoding"
                            | "content-language"
                            | "content-type"
                            | "expires"
                            | "x-amz-website-redirect-location"
                    )
            })
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        if !tags.tags.is_empty() {
            let value = tags
                .tags
                .iter()
                .map(|(key, value)| {
                    format!(
                        "{}={}",
                        utf8_percent_encode(key, NON_ALPHANUMERIC),
                        utf8_percent_encode(value, NON_ALPHANUMERIC),
                    )
                })
                .collect::<Vec<_>>()
                .join("&");
            headers.insert("x-amz-tagging".into(), value);
        }
        Ok(CopySource { tag, headers })
    }

    fn read_destination(&self, cancel: &AtomicBool) -> io::Result<Destination> {
        let response = self
            .session
            .http(self.object, "HEAD")?
            .send(cancel)?
            .expect(&[200, 404])?;
        Ok(Destination {
            version: response.headers.get("x-amz-version-id").cloned(),
            tag: (response.status == 200)
                .then(|| object_tag(&response))
                .transpose()?,
        })
    }
}

fn version_header(response: &http::Response) -> &str {
    response
        .headers
        .get("x-amz-version-id")
        .map_or("null", String::as_str)
}

fn object_tag(response: &http::Response) -> io::Result<String> {
    response
        .headers
        .get("etag")
        .and_then(|tag| etag(tag))
        .map(str::to_owned)
        .ok_or_else(|| io::Error::other("Der Server hat keinen gültigen S3-ETag geliefert."))
}
