use super::{scan_paths, scan_paths_with};
use crate::app::{App, Outcome};
use crate::commands::{Dialog, Pending};
use crate::panel::Location;
use arco_core::plugin::{Interface, Registration, WdxField, WdxMatcher};
use arco_core::profile::{WdxSearchField, WdxSearchProfile};
use arco_core::{files, tools};
use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

const MAX_CONDITIONS: usize = 16;

pub(crate) struct SearchTarget {
    side: usize,
    request: u64,
    path: PathBuf,
    masks: String,
    conditions: Vec<SearchCondition>,
    all: bool,
    name: Option<String>,
}

struct SearchCondition {
    plugin: Registration,
    field: WdxField,
    expression: String,
    matcher: WdxMatcher,
}

pub(in crate::app) struct ProfileFilter {
    masks: arco_core::sync::FileMasks,
    conditions: Vec<SearchCondition>,
    all: bool,
}

impl ProfileFilter {
    pub(in crate::app) fn compile(
        profile: &WdxSearchProfile,
        plugins: &[Registration],
    ) -> io::Result<Self> {
        if profile.conditions.is_empty() || profile.conditions.len() > MAX_CONDITIONS {
            return Err(io::Error::other(
                "Das WDX-Suchprofil enthält eine ungültige Anzahl von Bedingungen.",
            ));
        }
        let masks = arco_core::sync::FileMasks::new(&profile.masks)?;
        let conditions = profile
            .conditions
            .iter()
            .map(|field| {
                let plugin = plugins
                    .iter()
                    .find(|entry| {
                        entry.interface == Interface::Wdx
                            && entry.enabled
                            && entry
                                .path
                                .as_os_str()
                                .eq_ignore_ascii_case(field.plugin.as_os_str())
                    })
                    .ok_or_else(|| {
                        io::Error::other(format!(
                            "WDX-Plugin fehlt oder ist deaktiviert: {}",
                            field.plugin.display()
                        ))
                    })?;
                Ok(SearchCondition {
                    plugin: plugin.clone(),
                    field: field.selection.clone(),
                    expression: field.expression.clone(),
                    matcher: WdxMatcher::parse(&field.expression)?,
                })
            })
            .collect::<io::Result<_>>()?;
        Ok(Self {
            masks,
            conditions,
            all: profile.all,
        })
    }

    fn hits(
        condition: &SearchCondition,
        paths: &[&Path],
        cancel: &AtomicBool,
    ) -> io::Result<HashSet<PathBuf>> {
        let mut hits = HashSet::new();
        scan_paths_with(
            &condition.plugin,
            &condition.field,
            paths,
            cancel,
            |_| {},
            |path, value| {
                if condition.matcher.matches(&value) {
                    hits.insert(path.to_path_buf());
                }
            },
        )?;
        Ok(hits)
    }

    pub(in crate::app) fn accepts_name(&self, entry: &files::Entry) -> bool {
        entry.kind == files::Kind::File && self.masks.includes(Path::new(&entry.name))
    }

    pub(in crate::app) fn filter(
        &self,
        entries: &mut Vec<files::Entry>,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        entries.retain(|entry| self.accepts_name(entry));
        apply_conditions(entries, &self.conditions, self.all, |condition, paths| {
            Self::hits(condition, paths, cancel)
        })
    }

    pub(in crate::app) fn matches_file(
        &self,
        path: &Path,
        cancel: &AtomicBool,
    ) -> io::Result<bool> {
        for condition in &self.conditions {
            let hit = !Self::hits(condition, &[path], cancel)?.is_empty();
            if hit != self.all {
                return Ok(hit);
            }
        }
        Ok(self.all)
    }
}

impl App {
    pub(crate) fn show_wdx_search(&mut self) {
        if let Some(target) = self.wdx_search_target() {
            self.select_wdx_search_plugin(target);
        }
    }

    fn wdx_search_target(&self) -> Option<SearchTarget> {
        if self.panels[self.active].loading || !self.current().is_local() {
            self.status("Öffne einen lokalen Ordner für die WDX-Suche.");
            return None;
        }
        Some(SearchTarget {
            side: self.active,
            request: self.panels[self.active].request,
            path: self.current().path.clone(),
            masks: "*".into(),
            conditions: Vec::new(),
            all: true,
            name: None,
        })
    }

    fn select_wdx_search_plugin(&mut self, target: SearchTarget) {
        if !self.wdx_search_is_current(&target) {
            return;
        }
        let mut plugins = self
            .profile
            .plugins
            .iter()
            .filter(|entry| entry.enabled && entry.interface == Interface::Wdx)
            .cloned()
            .collect::<Vec<_>>();
        if plugins.len() == 1 {
            self.read_wdx_fields(plugins.pop().expect("one plugin"), Some(target));
            return;
        }
        if plugins.is_empty() {
            self.status("Aktiviere ein WDX-Plugin in der Plugin-Verwaltung.");
            return;
        }
        let labels = plugins
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>();
        self.dialog(
            "confirm",
            Pending::WdxSearchPlugin(target, plugins.clone()),
            Dialog {
                title: "WDX-Suche",
                description: "Wähle ein Plugin für die Suche.",
                options: &labels,
                ..Dialog::default()
            },
        );
    }

    pub(crate) fn show_wdx_criteria(
        &mut self,
        target: SearchTarget,
        plugin: Box<Registration>,
        field: WdxField,
    ) {
        if !self.wdx_search_is_current(&target) {
            return;
        }
        let masks = target.masks.clone();
        self.wdx_criteria_dialog(target, plugin, field, (&masks, ""), None);
    }

    fn wdx_criteria_dialog(
        &mut self,
        target: SearchTarget,
        plugin: Box<Registration>,
        field: WdxField,
        values: (&str, &str),
        error: Option<&io::Error>,
    ) {
        let mut description = format!(
            "{} · {}\nSuche in Unterordnern. Text sucht Teiltreffer. Verwende =, !=, <, <=, >, >= oder ~ für einen regulären Ausdruck.",
            plugin.name,
            field.label()
        );
        if let Some(error) = error {
            let _ = write!(description, "\n{error}");
        }
        self.dialog(
            "input",
            Pending::WdxSearchCriteria(target, plugin, field),
            Dialog {
                title: "Nach Plugin-Feld suchen",
                description: &description,
                value: values.0,
                second: values.1,
                placeholder: "Dateimaske",
                second_placeholder: "Feldwert, z. B. >= 1024",
                ..Dialog::default()
            },
        );
    }

    pub(super) fn wdx_search_is_current(&self, target: &SearchTarget) -> bool {
        let panel = &self.panels[target.side];
        !panel.loading
            && panel.request == target.request
            && panel.current().is_local()
            && panel.current().path == target.path
    }

    pub(crate) fn add_wdx_condition(
        &mut self,
        mut target: SearchTarget,
        plugin: Registration,
        field: WdxField,
        masks: &str,
        expression: &str,
    ) {
        if !self.wdx_search_is_current(&target) {
            return;
        }
        let matcher = match files::compile_masks(masks).and_then(|_| WdxMatcher::parse(expression))
        {
            Ok(matcher) => matcher,
            Err(error) => {
                self.wdx_criteria_dialog(
                    target,
                    Box::new(plugin),
                    field,
                    (masks, expression),
                    Some(&error),
                );
                return;
            }
        };
        target.masks = masks.into();
        target.conditions.push(SearchCondition {
            plugin,
            field,
            expression: expression.into(),
            matcher,
        });
        self.review_wdx_search(target);
    }

    fn review_wdx_search(&mut self, target: SearchTarget) {
        let mode = if target.all {
            "Alle Bedingungen"
        } else {
            "Mindestens eine Bedingung"
        };
        let mut summary = format!("Dateimaske: {}\nSuchmodus: {mode}\n", target.masks);
        let switch = if target.all {
            "Modus wechseln · Mindestens eine"
        } else {
            "Modus wechseln · Alle"
        };
        for (index, condition) in target.conditions.iter().enumerate() {
            let expression = if condition.expression.trim().is_empty() {
                "Feldwert vorhanden"
            } else {
                &condition.expression
            };
            let _ = writeln!(
                summary,
                "\n{}. {} · {} · {expression}",
                index + 1,
                condition.plugin.name,
                condition.field.label()
            );
        }
        self.dialog(
            "review",
            Pending::WdxSearchReview(target),
            Dialog {
                title: "WDX-Suche vorbereiten",
                description: "Prüfe die Bedingungen und wähle eine Aktion.",
                value: &summary,
                options: &[
                    "Suche starten",
                    switch,
                    "Bedingung hinzufügen",
                    "Letzte Bedingung entfernen",
                    "Als Suchprofil speichern",
                ],
                ..Dialog::default()
            },
        );
    }

    pub(crate) fn handle_wdx_search_review(
        &mut self,
        mut target: SearchTarget,
        option: usize,
    ) -> io::Result<()> {
        if !self.wdx_search_is_current(&target) {
            return Ok(());
        }
        match option {
            0 => return self.run_wdx_search(target),
            1 => {
                target.all = !target.all;
                self.review_wdx_search(target);
            }
            2 if target.conditions.len() < MAX_CONDITIONS => self.select_wdx_search_plugin(target),
            2 => {
                self.status(format!(
                    "Es sind bis zu {MAX_CONDITIONS} WDX-Bedingungen möglich."
                ));
                self.review_wdx_search(target);
            }
            3 => {
                target.conditions.pop();
                if target.conditions.is_empty() {
                    self.select_wdx_search_plugin(target);
                } else {
                    self.review_wdx_search(target);
                }
            }
            4 => {
                let name = target.name.clone().unwrap_or_default();
                self.dialog(
                    "input",
                    Pending::WdxSearchSave(target),
                    Dialog {
                        title: "WDX-Suchprofil speichern",
                        value: &name,
                        placeholder: "Profilname",
                        ..Dialog::default()
                    },
                );
            }
            _ => (),
        }
        Ok(())
    }

    fn run_wdx_search(&mut self, target: SearchTarget) -> io::Result<()> {
        let query = tools::Search {
            names: files::compile_masks(&target.masks)?,
            text: None,
            min_size: None,
            max_size: None,
            max_depth: None,
            show_hidden: self.profile.show_hidden,
            ignore: self.ignore_list(),
        };
        let description = crate::app::JobDescription::new(
            crate::OperationKind::Search,
            crate::display_path(&target.path),
            String::new(),
        );
        self.start_operation_job(
            "Suche nach WDX-Feldern …",
            false,
            description,
            move |job| {
                let mut directory =
                    tools::search(std::slice::from_ref(&target.path), &query, &job.cancel)?;
                job.phase("WDX-Bedingungen prüfen …");
                let count = target.conditions.len();
                apply_conditions(
                    &mut directory.entries,
                    target.conditions,
                    target.all,
                    |condition, paths| {
                        let mut hits = HashSet::new();
                        scan_paths(
                            &condition.plugin,
                            &condition.field,
                            paths,
                            &job,
                            |path, value| {
                                if condition.matcher.matches(&value) {
                                    hits.insert(path.to_path_buf());
                                }
                            },
                        )?;
                        Ok(hits)
                    },
                )?;
                Ok(Outcome::Entries {
                    side: target.side,
                    location: Location::Local(target.path),
                    request: target.request,
                    entries: directory,
                    label: format!("WDX-Suchtreffer · {count} Bedingungen"),
                    scope: arco_core::view_settings::Scope::Search,
                })
            },
        );
        Ok(())
    }

    pub(crate) fn save_wdx_search(
        &mut self,
        mut target: SearchTarget,
        name: &str,
        replace: bool,
    ) -> io::Result<()> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
            return Err(io::Error::other(
                "Gib einen Profilnamen mit bis zu 80 Zeichen ein.",
            ));
        }
        if !self.wdx_search_is_current(&target) {
            return Ok(());
        }
        if !replace && self.profile.wdx_search_profiles.contains_key(name) {
            self.dialog(
                "confirm",
                Pending::WdxSearchReplace(target, name.into()),
                Dialog {
                    title: "WDX-Suchprofil ersetzen?",
                    description: name,
                    ..Dialog::default()
                },
            );
            return Ok(());
        }
        let profile = WdxSearchProfile {
            masks: target.masks.clone(),
            all: target.all,
            conditions: target
                .conditions
                .iter()
                .map(|condition| WdxSearchField {
                    plugin: condition.plugin.path.clone(),
                    selection: condition.field.clone(),
                    expression: condition.expression.clone(),
                })
                .collect(),
        };
        self.update_wdx_search_profiles(|profiles| {
            profiles.insert(name.into(), profile);
        })?;
        self.status(format!("WDX-Suchprofil gespeichert · {name}"));
        target.name = Some(name.into());
        self.review_wdx_search(target);
        Ok(())
    }

    pub(crate) fn show_wdx_search_profiles(&mut self) {
        let names = self
            .profile
            .wdx_search_profiles
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        if names.is_empty() {
            self.status("Speichere zuerst eine WDX-Suche als Profil.");
            return;
        }
        let options = names.iter().map(String::as_str).collect::<Vec<_>>();
        self.dialog(
            "confirm",
            Pending::WdxSearchProfiles(names.clone()),
            Dialog {
                title: "WDX-Suchprofile",
                options: &options,
                ..Dialog::default()
            },
        );
    }

    pub(crate) fn wdx_search_profile_actions(&mut self, name: String) {
        self.dialog(
            "confirm",
            Pending::WdxSearchProfileAction(name.clone()),
            Dialog {
                title: "WDX-Suchprofil",
                description: &name,
                options: &["Öffnen", "Löschen"],
                ..Dialog::default()
            },
        );
    }

    pub(crate) fn load_wdx_search(&mut self, name: &str) -> io::Result<()> {
        let Some(mut target) = self.wdx_search_target() else {
            return Ok(());
        };
        let saved = self
            .profile
            .wdx_search_profiles
            .get(name)
            .ok_or_else(|| io::Error::other("Das WDX-Suchprofil fehlt."))?;
        let filter = ProfileFilter::compile(saved, &self.profile.plugins)?;
        target.masks.clone_from(&saved.masks);
        target.all = saved.all;
        target.name = Some(name.into());
        target.conditions = filter.conditions;
        self.review_wdx_search(target);
        Ok(())
    }

    pub(crate) fn confirm_wdx_search_profile_delete(&mut self, name: String) {
        self.dialog(
            "confirm",
            Pending::WdxSearchProfileDelete(name.clone()),
            Dialog {
                title: "WDX-Suchprofil löschen?",
                description: &name,
                ..Dialog::default()
            },
        );
    }

    pub(crate) fn delete_wdx_search_profile(&mut self, name: &str) -> io::Result<()> {
        if let Some((profile, _)) = self
            .profile
            .search_profiles
            .iter()
            .find(|(_, options)| options.wdx_profile.trim() == name)
        {
            return Err(io::Error::other(format!(
                "Entferne zuerst den WDX-Verweis im gemeinsamen Suchprofil „{profile}“."
            )));
        }
        self.update_wdx_search_profiles(|profiles| {
            profiles.remove(name);
        })?;
        self.status(format!("WDX-Suchprofil gelöscht · {name}"));
        Ok(())
    }

    fn update_wdx_search_profiles(
        &mut self,
        update: impl FnOnce(&mut BTreeMap<String, WdxSearchProfile>),
    ) -> io::Result<()> {
        if !self.save_profile {
            return Err(io::Error::other(
                "Das Arco-Profil konnte beim Start nicht geladen werden.",
            ));
        }
        let mut profile = self.profile_snapshot();
        update(&mut profile.wdx_search_profiles);
        if !profile
            .wdx_search_profiles
            .contains_key(profile.search_last.wdx_profile.trim())
        {
            profile.search_last.wdx_profile.clear();
        }
        profile.save(&self.profile_path)?;
        self.profile = profile;
        self.publish_search_profiles();
        Ok(())
    }
}

fn apply_conditions<C>(
    entries: &mut Vec<files::Entry>,
    conditions: impl IntoIterator<Item = C>,
    all: bool,
    mut matches: impl FnMut(C, &[&Path]) -> io::Result<HashSet<PathBuf>>,
) -> io::Result<()> {
    let mut matched = HashSet::new();
    for condition in conditions {
        let paths: Vec<_> = entries
            .iter()
            .filter(|entry| all || !matched.contains(&entry.path))
            .map(|entry| entry.path.as_path())
            .collect();
        if paths.is_empty() {
            break;
        }
        let hits = matches(condition, &paths)?;
        if all {
            entries.retain(|entry| hits.contains(&entry.path));
        } else {
            matched.extend(hits);
        }
    }
    if !all {
        entries.retain(|entry| matched.contains(&entry.path));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use arco_core::files::{Entry, Kind};

    #[test]
    fn combines_fields_without_including_files_with_no_match() {
        let paths = ["a", "b", "c", "missing"];
        for (all, expected) in [(true, vec!["b"]), (false, vec!["a", "b", "c"])] {
            let mut entries: Vec<_> = paths
                .iter()
                .map(|path| Entry::from_remote(PathBuf::from(path), Kind::File, None, None))
                .collect();
            apply_conditions(
                &mut entries,
                [["a", "b"], ["b", "c"]],
                all,
                |hits, candidates| {
                    Ok(candidates
                        .iter()
                        .copied()
                        .filter(|path| hits.iter().any(|hit| *path == Path::new(hit)))
                        .map(Path::to_path_buf)
                        .collect())
                },
            )
            .unwrap();
            assert_eq!(
                entries
                    .iter()
                    .map(|entry| entry.path.as_path())
                    .collect::<Vec<_>>(),
                expected.into_iter().map(Path::new).collect::<Vec<_>>()
            );
        }
    }
}
