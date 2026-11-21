use crate::network_sync::FtpEndpoint;
use arco_core::profile::FtpTransferMode;
use arco_core::transfer_presets::{self, TransferContext, TransferPreset};
use arco_windows::ftp::{PreferredTransferMode, Session, TransferSettings, Url};
use std::io;
use std::path::Path;
use std::sync::Arc;

#[derive(Clone, Debug, Default)]
pub struct JobOptions {
    preset: Option<String>,
    speed_kib: Option<u32>,
}

impl JobOptions {
    pub fn new(preset: Option<String>, speed: &str) -> io::Result<Self> {
        Ok(Self {
            preset,
            speed_kib: parse_speed(speed)?,
        })
    }

    pub fn resolve(
        &self,
        presets: &[TransferPreset],
        session: &Arc<Session>,
        directory: &Url,
        local: Option<&Path>,
    ) -> io::Result<Arc<Session>> {
        let session = resolve(presets, session, directory, local, self.preset.as_deref())?;
        let Some(limit) = self.speed_kib else {
            return Ok(session);
        };
        let mut settings = session.transfer_settings().clone();
        settings.speed_kib = (limit != 0).then_some(limit);
        Ok(Arc::new(session.with_transfer_snapshot(settings)))
    }

    pub fn endpoint(
        &self,
        presets: &[TransferPreset],
        (session, directory): &FtpEndpoint,
        local: Option<&Path>,
    ) -> io::Result<FtpEndpoint> {
        Ok((
            self.resolve(presets, session, directory, local)?,
            directory.clone(),
        ))
    }
}

pub fn parse_speed(speed: &str) -> io::Result<Option<u32>> {
    let speed = speed.trim();
    (!speed.is_empty())
        .then(|| {
            speed
                .parse::<u32>()
                .ok()
                .filter(|rate| *rate <= 1_048_576)
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "Gib ein Durchsatzlimit von 0 bis 1048576 KiB/s ein.",
                    )
                })
        })
        .transpose()
}

pub fn resolve(
    presets: &[TransferPreset],
    session: &Arc<Session>,
    directory: &Url,
    local: Option<&Path>,
    explicit: Option<&str>,
) -> io::Result<Arc<Session>> {
    let configured = session.configured_transfer_preset();
    let configured = configured
        .as_deref()
        .filter(|name| presets.iter().any(|preset| &preset.name == name));
    let selected = transfer_presets::select(
        presets,
        explicit.or(configured),
        TransferContext {
            host: directory.host_str().unwrap_or_default(),
            user: session.transfer_username(),
            remote: &Session::directory_path(directory),
            local,
        },
    )?;
    let Some(preset) = selected else {
        return Ok(session.clone());
    };
    transfer_presets::validate(std::slice::from_ref(preset))?;
    let settings = &preset.settings;
    let mode = match (session.security().is_ftp(), settings.mode) {
        (true, FtpTransferMode::Text) => PreferredTransferMode::Text,
        (true, FtpTransferMode::Automatic) => PreferredTransferMode::Automatic,
        _ => PreferredTransferMode::Binary,
    };
    Ok(Arc::new(session.with_transfer_snapshot(TransferSettings {
        preset: Some(preset.name.clone()),
        mode,
        speed_kib: settings.speed_kib,
        preserve_time: settings.preserve_time,
        masks: settings.masks.clone(),
        names: settings.names,
    })))
}

pub fn endpoint(
    presets: &[TransferPreset],
    (session, directory): &FtpEndpoint,
    local: Option<&Path>,
    explicit: Option<&str>,
) -> io::Result<FtpEndpoint> {
    Ok((
        resolve(presets, session, directory, local, explicit)?,
        directory.clone(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use arco_core::profile::FtpTransfer;
    use arco_core::transfer_names::{NameCase, NameRules};
    use arco_core::transfer_presets::TransferRule;
    use arco_windows::ftp::{Connection, Security};

    #[test]
    fn job_overrides_preserve_profile_and_automatic_defaults_for_later_operations() {
        let session = Arc::new(
            Connection::new("ftp://transfer.invalid/current/", "arco", Security::Plain)
                .unwrap()
                .with_transfer_settings(TransferSettings {
                    speed_kib: Some(100),
                    ..Default::default()
                })
                .unwrap()
                .authenticate("fixture-only")
                .unwrap(),
        );
        let directory = session.initial_directory();
        let presets = [
            TransferPreset {
                name: "Office".into(),
                settings: FtpTransfer {
                    speed_kib: Some(80),
                    ..Default::default()
                },
                rule: Some(TransferRule {
                    host: "transfer.invalid".into(),
                    ..Default::default()
                }),
            },
            TransferPreset {
                name: "Reports".into(),
                settings: FtpTransfer {
                    mode: FtpTransferMode::Text,
                    speed_kib: Some(15),
                    preserve_time: Some(true),
                    masks: "*.txt".into(),
                    names: NameRules {
                        case: NameCase::Upper,
                        ..Default::default()
                    },
                    ..Default::default()
                },
                rule: None,
            },
        ];
        let apply = |name: Option<&str>, speed| {
            JobOptions::new(name.map(str::to_owned), speed)
                .unwrap()
                .resolve(&presets, &session, &directory, None)
                .unwrap()
        };
        let automatic = apply(None, "");
        assert_eq!(
            automatic.transfer_settings().preset.as_deref(),
            Some("Office")
        );
        assert_eq!(automatic.transfer_settings().speed_kib, Some(80));
        let selected = apply(Some("Reports"), " 32 ");
        assert_eq!(
            selected.transfer_settings().preset.as_deref(),
            Some("Reports")
        );
        assert_eq!(selected.transfer_settings().speed_kib, Some(32));
        assert_eq!(
            selected.transfer_settings().mode,
            PreferredTransferMode::Text
        );
        assert_eq!(selected.transfer_settings().preserve_time, Some(true));
        assert_eq!(selected.transfer_masks(), "*.txt");
        assert_eq!(
            selected
                .transfer_settings()
                .names
                .upload("report.txt")
                .unwrap(),
            "REPORT.TXT"
        );
        assert_eq!(
            apply(Some("Reports"), "").transfer_settings().speed_kib,
            Some(15)
        );
        assert_eq!(
            apply(Some("Reports"), "0").transfer_settings().speed_kib,
            None
        );
        assert_eq!(session.transfer_settings().speed_kib, Some(100));
        assert_eq!(apply(None, "").transfer_settings().speed_kib, Some(80));
        assert!(
            JobOptions::new(Some("Missing".into()), "")
                .unwrap()
                .resolve(&presets, &session, &directory, None)
                .is_err()
        );
        for invalid in ["-1", "1.5", "1048577", "unlimited"] {
            assert!(JobOptions::new(None, invalid).is_err());
        }
    }
}
