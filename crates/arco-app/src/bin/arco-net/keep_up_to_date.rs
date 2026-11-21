use super::{OpenSession, invalid, transfer_names::NameOverrides};
use arco_app::network_transfer;
use arco_app::network_watch::{WatchOptions, parse_interval, watch};
use arco_core::{
    ignore::IgnoreList,
    profile::Profile,
    sync::{FileMasks, LocalRoot},
};
use serde_json::{Value, json};
use std::io;
use std::path::Path;
use std::time::Duration;

pub(super) struct KeepUpToDate<'a> {
    local: Option<&'a str>,
    remote: Option<&'a str>,
    masks: &'a str,
    preset: Option<&'a str>,
    names: NameOverrides,
    interval: Duration,
    duration: Option<Duration>,
    delete: bool,
    initial_upload: bool,
}

impl<'a> KeepUpToDate<'a> {
    pub(super) fn parse(args: &[&'a str]) -> io::Result<Self> {
        let mut paths = Vec::new();
        let mut literal = false;
        let mut result = Self {
            local: None,
            remote: None,
            masks: "",
            preset: None,
            names: NameOverrides::default(),
            interval: Duration::from_secs(5),
            duration: None,
            delete: false,
            initial_upload: false,
        };
        let mut args = args.iter().copied().peekable();
        while let Some(arg) = args.next() {
            if !literal {
                if arg == "--" {
                    literal = true;
                    continue;
                }
                if arg == "-delete" {
                    result.delete = true;
                    continue;
                }
                if arg == "-initial-sync" {
                    result.initial_upload = true;
                    continue;
                }
                if result.names.parse(arg, &mut args)? {
                    continue;
                }
                if let Some(seconds) = arg.strip_prefix("-interval=") {
                    result.interval = parse_interval(seconds)?;
                    continue;
                }
                if let Some(seconds) = arg.strip_prefix("-duration=") {
                    result.duration = Some(Duration::from_secs(
                        seconds
                            .parse::<u64>()
                            .ok()
                            .filter(|seconds| (1..=604_800).contains(seconds))
                            .ok_or_else(|| invalid("-duration benötigt 1 bis 604800 Sekunden."))?,
                    ));
                    continue;
                }
                if let Some(mask) = arg.strip_prefix("-filemask=") {
                    if mask.len() > 4096 {
                        return Err(invalid("Die Transfermaske ist zu lang."));
                    }
                    FileMasks::new(mask)?;
                    result.masks = mask;
                    continue;
                }
                if let Some(preset) = arg.strip_prefix("-preset=") {
                    if preset.is_empty() || result.preset.replace(preset).is_some() {
                        return Err(invalid("Gib mit -preset genau eine Transfervorgabe an."));
                    }
                    continue;
                }
                if arg.starts_with('-') {
                    return Err(invalid("Unbekannte Option für keepuptodate."));
                }
            }
            paths.push(arg);
        }
        match paths.as_slice() {
            [] => (),
            [local] => result.local = Some(local),
            [local, remote] => {
                result.local = Some(local);
                result.remote = Some(remote);
            }
            _ => return Err(invalid("keepuptodate erwartet höchstens zwei Ordner.")),
        }
        Ok(result)
    }

    pub(super) fn execute(
        self,
        profile: &Profile,
        current: &OpenSession,
        local: &Path,
        mut publish: impl FnMut(Value) -> io::Result<()>,
    ) -> io::Result<(Value, bool)> {
        let local = self
            .local
            .map_or_else(|| local.to_owned(), |path| local.join(path));
        let root = LocalRoot::open(&local)?;
        let (session, directory) = &current.endpoint;
        let directory = match self.remote {
            Some(path) if path.starts_with('/') => session.directory(path)?,
            Some(path) => session.relative_directory(directory, Path::new(path))?,
            None => directory.clone(),
        };
        let session = network_transfer::resolve(
            &profile.network_transfer_presets,
            session,
            &directory,
            Some(root.path()),
            self.preset,
        )?;
        let session = self.names.session(session);
        let ignore = profile
            .ignore_enabled
            .then(|| IgnoreList::parse(&profile.ignore_rules))
            .transpose()?;
        let options = WatchOptions {
            profile_name: current.profile.as_deref(),
            ignore: ignore.as_ref(),
            interval: self.interval,
            duration: self.duration,
            delete: self.delete,
            initial_upload: self.initial_upload,
            masks: self.masks,
        };
        let work = |cancel: &std::sync::atomic::AtomicBool| {
            watch(
                root,
                (session, directory),
                options,
                cancel,
                |uploaded, deleted| {
                    let state = if uploaded == 0 && deleted == 0 {
                        "started"
                    } else {
                        "changed"
                    };
                    publish(
                        json!({"keepuptodate": state, "uploaded": uploaded, "deleted": deleted}),
                    )
                },
            )
        };
        #[cfg(windows)]
        let result = arco_windows::console_cancel::run(work);
        #[cfg(not(windows))]
        let result = work(&std::sync::atomic::AtomicBool::new(false));
        match result {
            Ok(()) => Ok((json!({"keepuptodate": "completed"}), false)),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                Ok((json!({"keepuptodate": "interrupted"}), true))
            }
            Err(error) => Err(error),
        }
    }
}
