use crate::app::{App, Event};
use arco_core::view_settings::{Mode, ViewCommands};
use std::collections::{HashMap, VecDeque};

#[derive(Default)]
pub(in crate::app) struct Runtime {
    last: Option<Snapshot>,
    queue: VecDeque<String>,
    pub icons: HashMap<String, slint::Image>,
    loading: Option<String>,
}

#[derive(Clone)]
struct Snapshot {
    side: usize,
    tab: usize,
    location: String,
    mode: Mode,
    observed_mode: Mode,
}

impl Snapshot {
    fn mode_changed_from(&self, previous: &Self) -> bool {
        let navigated = self.side != previous.side
            || self.tab != previous.tab
            || self.location != previous.location;
        if navigated {
            previous.mode != self.mode
        } else {
            previous.observed_mode != self.mode
        }
    }
}

impl App {
    pub(in crate::app) fn poll_view_lifecycle(&mut self) {
        if self.view_lifecycle.loading.is_none() {
            let next = self
                .panels
                .iter()
                .flat_map(|panel| &panel.tabs)
                .filter_map(|tab| tab.view_override.as_ref().map(|mode| &mode.icon))
                .find(|icon| !icon.is_empty() && !self.view_lifecycle.icons.contains_key(*icon))
                .cloned();
            if let Some(icon) = next
                && self.view_lifecycle.icons.len() < 128
            {
                self.view_lifecycle.loading = Some(icon.clone());
                let sender = self.sender.clone();
                std::thread::spawn(move || {
                    let image = super::super::buttonbars::icon_image(icon.trim_start_matches('*'));
                    let _ = sender.send(Event::ViewIcon(icon, image));
                });
            }
        }
        if self.job.is_some()
            || !self.ui().get_dialog_kind().is_empty()
            || self.ui().get_settings_open()
        {
            return;
        }
        let panel = &self.panels[self.active];
        let tab = panel.current();
        if panel.loading || !tab.loaded {
            return;
        }
        let next = Snapshot {
            side: self.active,
            tab: panel.tab,
            location: tab.location().display(),
            mode: tab.view_override.clone().unwrap_or_default(),
            observed_mode: tab.view_override.clone().unwrap_or_default(),
        };
        let previous = self.view_lifecycle.last.as_ref();
        let mode_changed = previous.is_none_or(|previous| next.mode_changed_from(previous));
        let directory_changed = previous.is_none_or(|previous| previous.location != next.location);
        let tab_changed =
            previous.is_some_and(|previous| previous.side != next.side || previous.tab != next.tab);
        if mode_changed || directory_changed || tab_changed {
            self.view_lifecycle.queue.clear();
            let commands = (|| {
                let mut commands = Vec::new();
                if let Some(previous) = previous {
                    commands.extend(
                        ViewCommands::parse(&previous.mode.commands)?
                            .leaving(mode_changed, tab_changed)
                            .iter()
                            .cloned(),
                    );
                }
                commands.extend(
                    ViewCommands::parse(&next.mode.commands)?
                        .entering(mode_changed, directory_changed, tab_changed)
                        .iter()
                        .cloned(),
                );
                Ok::<_, std::io::Error>(commands)
            })();
            match commands {
                Ok(commands) => self.view_lifecycle.queue.extend(commands),
                Err(error) => self.status(error.to_string()),
            }
            self.view_lifecycle.last = Some(next);
        }
        if let Some(command) = self.view_lifecycle.queue.pop_front() {
            self.command_line(&command);
            // Internal view commands can deliberately alter presentation. Retain
            // the lifecycle identity until the next actual navigation/mode change.
            if let Some(last) = self.view_lifecycle.last.as_mut() {
                last.observed_mode = self.panels[self.active]
                    .current()
                    .view_override
                    .clone()
                    .unwrap_or_default();
            }
        }
    }

    pub(in crate::app) fn apply_view_icon(&mut self, key: String, image: Option<image::RgbaImage>) {
        self.view_lifecycle.loading = None;
        self.view_lifecycle.icons.insert(
            key,
            image
                .map(|image| crate::preview::to_slint(&image))
                .unwrap_or_default(),
        );
        self.publish(0);
        self.publish(1);
    }

    pub(in crate::app) fn publish_view_icons(&self, side: usize, data: &mut crate::PanelData) {
        use slint::{Model, ModelRc, VecModel};
        let tabs = data
            .tabs
            .iter()
            .enumerate()
            .map(|(index, mut item)| {
                if let Some(icon) = self.panels[side]
                    .tabs
                    .get(index)
                    .and_then(|tab| tab.view_override.as_ref())
                    .and_then(|mode| self.view_lifecycle.icons.get(&mode.icon))
                {
                    item.custom_icon = icon.clone();
                    item.has_icon = *icon != slint::Image::default();
                }
                item
            })
            .collect::<Vec<_>>();
        data.tabs = ModelRc::new(VecModel::from(tabs));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn presentation_commands_keep_exit_hooks_until_real_navigation_or_mode_change() {
        let original = Mode {
            identity: "Pictures".into(),
            commands: "cm_SrcBrief|cm_50Percent".into(),
            ..Default::default()
        };
        let previous = Snapshot {
            side: 0,
            tab: 0,
            location: "C:\\Pictures".into(),
            mode: original.clone(),
            observed_mode: Mode::default(),
        };
        let mut next = Snapshot {
            side: 0,
            tab: 0,
            location: previous.location.clone(),
            mode: Mode::default(),
            observed_mode: Mode::default(),
        };
        assert!(!next.mode_changed_from(&previous));
        next.location = "C:\\Documents".into();
        assert!(next.mode_changed_from(&previous));
        assert_eq!(
            ViewCommands::parse(&previous.mode.commands)
                .unwrap()
                .leaving(true, false),
            ["cm_50Percent"]
        );
        next.location.clone_from(&previous.location);
        next.mode = Mode {
            identity: "Manual view".into(),
            ..Default::default()
        };
        assert!(next.mode_changed_from(&previous));
        next.mode = original;
        next.tab = 1;
        assert!(!next.mode_changed_from(&previous));
    }
}
