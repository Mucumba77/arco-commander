use super::{
    Entry, ListFormat, Listing, Session, Url, check_cancel, check_segment, http, invalid_input,
};
use std::io;
use std::path::Path;
use std::sync::atomic::AtomicBool;

const DAV: &str = "DAV:";
const PROPERTIES: &str = r#"<d:propfind xmlns:d="DAV:"><d:prop><d:resourcetype/><d:getcontentlength/><d:getlastmodified/><d:getetag/></d:prop></d:propfind>"#;

impl Session {
    pub(super) fn list_webdav(&self, directory: &Url, cancel: &AtomicBool) -> io::Result<Listing> {
        let mut request = self.http(directory, "PROPFIND")?;
        request
            .header("Depth", "1")?
            .header("Content-Type", "application/xml")?
            .body(PROPERTIES)?;
        let response = request.send(cancel)?.expect(&[207])?;
        let xml = http::xml(&response.body)?;
        let root = xml.root_element();
        if !root.has_tag_name((DAV, "multistatus")) {
            return Err(invalid_input("Ungültige WebDAV-Verzeichnisauskunft."));
        }
        let path = segments(directory)?;
        let mut entries = Vec::new();
        let mut names = std::collections::BTreeSet::new();
        for resource in root
            .children()
            .filter(|node| node.has_tag_name((DAV, "response")))
        {
            check_cancel(cancel)?;
            let address = text(resource, "href")
                .ok_or_else(|| invalid_input("WebDAV-Ressource ohne Adresse."))?;
            let url = directory.join(address).map_err(io::Error::other)?;
            self.validate(&url)?;
            let parts = segments(&url)?;
            if parts == path {
                continue;
            }
            let [name] = parts.strip_prefix(path.as_slice()).unwrap_or_default() else {
                return Err(invalid_input(
                    "Die WebDAV-Liste enthält einen Eintrag außerhalb des Ordners.",
                ));
            };
            let mut entry = Entry {
                name: name.to_owned(),
                directory: None,
                size: None,
                modified: None,
                unique: None,
            };
            for group in resource
                .children()
                .filter(|node| node.has_tag_name((DAV, "propstat")))
            {
                let status =
                    text(group, "status").and_then(|value| value.split_whitespace().nth(1));
                if status != Some("200") {
                    continue;
                }
                let Some(props) = child(group, "prop") else {
                    continue;
                };
                if let Some(kind) = child(props, "resourcetype") {
                    entry.directory = Some(child(kind, "collection").is_some());
                }
                if let Some(value) = text(props, "getcontentlength") {
                    entry.size = value.trim().parse().ok();
                }
                if let Some(value) = text(props, "getlastmodified") {
                    entry.modified = chrono::DateTime::parse_from_rfc2822(value.trim())
                        .ok()
                        .map(|time| time.to_utc().format("%Y%m%d%H%M%S").to_string());
                }
                if let Some(value) = text(props, "getetag") {
                    let value = value.trim();
                    entry.unique = etag(value).map(str::to_owned).or_else(|| {
                        let quoted = format!("\"{value}\"");
                        etag(&quoted).map(str::to_owned)
                    });
                }
            }
            if entry.directory.is_none() {
                return Err(invalid_input("WebDAV-Ressourcentyp fehlt."));
            }
            if entry.directory == Some(true) {
                entry.size = None;
            }
            if !names.insert(name.to_owned()) {
                return Err(invalid_input(
                    "Die WebDAV-Liste enthält einen doppelten Namen.",
                ));
            }
            entries.push(entry);
        }
        Ok(Listing {
            entries,
            format: ListFormat::WebDav,
        })
    }

    pub(super) fn webdav_mkdir(&self, url: &Url, cancel: &AtomicBool) -> io::Result<()> {
        let mut request = self.http(url, "MKCOL")?;
        self.webdav_conditions(&mut request, &[(url, None)], cancel)?;
        request.send(cancel)?.expect(&[201]).map(|_| ())
    }

    pub(super) fn webdav_remove(
        &self,
        url: &Url,
        directory: bool,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let mut request = self.http(url, "DELETE")?;
        if directory {
            if !self.list_webdav(url, cancel)?.entries.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::DirectoryNotEmpty,
                    "Der WebDAV-Ordner ist noch belegt.",
                ));
            }
        }
        request.header("Depth", "infinity")?;
        self.webdav_conditions(&mut request, &[(url, None)], cancel)?;
        request.send(cancel)?.expect(&[200, 204])?;
        self.webdav_forget_removed_locks(url);
        Ok(())
    }

    pub(super) fn webdav_delete_entry(
        &self,
        url: &Url,
        before: &Entry,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let directory = before.directory == Some(true);
        if directory && !self.list_webdav(url, cancel)?.entries.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::DirectoryNotEmpty,
                "Der WebDAV-Ordner ist noch belegt.",
            ));
        }
        let owned = self.webdav_has_write_lock(url, cancel)?;
        let tag = (!directory)
            .then(|| before.unique.as_deref().and_then(etag))
            .flatten();
        if owned || tag.is_some() {
            let mut request = self.http(url, "DELETE")?;
            if let Some(tag) = tag {
                request.header("If-Match", tag)?;
            }
            self.webdav_conditions(&mut request, &[(url, None)], cancel)?;
            request.send(cancel)?.expect(&[200, 204])?;
            self.webdav_forget_removed_locks(url);
            return Ok(());
        }
        let token = self.webdav_acquire_lock(url, directory, 120, cancel)?.token;
        let result = (|| {
            let mut folder = url.clone();
            folder
                .path_segments_mut()
                .map_err(|_| invalid_input("Ungültiger WebDAV-Pfad."))?
                .pop_if_empty()
                .pop()
                .push("");
            if directory {
                let unchanged =
                    self.list_webdav(&folder, cancel)?
                        .entries
                        .into_iter()
                        .any(|entry| {
                            entry.name == before.name
                                && entry.directory == Some(true)
                                && entry.unique == before.unique
                        });
                if !unchanged || !self.list_webdav(url, cancel)?.entries.is_empty() {
                    return Err(io::Error::new(
                        io::ErrorKind::DirectoryNotEmpty,
                        "Der WebDAV-Ordner wurde geändert oder ist noch belegt.",
                    ));
                }
            } else {
                self.verify_sync_entry(&folder, before, cancel)?;
            }
            let mut request = self.http(url, "DELETE")?;
            request.header(
                "If",
                &zeroize::Zeroizing::new(format!("({})", token.as_str())),
            )?;
            request.send(cancel)?.expect(&[200, 204]).map(|_| ())
        })();
        if result.is_err() {
            let mut unlock = self.http(url, "UNLOCK")?;
            unlock.header("Lock-Token", &token)?;
            let _ = unlock.send(&AtomicBool::new(false));
        }
        result
    }

    pub(super) fn webdav_move(
        &self,
        from: &Url,
        to: &Url,
        source: Option<&Entry>,
        target: Option<&Entry>,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        self.validate(to)?;
        let mut request = self.http(from, "MOVE")?;
        request.header("Destination", to.as_str())?;
        if let Some(tag) = source
            .and_then(|entry| entry.unique.as_deref())
            .and_then(etag)
        {
            request.header("If-Match", tag)?;
        }
        let target_tag = if let Some(previous) = target {
            request.header("Overwrite", "T")?;
            Some(previous.unique.as_deref().and_then(etag).ok_or_else(|| {
                invalid_input("Der WebDAV-Server liefert keinen eindeutigen ETag zum Ersetzen.")
            })?)
        } else {
            request.header("Overwrite", "F")?;
            None
        };
        self.webdav_conditions(&mut request, &[(from, None), (to, target_tag)], cancel)?;
        request.send(cancel)?.expect(&[201, 204])?;
        self.webdav_forget_removed_locks(from);
        Ok(())
    }

    pub(super) fn webdav_upload(
        &self,
        parent: &Url,
        source: &Path,
        name: &str,
        expected: Option<&Entry>,
        verify: impl FnOnce() -> io::Result<()>,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let destination = self.file(parent, name)?;
        let reservation = tempfile::Builder::new().prefix(".arco-dav-").tempfile()?;
        let token = reservation
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| invalid_input("Temporärer WebDAV-Dateiname fehlt."))?;
        let temporary = self.file(parent, token)?;
        let mut request = self.http(&temporary, "PUT")?;
        request.header("If-None-Match", "*")?.upload(source);
        self.webdav_conditions(&mut request, &[(&temporary, None)], cancel)?;
        request
            .execute(None, cancel, || Ok(()))?
            .expect(&[200, 201, 204])?;
        let result = verify()
            .and_then(|()| self.webdav_move(&temporary, &destination, None, expected, cancel));
        if result.is_err() {
            let cleanup = AtomicBool::new(false);
            if let Err(error) = self.webdav_remove(&temporary, false, &cleanup)
                && error.kind() != io::ErrorKind::NotFound
            {
                return Err(io::Error::other(format!(
                    "WebDAV-Veröffentlichung fehlgeschlagen. Prüfe die temporäre Datei {token}."
                )));
            }
        }
        result
    }
}

fn segments(url: &Url) -> io::Result<Vec<String>> {
    url.path_segments()
        .into_iter()
        .flatten()
        .filter(|part| !part.is_empty())
        .map(|part| {
            let name = percent_encoding::percent_decode_str(part)
                .decode_utf8()
                .map_err(io::Error::other)?;
            check_segment(&name)?;
            Ok(name.into_owned())
        })
        .collect()
}

fn child<'a, 'input>(
    node: roxmltree::Node<'a, 'input>,
    name: &str,
) -> Option<roxmltree::Node<'a, 'input>> {
    node.children().find(|item| item.has_tag_name((DAV, name)))
}

fn text<'a, 'input>(node: roxmltree::Node<'a, 'input>, name: &str) -> Option<&'a str> {
    child(node, name)?.text()
}

pub(super) fn etag(value: &str) -> Option<&str> {
    let inner = value.strip_prefix('"')?.strip_suffix('"')?;
    (!inner.is_empty()
        && inner
            .bytes()
            .all(|byte| byte >= 33 && byte <= 126 && !matches!(byte, b'"' | b'[' | b']')))
    .then_some(value)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::ftp::{Connection, Security};
    use std::fs;

    #[test]
    #[ignore = "requires scripts/test-native-http-reference.py"]
    fn reference_native_http_webdav() {
        let address = std::env::var("ARCO_TEST_WEBDAV").unwrap();
        let cancel = AtomicBool::new(false);
        let connect = |secret| {
            Connection::new(&address, "arco", Security::WebDav)
                .unwrap()
                .authenticate(secret)
                .unwrap()
        };
        let session = connect("fixture-only");
        let root = session.initial_directory();
        if let Ok(tls) = std::env::var("ARCO_TEST_WEBDAV_TLS") {
            let connection = || Connection::new(&tls, "arco", Security::WebDav).unwrap();
            let untrusted = connection().authenticate("fixture-only").unwrap();
            assert_eq!(
                untrusted
                    .list(&untrusted.initial_directory(), &cancel)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::PermissionDenied
            );
            let trusted = connection()
                .with_options(crate::ftp::ConnectionOptions {
                    ca_bundle: std::env::var_os("ARCO_TEST_WEBDAV_CA").map(Into::into),
                    ..Default::default()
                })
                .unwrap()
                .authenticate("fixture-only")
                .unwrap();
            assert_eq!(
                trusted
                    .list(&trusted.initial_directory(), &cancel)
                    .unwrap()
                    .format,
                ListFormat::WebDav
            );
        }
        assert_eq!(session.parent_directory(&root).unwrap(), root);
        assert!(
            session
                .restore_directory(Some(root.join("../").unwrap().as_str()))
                .is_err()
        );
        assert_eq!(
            connect("wrong-secret")
                .list(&root, &cancel)
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        session
            .create_sync_directory(&root, "Büro & Team", &cancel)
            .unwrap();
        let parent = session
            .relative_directory(&root, Path::new("Büro & Team"))
            .unwrap();
        let local = tempfile::tempdir().unwrap();
        let source = local.path().join("source");
        let output = local.path().join("download");
        let name = "Grüße #1.txt";
        session.webdav_lock(&parent, true, 600, &cancel).unwrap();
        assert!(
            session
                .webdav_locks(&parent, &cancel)
                .unwrap()
                .iter()
                .any(|lock| lock.owned)
        );
        let other = connect("fixture-only");
        assert!(
            other
                .webdav_locks(&parent, &cancel)
                .unwrap()
                .iter()
                .all(|lock| !lock.owned)
        );
        assert!(
            other
                .create_sync_directory(&parent, "blocked", &cancel)
                .is_err()
        );
        session
            .create_sync_directory(&parent, "allowed", &cancel)
            .unwrap();
        let allowed = session
            .list_sync(&parent, &cancel)
            .unwrap()
            .entries
            .pop()
            .unwrap();
        session
            .delete_sync_entry(&parent, &allowed, &cancel)
            .unwrap();
        fs::write(&source, b"first version").unwrap();
        session
            .upload(&parent, &source, name, false, &cancel)
            .unwrap();
        assert!(
            session
                .upload(&parent, &source, name, false, &cancel)
                .is_err()
        );
        fs::write(&source, b"another version").unwrap();
        session
            .upload(&parent, &source, name, true, &cancel)
            .unwrap();
        session.download(&parent, name, &output, &cancel).unwrap();
        assert_eq!(fs::read(&output).unwrap(), b"another version");
        let before = session
            .list_sync(&parent, &cancel)
            .unwrap()
            .entries
            .pop()
            .unwrap();
        assert_eq!(before.name, name);
        let bounded = local.path().join("bounded");
        session
            .download_bounded(&parent, name, &bounded, before.size.unwrap(), &cancel)
            .unwrap();
        assert_eq!(fs::read(&bounded).unwrap(), b"another version");
        session.webdav_refresh_lock(&parent, 900, &cancel).unwrap();
        session.webdav_unlock(&parent, &cancel).unwrap();
        assert!(session.webdav_locks(&parent, &cancel).unwrap().is_empty());
        let locked = session.file(&parent, name).unwrap();
        session.webdav_lock(&locked, false, 600, &cancel).unwrap();
        assert!(session.webdav_lock(&locked, false, 600, &cancel).is_err());
        let own_file_lock = session.webdav_locks(&locked, &cancel).unwrap();
        assert_eq!(own_file_lock.len(), 1);
        assert!(own_file_lock[0].owned);
        assert_eq!(
            other
                .delete_sync_entry(&parent, &before, &cancel)
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        session.webdav_unlock(&locked, &cancel).unwrap();
        session
            .rename_sync_entry(&parent, &before, "Ergebnis.txt", &cancel)
            .unwrap();
        let renamed = session
            .list_sync(&parent, &cancel)
            .unwrap()
            .entries
            .pop()
            .unwrap();
        assert_eq!(renamed.name, "Ergebnis.txt");
        session
            .delete_sync_entry(&parent, &renamed, &cancel)
            .unwrap();
        let folder = session
            .list_sync(&root, &cancel)
            .unwrap()
            .entries
            .pop()
            .unwrap();
        session
            .rename_sync_entry(&root, &folder, "Neuer Ordner", &cancel)
            .unwrap();
        let folder = session
            .list_sync(&root, &cancel)
            .unwrap()
            .entries
            .pop()
            .unwrap();
        assert_eq!(folder.name, "Neuer Ordner");
        session.delete_sync_entry(&root, &folder, &cancel).unwrap();
        assert!(
            session
                .list_sync(&root, &cancel)
                .unwrap()
                .entries
                .is_empty()
        );
    }
}
