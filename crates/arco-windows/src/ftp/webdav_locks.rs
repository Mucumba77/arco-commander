use super::{
    Security, Session, Url, check_cancel,
    http::{self, HttpRequest},
    invalid_input,
};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io;
use std::sync::{MutexGuard, TryLockError, atomic::AtomicBool};
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

const DAV: &str = "DAV:";
const LOCK_XML: &str = r#"<d:lockinfo xmlns:d="DAV:"><d:lockscope><d:exclusive/></d:lockscope><d:locktype><d:write/></d:locktype><d:owner>Arco Commander</d:owner></d:lockinfo>"#;
const DISCOVERY_XML: &str =
    r#"<d:propfind xmlns:d="DAV:"><d:prop><d:lockdiscovery/></d:prop></d:propfind>"#;

pub(super) struct WriteLock {
    pub(super) token: Zeroizing<String>,
    recursive: bool,
    expires: Option<Instant>,
}

pub struct WebDavLockInfo {
    pub owner: String,
    pub depth: String,
    pub timeout: String,
    pub owned: bool,
}

impl Session {
    pub fn webdav_locks(&self, url: &Url, cancel: &AtomicBool) -> io::Result<Vec<WebDavLockInfo>> {
        if self.security() != Security::WebDav {
            return Err(invalid_input("Dieser Auftrag benötigt WebDAV."));
        }
        let mut request = self.http(url, "PROPFIND")?;
        request
            .header("Depth", "0")?
            .header("Content-Type", "application/xml")?
            .body(DISCOVERY_XML)?;
        let response = request.send(cancel)?.expect(&[207])?;
        let xml = http::xml(&response.body)?;
        let own = self.lock_map(cancel)?;
        let result = xml
            .descendants()
            .find(|node| {
                node.has_tag_name((DAV, "lockdiscovery"))
                    && node.ancestors().any(|parent| {
                        parent.has_tag_name((DAV, "propstat"))
                            && value(parent, "status").split_whitespace().nth(1) == Some("200")
                    })
            })
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::Unsupported,
                    "Der WebDAV-Server liefert keine Sperrinformationen.",
                )
            })?;
        Ok(result
            .children()
            .filter(|node| node.has_tag_name((DAV, "activelock")))
            .map(|node| {
                let token = child(node, "locktoken").map(|node| value(node, "href"));
                let owned = token.is_some_and(|token| {
                    own.values()
                        .any(|lock| lock.token.trim_matches(['<', '>']) == token)
                });
                let owner = child(node, "owner")
                    .map(|node| {
                        node.descendants()
                            .filter(roxmltree::Node::is_text)
                            .filter_map(|node| node.text())
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default();
                WebDavLockInfo {
                    owner: owner
                        .chars()
                        .filter(|value| !value.is_control())
                        .take(200)
                        .collect(),
                    depth: value(node, "depth").chars().take(20).collect(),
                    timeout: value(node, "timeout").chars().take(40).collect(),
                    owned,
                }
            })
            .collect())
    }

    pub fn webdav_lock(
        &self,
        url: &Url,
        recursive: bool,
        seconds: u32,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let mut own = self.lock_map(cancel)?;
        if own.contains_key(url) {
            return Err(invalid_input(
                "Arco hat für diesen Pfad schon eine Sperre. Verlängere sie oder hebe sie auf.",
            ));
        }
        let lock = self.webdav_acquire_lock(url, recursive, seconds, cancel)?;
        own.insert(url.clone(), lock);
        Ok(())
    }

    pub fn webdav_refresh_lock(
        &self,
        url: &Url,
        seconds: u32,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let mut own = self.lock_map(cancel)?;
        let lock = own.get(url).ok_or_else(missing_lock)?;
        let mut request = self.http(url, "LOCK")?;
        request
            .header("Timeout", &timeout(seconds)?)?
            .header("If", &Zeroizing::new(format!("({})", lock.token.as_str())))?
            .header("Content-Length", "0")?;
        let response = request.send(cancel)?;
        if response.status == 200 {
            own.get_mut(url).expect("owned lock").expires = expiration(&response, seconds);
        } else if matches!(response.status, 404 | 412) {
            own.remove(url);
        }
        response.expect(&[200]).map(|_| ())
    }

    pub fn webdav_unlock(&self, url: &Url, cancel: &AtomicBool) -> io::Result<()> {
        let mut own = self.lock_map(cancel)?;
        let lock = own.get(url).ok_or_else(missing_lock)?;
        let mut request = self.http(url, "UNLOCK")?;
        request.header("Lock-Token", &lock.token)?;
        let response = request.send(cancel)?;
        if matches!(response.status, 200 | 204 | 404 | 409 | 412) {
            own.remove(url);
        }
        response.expect(&[200, 204]).map(|_| ())
    }

    pub(super) fn webdav_acquire_lock(
        &self,
        url: &Url,
        recursive: bool,
        seconds: u32,
        cancel: &AtomicBool,
    ) -> io::Result<WriteLock> {
        if self.security() != Security::WebDav {
            return Err(invalid_input("Dieser Auftrag benötigt WebDAV."));
        }
        let mut request = self.http(url, "LOCK")?;
        request
            .header("Depth", if recursive { "infinity" } else { "0" })?
            .header("Timeout", &timeout(seconds)?)?
            .header("If-Match", "*")?
            .header("Content-Type", "application/xml")?
            .body(LOCK_XML)?;
        // Once dispatched, capture a successful LOCK even if the caller has cancelled.
        // Otherwise the server could retain a lock for which Arco lost the token.
        check_cancel(cancel)?;
        let mut response = request.send(&AtomicBool::new(false))?.expect(&[200])?;
        let token = response
            .headers
            .remove("lock-token")
            .map(Zeroizing::new)
            .filter(|token| valid_token(token))
            .ok_or_else(|| {
                invalid_input("Der WebDAV-Server hat keinen gültigen Sperr-Token bestätigt.")
            })?;
        let expires = expiration(&response, seconds);
        Ok(WriteLock {
            token,
            recursive,
            expires,
        })
    }

    pub(super) fn webdav_has_write_lock(&self, url: &Url, cancel: &AtomicBool) -> io::Result<bool> {
        Ok(self
            .lock_map(cancel)?
            .iter()
            .any(|(root, lock)| applies(root, lock, url)))
    }

    pub(super) fn webdav_conditions(
        &self,
        request: &mut HttpRequest<'_>,
        resources: &[(&Url, Option<&str>)],
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let own = self.lock_map(cancel)?;
        let mut header = Zeroizing::new(String::new());
        for &(url, tag) in resources {
            let tokens = own.iter().filter(|(root, lock)| applies(root, lock, url));
            let mut conditions = Zeroizing::new(String::new());
            for (_, lock) in tokens {
                write!(conditions, "{} ", lock.token.as_str()).expect("format DAV token");
            }
            if let Some(tag) = tag {
                write!(conditions, "[{tag}]").expect("format DAV ETag");
            }
            if !conditions.is_empty() {
                write!(header, "<{url}> ({}) ", conditions.trim()).expect("format DAV conditions");
            }
        }
        if !header.is_empty() {
            request.header("If", header.trim())?;
        }
        Ok(())
    }

    pub(super) fn webdav_forget_removed_locks(&self, url: &Url) {
        if let Ok(mut own) = self.webdav_locks.lock() {
            own.retain(|root, _| !within(url, root));
        }
    }

    fn lock_map(
        &self,
        cancel: &AtomicBool,
    ) -> io::Result<MutexGuard<'_, BTreeMap<Url, WriteLock>>> {
        loop {
            check_cancel(cancel)?;
            match self.webdav_locks.try_lock() {
                Ok(mut own) => {
                    let now = Instant::now();
                    own.retain(|_, lock| lock.expires.is_none_or(|expires| expires > now));
                    return Ok(own);
                }
                Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(30)),
                Err(TryLockError::Poisoned(_)) => {
                    return Err(io::Error::other("Der WebDAV-Sperrstatus ist beschädigt."));
                }
            }
        }
    }
}

fn expiration(response: &http::Response, requested: u32) -> Option<Instant> {
    let xml = http::xml(&response.body).ok();
    let timeout = response
        .headers
        .get("timeout")
        .map(String::as_str)
        .or_else(|| {
            xml.as_ref()?
                .descendants()
                .find(|node| node.has_tag_name((DAV, "activelock")))
                .map(|node| value(node, "timeout"))
        });
    if timeout.is_some_and(|timeout| timeout.eq_ignore_ascii_case("Infinite")) {
        return None;
    }
    let seconds = timeout
        .and_then(|value| value.split_once('-'))
        .filter(|(unit, _)| unit.eq_ignore_ascii_case("Second"))
        .and_then(|(_, seconds)| seconds.parse::<u32>().ok())
        .unwrap_or(requested);
    Instant::now().checked_add(Duration::from_secs(u64::from(seconds)))
}

fn within(root: &Url, url: &Url) -> bool {
    let prefix = root.path().trim_end_matches('/');
    let path = url.path().trim_end_matches('/');
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn applies(root: &Url, lock: &WriteLock, url: &Url) -> bool {
    root.path().trim_end_matches('/') == url.path().trim_end_matches('/')
        || (lock.recursive && within(root, url))
}

fn child<'a, 'input>(
    node: roxmltree::Node<'a, 'input>,
    name: &str,
) -> Option<roxmltree::Node<'a, 'input>> {
    node.children().find(|node| node.has_tag_name((DAV, name)))
}

fn value<'a>(node: roxmltree::Node<'a, '_>, name: &str) -> &'a str {
    child(node, name)
        .and_then(|node| node.text())
        .unwrap_or_default()
        .trim()
}

fn valid_token(token: &str) -> bool {
    token
        .strip_prefix('<')
        .and_then(|value| value.strip_suffix('>'))
        .is_some_and(|value| {
            !value.is_empty()
                && value.len() <= 512
                && value.contains(':')
                && value
                    .bytes()
                    .all(|byte| (33..=126).contains(&byte) && !b"<>[]()".contains(&byte))
        })
}

fn timeout(seconds: u32) -> io::Result<String> {
    if !(30..=3600).contains(&seconds) {
        return Err(invalid_input(
            "Gib für die WebDAV-Sperre eine Dauer von 30 bis 3600 Sekunden ein.",
        ));
    }
    Ok(format!("Second-{seconds}"))
}

fn missing_lock() -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        "Arco hat in dieser Verbindung keine Sperre für diesen Pfad angelegt.",
    )
}
