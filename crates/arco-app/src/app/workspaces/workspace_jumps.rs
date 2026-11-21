use super::{App, Profile};
use std::io;
use std::path::Path;

const HISTORY_LIMIT: usize = 12;

pub(super) fn remember(profile: &mut Profile, name: &str) {
    profile.recent_workspaces.retain(|entry| entry != name);
    profile.recent_workspaces.insert(0, name.to_owned());
    profile.recent_workspaces.truncate(HISTORY_LIMIT);
}

impl App {
    pub(super) fn toggle_workspace_jump_list(&mut self) -> io::Result<()> {
        let mut updated = self.profile_snapshot();
        updated.workspace_jump_list = !updated.workspace_jump_list;
        publish(&mut updated, &self.profile_path)?;
        let enabled = updated.workspace_jump_list;
        self.commit_workspace_profile(updated)?;
        self.status(crate::locale::tr(if enabled {
            "Windows-Sprungliste aktiviert."
        } else {
            "Windows-Sprungliste deaktiviert."
        }));
        self.show_workspaces();
        Ok(())
    }

    pub(super) fn update_workspace_jump_list(&mut self) {
        if !self.profile.workspace_jump_list {
            return;
        }
        let mut updated = self.profile_snapshot();
        let result = publish(&mut updated, &self.profile_path).and_then(|()| {
            if updated.recent_workspaces != self.profile.recent_workspaces {
                self.commit_workspace_profile(updated)?;
            }
            Ok(())
        });
        if let Err(error) = result {
            self.status(format!(
                "{} · {error}",
                crate::locale::tr("Windows-Sprungliste")
            ));
        }
    }
}

#[cfg(windows)]
fn publish(profile: &mut Profile, profile_path: &Path) -> io::Result<()> {
    let mut entries = Vec::new();
    if profile.workspace_jump_list {
        profile
            .recent_workspaces
            .retain(|name| profile.workspaces.contains_key(name));
        for name in profile.recent_workspaces.iter().take(HISTORY_LIMIT) {
            entries.push((
                name.clone(),
                super::workspace_arguments(profile_path, name)?,
            ));
        }
    }
    let removed = arco_windows::jump_list::publish(
        &std::env::current_exe()?,
        crate::locale::tr("Arbeitsbereiche"),
        &entries,
    )?;
    let removed_names: std::collections::BTreeSet<_> = entries
        .iter()
        .filter(|(_, args)| removed.contains(args))
        .map(|(name, _)| name)
        .collect();
    profile
        .recent_workspaces
        .retain(|name| !removed_names.contains(name));
    Ok(())
}

#[cfg(not(windows))]
fn publish(_: &mut Profile, _: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Diese Funktion benötigt Windows.",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_is_bounded_and_reopening_promotes_an_existing_workspace() {
        let mut profile = Profile::default();
        for index in 0..14 {
            remember(&mut profile, &format!("Demo {index}"));
        }
        remember(&mut profile, "Demo 4");
        assert_eq!(profile.recent_workspaces.len(), HISTORY_LIMIT);
        assert_eq!(&profile.recent_workspaces[..2], ["Demo 4", "Demo 13"]);
        assert_eq!(
            profile
                .recent_workspaces
                .iter()
                .filter(|entry| *entry == "Demo 4")
                .count(),
            1
        );
    }
}
