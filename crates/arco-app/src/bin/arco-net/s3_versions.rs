use arco_windows::ftp::{S3Mfa, S3Versioning, S3VersioningConfig, Session, Url};
use serde_json::{Value, json};
use std::io;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use zeroize::Zeroizing;

pub(super) enum Command<'a> {
    List(&'a str),
    Download(&'a str, &'a str, &'a str),
    Restore(&'a str, &'a str),
    Delete(&'a str, &'a str, Option<MfaOptions<'a>>),
    Versioning(Option<VersioningEdit>, Option<MfaOptions<'a>>),
}

#[derive(Clone, Copy)]
pub(super) enum VersioningEdit {
    Status(S3Versioning),
    MfaDelete(bool),
}

#[derive(Clone, Copy)]
pub(super) struct MfaOptions<'a> {
    serial: &'a str,
    code_env: &'a str,
}

impl<'a> MfaOptions<'a> {
    fn parse(words: &[&'a str]) -> io::Result<Option<Self>> {
        let (serial, code_env) = match words {
            [] => return Ok(None),
            [a, b] if a.starts_with("-mfa-serial=") => (a, b),
            [a, b] => (b, a),
            _ => return Err(invalid_mfa_options()),
        };
        let serial = serial
            .strip_prefix("-mfa-serial=")
            .filter(|value| !value.is_empty());
        let code_env = code_env
            .strip_prefix("-mfa-code-env=")
            .filter(|name| !name.is_empty() && !name.chars().any(|c| c.is_control() || c == '='));
        match (serial, code_env) {
            (Some(serial), Some(code_env)) => Ok(Some(Self { serial, code_env })),
            _ => Err(invalid_mfa_options()),
        }
    }

    fn load(self) -> io::Result<S3Mfa> {
        let code = Zeroizing::new(std::env::var(self.code_env).map_err(|_| {
            super::invalid("Die Umgebungsvariable für den S3-MFA-Code ist nicht verfügbar.")
        })?);
        S3Mfa::new(self.serial, &code)
    }
}

fn invalid_mfa_options() -> io::Error {
    super::invalid("Gib -mfa-serial und -mfa-code-env gemeinsam an.")
}

impl<'a> Command<'a> {
    pub(super) fn parse(words: &[&'a str]) -> io::Result<Self> {
        Ok(match words {
            ["s3-versions", path] => Self::List(path),
            ["s3-get-version", path, id, target] => Self::Download(path, id, target),
            ["s3-restore-version", path, id] => Self::Restore(path, id),
            ["s3-delete-version", "-permanent", options @ .., path, id] => {
                Self::Delete(path, id, MfaOptions::parse(options)?)
            }
            ["s3-delete-version", ..] => {
                return Err(super::invalid(
                    "s3-delete-version benötigt -permanent, einen Objektpfad und eine Versions-ID.",
                ));
            }
            ["s3-versioning"] => Self::Versioning(None, None),
            ["s3-versioning", action, options @ ..] => {
                let edit = match *action {
                    "enable" => VersioningEdit::Status(S3Versioning::Enabled),
                    "suspend" => VersioningEdit::Status(S3Versioning::Suspended),
                    "mfa-enable" => VersioningEdit::MfaDelete(true),
                    "mfa-disable" => VersioningEdit::MfaDelete(false),
                    _ => return Err(super::invalid("Ungültige S3-Versionierungsaktion.")),
                };
                Self::Versioning(Some(edit), MfaOptions::parse(options)?)
            }
            _ => {
                return Err(super::invalid(
                    "Ungültige Argumente für den S3-Versionsbefehl.",
                ));
            }
        })
    }

    pub(super) fn run(
        self,
        session: &Session,
        directory: &Url,
        local: &Path,
        cancel: &AtomicBool,
    ) -> io::Result<Value> {
        let (path, selected) = match self {
            Self::List(path) => (path, None),
            Self::Download(path, id, _) | Self::Restore(path, id) | Self::Delete(path, id, _) => {
                (path, Some(id))
            }
            Self::Versioning(edit, mfa) => {
                let (bucket, current) = session.s3_bucket_versioning_details(directory, cancel)?;
                let next = match edit {
                    Some(VersioningEdit::Status(status)) => {
                        S3VersioningConfig { status, ..current }
                    }
                    Some(VersioningEdit::MfaDelete(mfa_delete))
                        if mfa_delete != current.mfa_delete =>
                    {
                        S3VersioningConfig {
                            status: if current.status == S3Versioning::Unversioned {
                                S3Versioning::Enabled
                            } else {
                                current.status
                            },
                            mfa_delete,
                        }
                    }
                    _ => current,
                };
                if next != current {
                    let mfa = mfa.map(MfaOptions::load).transpose()?;
                    session.update_s3_bucket_versioning(
                        directory,
                        current,
                        next,
                        mfa.as_ref(),
                        cancel,
                    )?;
                }
                return Ok(
                    json!({"bucket": bucket, "versioning": next.status.as_str(), "mfa_delete": next.mfa_delete}),
                );
            }
        };
        let (parent, name) = session.download_location(directory, path)?;
        let versions = session.list_s3_versions(&parent, &name, cancel)?;
        let Some(id) = selected else {
            let items = versions
                .into_iter()
                .map(|version| {
                    json!({
                        "id": version.id, "modified": version.modified, "latest": version.latest,
                        "size": version.size, "delete_marker": version.size.is_none(),
                    })
                })
                .collect::<Vec<_>>();
            return Ok(json!({"name": name, "versions": items}));
        };
        let version = versions
            .iter()
            .find(|version| version.id == id)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "Die gewählte S3-Version wurde nicht gefunden.",
                )
            })?;
        match self {
            Self::Download(_, _, target) => {
                let target = local.join(target);
                session.download_s3_version(&parent, &name, version, &target, cancel, |_| {})?;
                Ok(json!({"downloaded": target, "version": id}))
            }
            Self::Restore(..) => {
                let current = session.restore_s3_version(&parent, &name, version, cancel)?;
                Ok(json!({"restored": name, "source_version": id, "version": current}))
            }
            Self::Delete(_, _, mfa) => {
                let mfa = mfa.map(MfaOptions::load).transpose()?;
                session.delete_s3_version_authorized(
                    &parent,
                    &name,
                    version,
                    mfa.as_ref(),
                    cancel,
                )?;
                Ok(json!({"removed": name, "version": id, "delete_marker": version.size.is_none()}))
            }
            _ => unreachable!("an object version was selected"),
        }
    }
}
