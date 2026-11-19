use crate::network_sync::FtpEndpoint;
use arco_core::profile::FtpTransferMode;
use arco_core::transfer_presets::{self, TransferContext, TransferPreset};
use arco_windows::ftp::{PreferredTransferMode, Session, TransferSettings, Url};
use std::io;
use std::path::Path;
use std::sync::Arc;

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
