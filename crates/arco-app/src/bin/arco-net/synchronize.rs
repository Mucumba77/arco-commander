use arco_app::network_sync::{FtpEndpoint, FtpPlan, ScriptComparison, ftp_ignores};
use arco_core::ignore::IgnoreList;
use arco_core::profile::Profile;
use arco_core::sync::{Action, Comparison, Mode, Options};
use arco_windows::ftp::Session;
use serde_json::{Value, json};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

pub(super) struct Synchronize<'a> {
    options: Options,
    comparison: ScriptComparison,
    local: Option<&'a str>,
    remote: Option<&'a str>,
    preview: bool,
    delete_local: bool,
    preset: Option<&'a str>,
}

impl<'a> Synchronize<'a> {
    pub(super) fn parse(direction: &str, args: &[&'a str]) -> io::Result<Self> {
        let mut options = Options {
            mode: match direction {
                "local" => Mode::RightToLeft,
                "remote" => Mode::LeftToRight,
                "both" => Mode::BothWays,
                _ => {
                    return Err(super::invalid(
                        "synchronize erwartet local, remote oder both.",
                    ));
                }
            },
            ..Options::default()
        };
        let mut comparison = ScriptComparison {
            time: true,
            size: false,
            checksum: false,
            mirror: false,
            preserve_time: None,
        };
        let mut paths = Vec::new();
        let mut preview = false;
        let mut delete = false;
        let mut positional = false;
        let mut preset = None;
        for &arg in args {
            match arg {
                "--" if !positional => positional = true,
                "-preview" if !positional => preview = true,
                "-delete" if !positional => delete = true,
                "-mirror" if !positional => comparison.mirror = true,
                "-timestamps" if !positional => options.timestamps_only = true,
                "-all-sizes" if !positional => options.timestamp_all_sizes = true,
                "-preservetime" if !positional => comparison.preserve_time = Some(true),
                "-nopreservetime" if !positional => comparison.preserve_time = Some(false),
                arg if !positional && arg.starts_with("-preset=") => {
                    let name = &arg["-preset=".len()..];
                    if name.is_empty() || preset.replace(name).is_some() {
                        return Err(super::invalid(
                            "Gib mit -preset genau eine Transfervorgabe an.",
                        ));
                    }
                }
                arg if !positional && arg.starts_with("-filemask=") => {
                    options.masks = arg["-filemask=".len()..].to_owned();
                }
                arg if !positional && arg.starts_with("-criteria=") => {
                    let criteria = &arg["-criteria=".len()..];
                    comparison.time = false;
                    comparison.size = false;
                    comparison.checksum = false;
                    for criterion in criteria.split(',') {
                        match criterion {
                            "time" => comparison.time = true,
                            "size" => comparison.size = true,
                            "checksum" => comparison.checksum = true,
                            "either" if criteria == "either" => {
                                comparison.time = true;
                                comparison.size = true;
                            }
                            "none" if criteria == "none" => (),
                            _ => return Err(super::invalid("Unbekanntes Abgleichkriterium.")),
                        }
                    }
                }
                arg if !positional && arg.starts_with('-') => {
                    return Err(super::invalid("Unbekannte Option für synchronize."));
                }
                path if paths.len() < 2 && !path.trim().is_empty() => paths.push(path),
                _ => {
                    return Err(super::invalid(
                        "Gib höchstens einen lokalen und einen entfernten Ordner an.",
                    ));
                }
            }
        }
        if options.timestamps_only && (delete || comparison.mirror) {
            return Err(super::invalid(
                "-timestamps lässt sich nicht mit -delete oder -mirror kombinieren.",
            ));
        }
        if options.timestamp_all_sizes && !options.timestamps_only {
            return Err(super::invalid("-all-sizes benötigt -timestamps."));
        }
        // Bidirectional updates need timestamps to order changed files.
        if options.mode == Mode::BothWays && !comparison.time {
            return Err(super::invalid(
                "synchronize both benötigt das Kriterium time.",
            ));
        }
        if options.mode == Mode::BothWays {
            comparison.mirror = false;
        }
        if delete && options.mode == Mode::LeftToRight {
            options.mode = Mode::Mirror;
        }
        options.validate()?;
        Ok(Self {
            delete_local: delete && options.mode == Mode::RightToLeft,
            options,
            comparison,
            local: paths.first().copied(),
            remote: paths.get(1).copied(),
            preview,
            preset,
        })
    }

    pub(super) fn execute(
        self,
        current_local: &Path,
        endpoint: &FtpEndpoint,
        profile: &Profile,
        name: Option<&str>,
        cancel: &AtomicBool,
    ) -> io::Result<Value> {
        let local = self
            .local
            .map_or_else(|| current_local.to_owned(), |path| current_local.join(path));
        let local = local.canonicalize()?;
        let (session, current_remote) = endpoint;
        let remote = match self.remote {
            Some(path) if path.starts_with('/') => session.directory(path)?,
            Some(path) => session.relative_directory(current_remote, Path::new(path))?,
            None => current_remote.clone(),
        };
        let session = arco_app::network_transfer::resolve(
            &profile.network_transfer_presets,
            session,
            &remote,
            Some(&local),
            self.preset,
        )?;
        let ignore = profile
            .ignore_enabled
            .then(|| IgnoreList::parse(&profile.ignore_rules))
            .transpose()?;
        let remote_ignored = ftp_ignores(ignore.as_ref(), &remote, name);
        let ignored = |path: &Path, directory| {
            remote_ignored(path, directory)
                || ignore
                    .as_ref()
                    .is_some_and(|rules| rules.local(&local.join(path), directory))
        };
        let plan = FtpPlan::for_script(
            &local,
            (session.clone(), remote.clone()),
            &self.options,
            self.comparison,
            &ignored,
            cancel,
        )?;
        let masks = !self.options.masks.is_empty() || !session.transfer_masks().is_empty();
        let local_deletions: Vec<PathBuf> = if self.delete_local {
            plan.local_obsolete(masks)
                .map(|row| row.relative.clone())
                .collect()
        } else {
            Vec::new()
        };
        if self.preview {
            let mut rows: Vec<Value> = plan
                .rows
                .iter()
                .filter_map(|row| {
                    let action = match row.action {
                        Action::CopyLeft if self.options.timestamps_only => "timestamp-local",
                        Action::CopyRight if self.options.timestamps_only => "timestamp-remote",
                        Action::Skip
                            if self.options.timestamps_only
                                && row.comparison == Comparison::Protected =>
                        {
                            "timestamp-skipped"
                        }
                        Action::CopyLeft => "download",
                        Action::CopyRight => "upload",
                        Action::DeleteRight => "delete-remote",
                        Action::Skip if row.comparison == Comparison::TypeConflict => {
                            "type-conflict"
                        }
                        Action::Skip => return None,
                    };
                    Some(json!({"path": row.relative, "action": action}))
                })
                .collect();
            rows.extend(
                local_deletions
                    .iter()
                    .map(|path| json!({"path":path, "action":"delete-local"})),
            );
            return Ok(
                json!({"synchronize": "preview", "local": local, "remote": Session::directory_path(&remote), "changes": rows}),
            );
        }
        if let Some(row) = plan
            .rows
            .iter()
            .find(|row| row.comparison == Comparison::TypeConflict)
        {
            return Err(io::Error::other(format!(
                "Datei und Ordner stehen im Konflikt: {}",
                row.relative.display()
            )));
        }
        let mut report = plan.apply(cancel, |_| {});
        if self.delete_local {
            plan.apply_local_deletions(masks, &mut report, cancel);
        }
        if report.cancelled {
            return Err(io::ErrorKind::Interrupted.into());
        }
        if let Some(error) = report.errors.first() {
            return Err(io::Error::other(format!(
                "Abgleich nach {} Änderungen mit {} Fehlern beendet. {error}",
                report.completed,
                report.errors.len()
            )));
        }
        Ok(
            json!({"synchronize": "applied", "local": local, "remote": Session::directory_path(&remote), "completed": report.completed}),
        )
    }
}
