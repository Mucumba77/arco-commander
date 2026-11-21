use crate::ftp::{Connection, Security, Session, Url, check_cancel, http::Response, invalid_input};
use std::collections::BTreeMap;
use std::io;
use std::sync::atomic::AtomicBool;

impl Connection {
    pub fn with_webdav_redirects(
        mut self,
        other_authority: bool,
        unencrypted: bool,
    ) -> io::Result<Self> {
        if self.security != Security::WebDav {
            return Err(invalid_input("Diese Optionen benötigen WebDAV."));
        }
        self.webdav_cross_domain_redirects = other_authority;
        self.webdav_unencrypted_redirects = unencrypted;
        Ok(self)
    }
}

impl Session {
    pub(super) fn prepare_webdav_root(
        &self,
        directory: &Url,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let root = &self.connection.url;
        if directory != root
            && !self
                .webdav_aliases
                .lock()
                .map_err(|_| aliases_unavailable())?
                .contains_key(root)
        {
            // Restored tabs can open a subfolder before the initial URL has been resolved.
            // A direct subfolder remains usable if the server denies reading its parent.
            if self.list_webdav(root, cancel).is_err() {
                check_cancel(cancel)?;
            }
        }
        Ok(())
    }

    pub(in crate::ftp) fn webdav_redirect_target(
        &self,
        logical: &Url,
        verb: &str,
        response: &Response,
    ) -> io::Result<Url> {
        let current = &response.url;
        let location = response
            .headers
            .get("location")
            .filter(|location| {
                !location.is_empty()
                    && location.len() <= 16 * 1024
                    && !location.contains('\\')
                    && !location.chars().any(char::is_control)
            })
            .ok_or_else(invalid_redirect)?;
        let mut target = current.join(location).map_err(|_| invalid_redirect())?;
        if !matches!(target.scheme(), "http" | "https")
            || target.host().is_none()
            || !target.username().is_empty()
            || target.password().is_some()
        {
            return Err(invalid_redirect());
        }
        target.set_fragment(None);
        let cross_authority = current.host_str() != target.host_str()
            || current.port_or_known_default() != target.port_or_known_default();
        if current.scheme() == "https"
            && target.scheme() == "http"
            && !self.connection.webdav_unencrypted_redirects
        {
            return Err(invalid_input(
                "Die WebDAV-Weiterleitung auf unverschlüsseltes HTTP ist deaktiviert.",
            ));
        }
        if cross_authority && !self.connection.webdav_cross_domain_redirects {
            return Err(invalid_input(
                "Die WebDAV-Weiterleitung auf einen anderen Host oder Port ist deaktiviert.",
            ));
        }
        if verb == "GET" {
            return Ok(target);
        }
        if response.status == 303 || target.query().is_some() {
            return Err(invalid_redirect());
        }
        if verb == "PROPFIND" {
            let root = &self.connection.url;
            if logical == root || contains(&self.webdav_wire_url(root)?, &target) {
                super::segments(&target)?;
                return Ok(target);
            }
            return Err(invalid_input(
                "Die WebDAV-Weiterleitung führt aus dem geöffneten Stammordner.",
            ));
        }
        // Replaying a mutation must still address the same resource. A collection's
        // trailing slash is the one canonicalization the server can request here.
        if current.origin() != target.origin()
            || target.path().strip_suffix('/') != Some(current.path())
        {
            return Err(invalid_input(
                "Der WebDAV-Schreibauftrag wurde zu einer anderen Ressource umgeleitet.",
            ));
        }
        Ok(target)
    }

    pub(in crate::ftp) fn webdav_wire_url(&self, logical: &Url) -> io::Result<Url> {
        let aliases = self
            .webdav_aliases
            .lock()
            .map_err(|_| aliases_unavailable())?;
        Ok(mapped_url(&aliases, logical))
    }

    pub(super) fn remember_webdav_alias(
        &self,
        logical: &Url,
        previous: &Url,
        effective: &Url,
    ) -> io::Result<()> {
        if previous == effective && logical != &self.connection.url {
            return Ok(());
        }
        let mut aliases = self
            .webdav_aliases
            .lock()
            .map_err(|_| aliases_unavailable())?;
        let current = mapped_url(&aliases, logical);
        if current == *effective {
            if logical == &self.connection.url {
                aliases
                    .entry(logical.clone())
                    .or_insert_with(|| effective.clone());
            }
            return Ok(());
        }
        if current != *previous {
            return Err(invalid_input(
                "Die WebDAV-Ordneradresse hat sich geändert. Lade den Ordner neu.",
            ));
        }
        if logical != &self.connection.url
            && !contains(&mapped_url(&aliases, &self.connection.url), effective)
        {
            return Err(invalid_input(
                "Die WebDAV-Weiterleitung führt aus dem geöffneten Stammordner.",
            ));
        }
        aliases.retain(|key, _| !contains(logical, key));
        if aliases.len() >= 256 {
            return Err(invalid_input(
                "Zu viele WebDAV-Ordnerweiterleitungen in dieser Verbindung.",
            ));
        }
        aliases.insert(logical.clone(), effective.clone());
        Ok(())
    }

    pub(in crate::ftp) fn forget_webdav_aliases(&self, logical: &Url) {
        if let Ok(mut aliases) = self.webdav_aliases.lock() {
            aliases.retain(|key, _| !contains(logical, key));
        }
    }
}

pub(super) fn as_directory(mut url: Url) -> io::Result<Url> {
    if !url.path().ends_with('/') {
        url.path_segments_mut()
            .map_err(|_| invalid_redirect())?
            .push("");
    }
    Ok(url)
}

fn mapped_url(aliases: &BTreeMap<Url, Url>, logical: &Url) -> Url {
    aliases
        .iter()
        .filter(|(key, _)| contains(key, logical))
        .max_by_key(|(key, _)| key.path().len())
        .map_or_else(
            || logical.clone(),
            |(key, target)| {
                let suffix = logical
                    .path()
                    .strip_prefix(key.path().trim_end_matches('/'))
                    .unwrap_or_default();
                let mut wire = target.clone();
                if !matches!(suffix, "" | "/") {
                    wire.set_path(&format!(
                        "{}{}",
                        target.path().trim_end_matches('/'),
                        suffix
                    ));
                }
                wire
            },
        )
}

fn contains(root: &Url, other: &Url) -> bool {
    let root_path = root.path().trim_end_matches('/');
    root.origin() == other.origin()
        && other
            .path()
            .strip_prefix(root_path)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

fn invalid_redirect() -> io::Error {
    invalid_input("Ungültiges Ziel der WebDAV-Weiterleitung.")
}

fn aliases_unavailable() -> io::Error {
    io::Error::other("Der WebDAV-Weiterleitungsstatus ist nicht verfügbar.")
}
