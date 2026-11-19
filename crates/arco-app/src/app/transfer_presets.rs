use super::{App, strings};
use crate::{TransferPresetData, locale::tr};
use arco_core::profile::{FtpTransfer, FtpTransferMode};
use arco_core::transfer_presets::{self, TransferPreset, TransferRule};
use std::io;

impl App {
    pub(crate) fn open_transfer_presets(&mut self) {
        self.pending = None;
        self.publish_transfer_preset(
            i32::from(!self.profile.network_transfer_presets.is_empty()),
            "",
        );
        self.ui().set_dialog_kind("transfer-presets".into());
    }

    fn publish_transfer_preset(&self, index: i32, notice: &str) {
        let ui = self.ui();
        let presets = &self.profile.network_transfer_presets;
        let index = index.clamp(0, presets.len() as i32);
        let data = if index == 0 {
            TransferPresetData::default()
        } else {
            let preset = &presets[index as usize - 1];
            let rule = preset.rule.as_ref();
            TransferPresetData {
                name: preset.name.as_str().into(),
                mode: match preset.settings.mode {
                    FtpTransferMode::Binary => 0,
                    FtpTransferMode::Text => 1,
                    FtpTransferMode::Automatic => 2,
                },
                speed: preset
                    .settings
                    .speed_kib
                    .map(|rate| rate.to_string())
                    .unwrap_or_default()
                    .into(),
                time: match preset.settings.preserve_time {
                    None => 0,
                    Some(true) => 1,
                    Some(false) => 2,
                },
                masks: preset.settings.masks.as_str().into(),
                automatic: rule.is_some(),
                host: rule.map_or("", |rule| rule.host.as_str()).into(),
                user: rule.map_or("", |rule| rule.user.as_str()).into(),
                local: rule.map_or("", |rule| rule.local.as_str()).into(),
                remote: rule.map_or("", |rule| rule.remote.as_str()).into(),
            }
        };
        ui.set_transfer_preset_names(strings(
            std::iter::once(tr("Neue Vorgabe"))
                .chain(presets.iter().map(|preset| preset.name.as_str())),
        ));
        ui.set_transfer_preset_index(index);
        ui.set_transfer_preset_data(data);
        ui.set_transfer_preset_notice(tr(notice).into());
        ui.set_transfer_preset_revision(ui.get_transfer_preset_revision().wrapping_add(1));
    }

    pub(super) fn operate_transfer_preset(
        &mut self,
        operation: &str,
        index: i32,
        data: TransferPresetData,
    ) {
        let ui = self.ui();
        if ui.get_dialog_kind() != "transfer-presets" {
            return;
        }
        if operation == "select" {
            self.publish_transfer_preset(index, "");
            return;
        }
        match self.edit_transfer_presets(operation, index, data) {
            Ok(index) => self.publish_transfer_preset(index, "Transfervorgaben gespeichert."),
            Err(error) => ui.set_transfer_preset_notice(tr(&error.to_string()).into()),
        }
    }

    fn edit_transfer_presets(
        &mut self,
        operation: &str,
        index: i32,
        data: TransferPresetData,
    ) -> io::Result<i32> {
        if !self.save_profile {
            return Err(io::Error::other(
                "Das Arco-Profil konnte beim Start nicht geladen werden.",
            ));
        }
        let mut updated = self.profile_snapshot();
        let presets = &mut updated.network_transfer_presets;
        let entry = (index > 0 && index as usize <= presets.len()).then(|| index as usize - 1);
        let mut reassign = None;
        let next = match (operation, entry) {
            ("save", entry) => {
                let preset = parse_draft(data)?;
                if let Some(entry) = entry {
                    reassign = Some((presets[entry].name.clone(), Some(preset.name.clone())));
                    presets[entry] = preset;
                    index
                } else {
                    presets.push(preset);
                    presets.len() as i32
                }
            }
            ("delete", Some(entry)) => {
                reassign = Some((presets.remove(entry).name, None));
                index.min(presets.len() as i32)
            }
            ("duplicate", Some(entry)) => {
                let mut preset = presets[entry].clone();
                let original = preset.name.chars().take(60).collect::<String>();
                let count = (1..)
                    .find(|count| {
                        let name = format!("{original} ({count})").to_lowercase();
                        !presets
                            .iter()
                            .any(|preset| preset.name.to_lowercase() == name)
                    })
                    .expect("an unused number exists");
                preset.name = format!("{original} ({count})");
                preset.rule = None;
                presets.insert(entry + 1, preset);
                index + 1
            }
            ("up", Some(entry)) if entry > 0 => {
                presets.swap(entry, entry - 1);
                index - 1
            }
            ("down", Some(entry)) if entry + 1 < presets.len() => {
                presets.swap(entry, entry + 1);
                index + 1
            }
            _ => return Err(io::Error::other("Wähle eine gespeicherte Transfervorgabe.")),
        };
        transfer_presets::validate(presets)?;
        if let Some((old, new)) = &reassign {
            for profile in updated.ftp_profiles.values_mut() {
                if profile.transfer_preset.as_ref() == Some(old) {
                    profile.transfer_preset.clone_from(new);
                }
            }
        }
        updated.save(&self.profile_path)?;
        self.profile = updated;
        if let Some((old, new)) = reassign {
            for panel in &self.panels {
                for tab in &panel.tabs {
                    if let Some(ftp) = &tab.ftp {
                        ftp.location
                            .session
                            .rename_transfer_preset(&old, new.as_deref());
                    }
                }
            }
        }
        Ok(next)
    }
}

fn parse_draft(data: TransferPresetData) -> io::Result<TransferPreset> {
    Ok(TransferPreset {
        name: data.name.trim().into(),
        settings: FtpTransfer {
            mode: match data.mode {
                1 => FtpTransferMode::Text,
                2 => FtpTransferMode::Automatic,
                _ => FtpTransferMode::Binary,
            },
            speed_kib: if data.speed.trim().is_empty() {
                None
            } else {
                Some(
                    data.speed.trim().parse().map_err(|_| {
                        io::Error::other("Gib das Durchsatzlimit in ganzen KiB/s ein.")
                    })?,
                )
            },
            preserve_time: match data.time {
                1 => Some(true),
                2 => Some(false),
                _ => None,
            },
            masks: data.masks.trim().into(),
        },
        rule: data.automatic.then(|| TransferRule {
            host: data.host.trim().into(),
            user: data.user.trim().into(),
            remote: data.remote.trim().into(),
            local: data.local.trim().into(),
        }),
    })
}
