use super::{path, scp::shell_arg, wire::Wire};
use crate::ftp::{Entry, Security, Session, Url, check_cancel, check_segment, invalid_input};
use std::{collections::BTreeSet, io, sync::atomic::AtomicBool};

const MAX_TARGET: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteLinkKind {
    Symbolic,
    Hard,
}

pub enum ResolvedLink {
    Directory(Url),
    File(Url, Entry),
}

impl Session {
    pub fn resolve_remote_link(
        &self,
        directory: &Url,
        name: &str,
        cancel: &AtomicBool,
    ) -> io::Result<ResolvedLink> {
        let link = path(&self.file(directory, name)?)?;
        let (canonical, directory, size) = match self.security() {
            Security::Sftp => {
                self.sftp_operation(cancel, |wire| resolve_sftp(wire, link, cancel))?
            }
            Security::Scp => {
                let link = shell_arg(&link);
                let reply = self.scp_output(
                    format!("test -L {link} && realpath -e -z -- {link}"),
                    MAX_TARGET + 1,
                    "Der symbolische Link verweist auf kein erreichbares Ziel.",
                    cancel,
                )?;
                let canonical =
                    str::from_utf8(reply.strip_suffix(&[0]).ok_or_else(invalid_target)?)
                        .map_err(io::Error::other)?;
                canonical_parts(canonical)?;
                let reply = self.scp_output(
                    format!("LC_ALL=C stat --printf '%f %s' -- {}", shell_arg(canonical)),
                    64,
                    "Der SCP-Server konnte den Typ des Linkziels nicht ermitteln.",
                    cancel,
                )?;
                let facts = str::from_utf8(&reply).map_err(io::Error::other)?;
                let (mode, size) = facts.split_once(' ').ok_or_else(invalid_target)?;
                let mode = u32::from_str_radix(mode, 16).map_err(io::Error::other)?;
                let directory = match mode & 0o170000 {
                    0o040000 => true,
                    0o100000 => false,
                    _ => return Err(invalid_target()),
                };
                (canonical.to_owned(), directory, size.parse::<u64>().ok())
            }
            _ => return Err(unsupported()),
        };
        let (parent, name) = canonical_parts(&canonical)?;
        if directory {
            return Ok(ResolvedLink::Directory(self.directory(&canonical)?));
        }
        Ok(ResolvedLink::File(
            self.directory(parent)?,
            Entry {
                name: name.to_owned(),
                directory: Some(false),
                symlink: false,
                size,
                modified: None,
                unique: None,
            },
        ))
    }

    pub(in crate::ftp) fn remove_remote_link(
        &self,
        directory: &Url,
        name: &str,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let link = path(&self.file(directory, name)?)?;
        match self.security() {
            Security::Sftp => self.sftp_operation(cancel, |wire| {
                if !wire.lstat(&link)?.is_symlink() {
                    return Err(changed_link());
                }
                check_cancel(cancel)?;
                wire.remove(&link, false)
            }),
            Security::Scp => {
                let link = shell_arg(&link);
                self.scp_output(
                    format!("test -L {link} && rm -- {link}"),
                    1024,
                    "Der SCP-Server konnte den symbolischen Link nicht löschen.",
                    cancel,
                )
                .map(|_| ())
            }
            _ => Err(unsupported()),
        }
    }

    pub fn remote_link_target(
        &self,
        directory: &Url,
        name: &str,
        cancel: &AtomicBool,
    ) -> io::Result<String> {
        let link = path(&self.file(directory, name)?)?;
        match self.security() {
            Security::Sftp => self.sftp_operation(cancel, |wire| wire.read_link(&link)),
            Security::Scp => {
                let output = self.scp_output(
                    format!("readlink -n -- {}", shell_arg(&link)),
                    MAX_TARGET,
                    "Der SCP-Eintrag ist kein lesbarer symbolischer Link.",
                    cancel,
                )?;
                String::from_utf8(output).map_err(io::Error::other)
            }
            _ => Err(unsupported()),
        }
    }

    pub fn create_remote_link(
        &self,
        directory: &Url,
        name: &str,
        target: &str,
        kind: RemoteLinkKind,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        check_target(target)?;
        let link = path(&self.file(directory, name)?)?;
        let folder = path(directory)?;
        let target = match kind {
            RemoteLinkKind::Hard if !target.starts_with('/') => child(&folder, target),
            _ => target.to_owned(),
        };
        match self.security() {
            Security::Sftp => self.sftp_operation(cancel, |wire| {
                match wire.lstat(&link) {
                    Ok(_) => return Err(io::ErrorKind::AlreadyExists.into()),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                    Err(error) => return Err(error),
                }
                match kind {
                    RemoteLinkKind::Symbolic => {
                        if !wire.knows_symlink_order() {
                            with_scratch(wire, &folder, |wire, scratch| {
                                wire.detect_symlink_order(scratch)
                            })?;
                        }
                        check_cancel(cancel)?;
                        wire.symlink(&target, &link)
                    }
                    RemoteLinkKind::Hard => {
                        if wire.lstat(&target)?.directory() != Some(false) {
                            return Err(invalid_input(
                                "Ein harter Link benötigt eine reguläre Datei.",
                            ));
                        }
                        wire.hard_link(&target, &link)
                    }
                }
            }),
            Security::Scp => {
                let target = shell_arg(&target);
                let link = shell_arg(&link);
                let command = match kind {
                    RemoteLinkKind::Symbolic => format!("ln -sT -- {target} {link}"),
                    RemoteLinkKind::Hard => format!(
                        "test -f {target} && test ! -L {target} && ln -T -- {target} {link}"
                    ),
                };
                self.scp_output(command, 1024, "Der SCP-Server konnte den Link nicht anlegen. Prüfe das Ziel und vorhandene Einträge.", cancel)?;
                Ok(())
            }
            security if security.is_ftp() && !self.is_http_forward_ftp() => {
                if kind == RemoteLinkKind::Hard {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "FTP unterstützt hier nur symbolische Links per SITE SYMLINK.",
                    ));
                }
                self.server_command(
                    directory,
                    &format!("SITE SYMLINK {} {}", ftp_word(&target), ftp_word(name)),
                    cancel,
                )?;
                Ok(())
            }
            _ => Err(unsupported()),
        }
    }

    pub fn update_remote_link(
        &self,
        directory: &Url,
        name: &str,
        previous: &str,
        target: &str,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        check_target(target)?;
        check_target(previous)?;
        let link = path(&self.file(directory, name)?)?;
        let folder = path(directory)?;
        match self.security() {
            Security::Sftp => self.sftp_operation(cancel, |wire| {
                with_scratch(wire, &folder, |wire, scratch| {
                    wire.detect_symlink_order(scratch)?;
                    let staged = child(scratch, "link");
                    wire.symlink(target, &staged)?;
                    let result = publish_sftp_link(wire, &link, previous, &staged, scratch, cancel);
                    if result.is_err() {
                        let _ = wire.remove(&staged, false);
                    }
                    result
                })
            }),
            Security::Scp => {
                let link = shell_arg(&link);
                let stage = child(&folder, &scratch_name()?);
                let staged_link = shell_arg(&child(&stage, "link"));
                let stage = shell_arg(&stage);
                let target = shell_arg(target);
                // Suffix the exact output before shell substitution strips trailing newlines.
                let previous = shell_arg(&format!("{previous}."));
                let command = format!(
                    "umask 077; mkdir -- {stage} || exit $?; \n\
                    ln -sT -- {target} {staged_link} && test -L {link} && \n\
                    test \"$(readlink -n -- {link} && printf .)\" = {previous} && \n\
                    mv -fT -- {staged_link} {link}; rc=$?; \n\
                    rm -f -- {staged_link} && rmdir -- {stage} || exit $?; exit \"$rc\""
                );
                self.scp_output(command, 1024, "Der SCP-Server konnte den Link nicht ändern. Prüfe, ob das bisherige Linkziel noch aktuell ist.", cancel)?;
                Ok(())
            }
            _ => Err(unsupported()),
        }
    }
}

fn resolve_sftp(
    wire: &mut Wire,
    mut link: String,
    cancel: &AtomicBool,
) -> io::Result<(String, bool, Option<u64>)> {
    if !wire.lstat(&link)?.is_symlink() {
        return Err(changed_link());
    }
    let mut visited = BTreeSet::new();
    for _ in 0..40 {
        check_cancel(cancel)?;
        let canonical = wire.real_path(&link)?;
        let (parent, _) = canonical_parts(&canonical)?;
        let attrs = wire.lstat(&canonical)?;
        if let Some(directory) = attrs.directory() {
            return Ok((canonical, directory, attrs.size));
        }
        if !attrs.is_symlink() {
            return Err(invalid_target());
        }
        // Some v3 servers normalize the path without following its final link.
        let target = wire.read_link(&canonical)?;
        check_target(&target)?;
        link = if target.starts_with('/') {
            target
        } else {
            child(parent, &target)
        };
        if !visited.insert(canonical) {
            break;
        }
    }
    Err(io::Error::other(
        "Die symbolischen Serverlinks bilden eine Schleife oder Kette mit mehr als 40 Einträgen.",
    ))
}

fn canonical_parts(canonical: &str) -> io::Result<(&str, &str)> {
    if canonical == "/" {
        return Ok(("/", ""));
    }
    let path = canonical.strip_prefix('/').ok_or_else(invalid_target)?;
    if canonical.len() > MAX_TARGET {
        return Err(invalid_target());
    }
    for component in path.split('/') {
        check_segment(component)?;
    }
    canonical.rsplit_once('/').ok_or_else(invalid_target)
}

fn invalid_target() -> io::Error {
    invalid_input("Das Linkziel ist kein gültiger Pfad zu einer regulären Datei oder einem Ordner.")
}

fn changed_link() -> io::Error {
    io::Error::other("Der symbolische Link wurde seit dem Anzeigen geändert.")
}

fn publish_sftp_link(
    wire: &mut Wire,
    link: &str,
    previous: &str,
    staged: &str,
    scratch: &str,
    cancel: &AtomicBool,
) -> io::Result<()> {
    fn check_link(wire: &mut Wire, link: &str, previous: &str) -> io::Result<()> {
        if !wire.lstat(link)?.is_symlink() || wire.read_link(link)? != previous {
            return Err(io::Error::other(
                "Das Linkziel wurde inzwischen geändert. Öffne den Dialog erneut.",
            ));
        }
        Ok(())
    }
    check_cancel(cancel)?;
    check_link(wire, link, previous)?;
    if wire.can_replace_link() {
        return wire.replace_link(staged, link);
    }
    // SFTP v3 RENAME retains an existing destination. Preserve the original in
    // the same directory tree until the server confirms publication.
    let backup = child(scratch, "vorheriger-link");
    wire.rename(link, &backup)?;
    let published = check_link(wire, &backup, previous)
        .and_then(|()| check_cancel(cancel))
        .and_then(|()| wire.rename(staged, link));
    match published {
        Ok(()) => wire.remove(&backup, false),
        Err(error) => match wire.rename(&backup, link) {
            Ok(()) => Err(error),
            Err(restore) => Err(io::Error::new(
                error.kind(),
                format!(
                    "{error}. Der bisherige Eintrag liegt möglicherweise unter {backup}: {restore}"
                ),
            )),
        },
    }
}

fn ftp_word(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "Diese Serverlink-Aktion benötigt SFTP oder SCP.",
    )
}

fn check_target(target: &str) -> io::Result<()> {
    if target.is_empty() || target.len() > MAX_TARGET || target.contains(['\0', '\r', '\n']) {
        return Err(invalid_input("Gib ein gültiges Serverlink-Ziel ein."));
    }
    Ok(())
}

fn child(folder: &str, name: &str) -> String {
    format!("{}/{name}", folder.trim_end_matches('/'))
}

fn scratch_name() -> io::Result<String> {
    let reserved = tempfile::Builder::new()
        .prefix(".arco-link-")
        .rand_bytes(16)
        .tempfile()?;
    reserved
        .path()
        .file_name()
        .and_then(|part| part.to_str())
        .map(str::to_owned)
        .ok_or_else(|| invalid_input("Temporärer Linkname fehlt."))
}

fn with_scratch<T>(
    wire: &mut Wire,
    folder: &str,
    operation: impl FnOnce(&mut Wire, &str) -> io::Result<T>,
) -> io::Result<T> {
    let scratch = child(folder, &scratch_name()?);
    wire.mkdir(&scratch)?;
    let result = operation(wire, &scratch);
    let cleanup = wire.remove(&scratch, true);
    match (result, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (result, Err(cleanup)) => {
            let message = format!("Temporären Ordner {scratch} auf dem Server prüfen: {cleanup}");
            Err(match result {
                Ok(_) => io::Error::other(message),
                Err(error) => io::Error::new(error.kind(), format!("{error}. {message}")),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ftp::Connection;

    #[test]
    #[ignore = "requires the two SFTP argument-order fixtures"]
    fn reference_sftp_server_links() {
        for prefix in ["ARCO_SFTP_LINK_", "ARCO_SFTP_REVERSED_"] {
            verify_ssh_links(prefix, Security::Sftp);
        }
    }

    #[test]
    #[ignore = "requires the SCP shell link fixture"]
    fn reference_scp_server_links() {
        verify_ssh_links("ARCO_SCP_LINK_", Security::Scp);
    }

    #[test]
    #[ignore = "requires the FTP SITE SYMLINK fixture"]
    fn reference_ftp_site_symlink() {
        let session = Connection::new(
            &std::env::var("ARCO_TEST_FTP").unwrap(),
            "arco",
            Security::Plain,
        )
        .unwrap()
        .authenticate("test-only")
        .unwrap();
        let cancel = AtomicBool::new(false);
        let directory = session.directory("arco-link-fixture").unwrap();
        let original = "Quell Datei.txt";
        let name = "Symbolischer Link.txt";
        let result = session.create_remote_link(
            &directory,
            name,
            original,
            RemoteLinkKind::Symbolic,
            &cancel,
        );
        result.unwrap();
        assert!(
            session
                .create_remote_link(
                    &directory,
                    original,
                    name,
                    RemoteLinkKind::Symbolic,
                    &cancel
                )
                .is_err()
        );
        assert!(
            session
                .create_remote_link(
                    &directory,
                    "hard.txt",
                    original,
                    RemoteLinkKind::Hard,
                    &cancel
                )
                .is_err()
        );
        let root = std::path::PathBuf::from(std::env::var_os("ARCO_TEST_FTP_ROOT").unwrap())
            .join("arco-link-fixture");
        assert_eq!(std::fs::read(root.join(name)).unwrap(), original.as_bytes());
        assert_eq!(
            std::fs::read(root.join(original)).unwrap(),
            b"preserve original"
        );
        assert!(!root.join("hard.txt").exists());
    }

    fn verify_ssh_links(prefix: &str, security: Security) {
        let variable = |suffix| std::env::var(format!("{prefix}{suffix}")).unwrap();
        let mut url = variable("URL");
        if security == Security::Scp {
            url = url.replacen("sftp://", "scp://", 1);
        }
        let session = Connection::new(&url, "arco", security)
            .unwrap()
            .with_ssh(&variable("FINGERPRINT"), None)
            .unwrap()
            .authenticate("fixture-only")
            .unwrap();
        let cancel = AtomicBool::new(false);
        let directory = session.directory("arco-link-fixture").unwrap();
        let name = "Arco's link #1";
        let first = "../missing ; '# Deutsch ä";
        let second = "source file.txt";
        session
            .create_remote_link(&directory, name, first, RemoteLinkKind::Symbolic, &cancel)
            .unwrap();
        assert_eq!(
            session
                .remote_link_target(&directory, name, &cancel)
                .unwrap(),
            first
        );
        assert!(
            session
                .resolve_remote_link(&directory, name, &cancel)
                .is_err()
        );
        assert!(
            session
                .create_remote_link(&directory, name, second, RemoteLinkKind::Hard, &cancel)
                .is_err()
        );
        assert!(
            session
                .update_remote_link(&directory, name, second, "stale write", &cancel)
                .is_err()
        );
        assert_eq!(
            session
                .remote_link_target(&directory, name, &cancel)
                .unwrap(),
            first
        );
        if security == Security::Sftp {
            assert!(
                session
                    .update_remote_link(&directory, name, first, "publication denied", &cancel)
                    .is_err()
            );
            assert_eq!(
                session
                    .remote_link_target(&directory, name, &cancel)
                    .unwrap(),
                first
            );
        }
        session
            .update_remote_link(&directory, name, first, second, &cancel)
            .unwrap();
        assert_eq!(
            session
                .remote_link_target(&directory, name, &cancel)
                .unwrap(),
            second
        );
        assert!(
            session
                .create_remote_link(&directory, second, first, RemoteLinkKind::Symbolic, &cancel)
                .is_err()
        );
        assert!(
            session
                .update_remote_link(&directory, second, first, name, &cancel)
                .is_err()
        );
        assert!(
            session
                .remote_link_target(&directory, second, &cancel)
                .is_err()
        );
        session
            .create_remote_link(
                &directory,
                "hard link.txt",
                second,
                RemoteLinkKind::Hard,
                &cancel,
            )
            .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let ResolvedLink::File(parent, target) = session
            .resolve_remote_link(&directory, name, &cancel)
            .unwrap()
        else {
            panic!("link should resolve to a file");
        };
        assert_eq!(parent, directory);
        assert_eq!(target.name, second);
        let from_link = temp.path().join("resolved.txt");
        session
            .download_bounded(&parent, &target.name, &from_link, 32, &cancel)
            .unwrap();
        assert_eq!(std::fs::read(&from_link).unwrap(), b"preserve original");
        for (link, target) in [
            ("folder alias", "target folder"),
            ("link chain", "folder alias"),
            ("link loop", "link loop"),
        ] {
            session
                .create_remote_link(&directory, link, target, RemoteLinkKind::Symbolic, &cancel)
                .unwrap();
        }
        let ResolvedLink::Directory(folder) = session
            .resolve_remote_link(&directory, "link chain", &cancel)
            .unwrap()
        else {
            panic!("link chain should resolve to a directory");
        };
        assert_eq!(
            folder,
            session
                .child_directory(&directory, "target folder")
                .unwrap()
        );
        assert!(
            session
                .resolve_remote_link(&directory, "link loop", &cancel)
                .is_err()
        );
        assert!(
            session
                .resolve_remote_link(&directory, second, &cancel)
                .is_err()
        );
        let entries = session.list_sync(&directory, &cancel).unwrap().entries;
        for link in [name, "link chain", "folder alias", "link loop"] {
            let entry = entries.iter().find(|entry| entry.name == link).unwrap();
            assert!(entry.symlink && entry.directory.is_none());
            if security == Security::Sftp && link == "folder alias" {
                let slot = session.recycle_entry(&directory, entry, &cancel).unwrap();
                let restored = session
                    .restore_recycled_entry(&slot, &directory, &cancel)
                    .unwrap();
                assert!(restored.symlink);
                assert_eq!(
                    session
                        .remote_link_target(&directory, link, &cancel)
                        .unwrap(),
                    "target folder"
                );
            }
            session
                .delete_sync_entry(&directory, entry, &cancel)
                .unwrap();
        }
        assert!(
            !session
                .list_sync(&directory, &cancel)
                .unwrap()
                .entries
                .iter()
                .any(|entry| entry.symlink)
        );
        assert_eq!(
            session.list_sync(&folder, &cancel).unwrap().entries[0].name,
            "preserved.txt"
        );
        for name in [second, "hard link.txt"] {
            let local = temp.path().join(name);
            session
                .download_preferred(&directory, name, &local, &cancel, |_| {})
                .unwrap();
            assert_eq!(std::fs::read(local).unwrap(), b"preserve original");
        }
    }
}
