use super::{HostKeys, SessionFields, SessionRecords, flag, invalid, parse_records, site, unmunge};
use crate::profile::{FtpProfile, read_tc_text};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

pub type ImportedWorkspace = (String, io::Result<Vec<WorkspaceTab>>);

pub enum WorkspaceTab {
    Local {
        left: PathBuf,
        right: PathBuf,
    },
    Remote {
        label: String,
        profile: Box<FtpProfile>,
    },
}

pub fn read_workspaces(path: &Path) -> io::Result<Vec<ImportedWorkspace>> {
    let input = Zeroizing::new(read_tc_text(path)?);
    let (records, keys) = parse_records(&input)?;
    workspaces(&records, &keys)
}

pub fn registry_workspaces(
    records: SessionRecords,
    host_keys: &BTreeMap<String, String>,
) -> io::Result<Vec<ImportedWorkspace>> {
    let records = records
        .into_iter()
        .map(|(name, fields)| {
            let fields = fields
                .into_iter()
                .filter(|(key, _)| {
                    super::WINSCP_FIELDS
                        .iter()
                        .any(|f| f.eq_ignore_ascii_case(key))
                })
                .map(|(key, value)| (key.to_ascii_lowercase(), unmunge(&value)))
                .collect();
            (unmunge(&name), fields)
        })
        .collect();
    let mut keys = BTreeMap::new();
    for (key, value) in host_keys {
        keys.entry(unmunge(key).to_ascii_lowercase())
            .and_modify(|entry| *entry = None)
            .or_insert(Some(value.as_str()));
    }
    workspaces(&records, &keys)
}

fn workspaces(records: &SessionRecords, keys: &HostKeys<'_>) -> io::Result<Vec<ImportedWorkspace>> {
    let mut by_name = BTreeMap::new();
    let mut groups = BTreeMap::<String, Vec<(u32, &SessionFields)>>::new();
    for (name, fields) in records {
        by_name
            .entry(name.to_lowercase())
            .and_modify(|entry| *entry = None)
            .or_insert(Some(fields));
        if !flag(field(fields, "isworkspace")) {
            continue;
        }
        let (group, index) = name
            .rsplit_once('/')
            .filter(|(group, _)| !group.is_empty())
            .ok_or_else(|| invalid("Ein WinSCP-Arbeitsbereich hat keinen gültigen Namen."))?;
        let index = u32::from_str_radix(index, 16)
            .map_err(|_| invalid("Ein WinSCP-Arbeitsbereich enthält eine ungültige Tabnummer."))?;
        groups
            .entry(group.to_owned())
            .or_default()
            .push((index, fields));
    }
    if groups.is_empty() {
        return Err(invalid(
            "Die Quelle enthält keine gespeicherten WinSCP-Arbeitsbereiche.",
        ));
    }
    Ok(groups
        .into_iter()
        .map(|(name, mut items)| {
            items.sort_by_key(|(index, _)| *index);
            let tabs = if items.windows(2).any(|pair| pair[0].0 == pair[1].0) {
                Err(invalid("Der Arbeitsbereich enthält doppelte Tabnummern."))
            } else {
                items
                    .into_iter()
                    .map(|(index, fields)| {
                        workspace_tab(fields, &by_name, keys).map_err(|error| {
                            invalid(&format!("Tab {}: {error}", u64::from(index) + 1))
                        })
                    })
                    .collect()
            };
            (name, tabs)
        })
        .collect())
}

fn workspace_tab(
    state: &SessionFields,
    by_name: &BTreeMap<String, Option<&SessionFields>>,
    keys: &HostKeys<'_>,
) -> io::Result<WorkspaceTab> {
    let mut resolved = state;
    let mut visited = BTreeSet::new();
    let mut link = field(state, "link");
    while !link.is_empty() {
        let key = link.to_lowercase();
        if !visited.insert(key.clone()) {
            return Err(invalid("WinSCP-Sitzungsverweise bilden einen Kreis."));
        }
        resolved = by_name
            .get(&key)
            .and_then(|fields| *fields)
            .ok_or_else(|| invalid("Die verknüpfte WinSCP-Sitzung fehlt oder ist mehrdeutig."))?;
        link = field(resolved, "link");
    }
    let mut settings = resolved.clone();
    if !field(state, "link").is_empty()
        && [
            "localdirectory",
            "otherlocaldirectory",
            "remotedirectory",
            "synchronizebrowsing",
        ]
        .iter()
        .any(|key| state.contains_key(*key))
    {
        for key in [
            "localdirectory",
            "otherlocaldirectory",
            "remotedirectory",
            "synchronizebrowsing",
        ] {
            settings.insert(key.to_owned(), field(state, key).to_owned());
        }
    }
    let left = local_path(field(&settings, "localdirectory"))?;
    if !field(&settings, "otherlocaldirectory").is_empty() {
        let right = local_path(field(&settings, "otherlocaldirectory"))?;
        return Ok(WorkspaceTab::Local {
            left: left.ok_or_else(|| invalid("Dem lokalen WinSCP-Tab fehlt der linke Ordner."))?,
            right: right
                .ok_or_else(|| invalid("Dem lokalen WinSCP-Tab fehlt der rechte Ordner."))?,
        });
    }
    let label = match field(state, "nameoverride") {
        "" => field(state, "link"),
        label => label,
    };
    settings.remove("isworkspace");
    settings.remove("workspace");
    let profile = site(&settings, keys)?;
    Ok(WorkspaceTab::Remote {
        label: if label.is_empty() {
            url::Url::parse(&profile.address)
                .ok()
                .and_then(|url| url.host_str().map(str::to_owned))
                .unwrap_or_default()
        } else {
            label.to_owned()
        },
        profile: Box::new(profile),
    })
}

fn local_path(raw: &str) -> io::Result<Option<PathBuf>> {
    if raw.is_empty() {
        return Ok(None);
    }
    let path = PathBuf::from(raw);
    if !path.is_absolute() || raw.chars().any(char::is_control) {
        return Err(invalid(
            "Der lokale WinSCP-Ordner benötigt einen absoluten Pfad.",
        ));
    }
    Ok(Some(path))
}

fn field<'a>(fields: &'a SessionFields, key: &str) -> &'a str {
    fields.get(key).map(String::as_str).unwrap_or_default()
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn resolves_links_and_keeps_local_and_remote_tabs_in_hex_order() {
        let input = r"
[Sessions\Atlas%20Test/000A]
IsWorkspace=1
NameOverride=Local%20pair
LocalDirectory=C:\Demo\left
OtherLocalDirectory=D:\Demo\right
[Sessions\Atlas%20Test/0002]
IsWorkspace=1
Link=Servers%2FAtlas
RemoteDirectory=/workspace/%C3%9Cbersicht
LocalDirectory=C:\Demo\workspace
SynchronizeBrowsing=1
[Sessions\Servers/Atlas]
HostName=example.test
UserName=alice
RemoteDirectory=/original
Password=fixture-secret
";
        let (records, keys) = parse_records(input).unwrap();
        let (name, tabs) = workspaces(&records, &keys).unwrap().pop().unwrap();
        assert_eq!(name, "Atlas Test");
        let tabs = tabs.unwrap();
        let [
            WorkspaceTab::Remote { label, profile },
            WorkspaceTab::Local { left, right },
        ] = tabs.as_slice()
        else {
            panic!("unexpected tabs")
        };
        assert_eq!(label, "Servers/Atlas");
        assert_eq!(
            profile.address,
            "sftp://example.test:22/workspace/%C3%9Cbersicht"
        );
        assert_eq!(
            profile.local_directory.as_deref(),
            Some(Path::new(r"C:\Demo\workspace"))
        );
        assert!(profile.synchronize_browsing);
        assert!(
            !serde_json::to_string(profile)
                .unwrap()
                .contains("fixture-secret")
        );
        assert_eq!(left, Path::new(r"C:\Demo\left"));
        assert_eq!(right, Path::new(r"D:\Demo\right"));
    }

    #[test]
    fn imports_registry_records_and_reports_circular_links_per_workspace() {
        let records = vec![
            (
                "Loop/0000".into(),
                BTreeMap::from([
                    ("IsWorkspace".into(), "1".into()),
                    ("Link".into(), "Loop/0000".into()),
                ]),
            ),
            (
                "Work/0000".into(),
                BTreeMap::from([
                    ("IsWorkspace".into(), "1".into()),
                    ("HostName".into(), "example.test".into()),
                ]),
            ),
        ];
        let result = registry_workspaces(records, &BTreeMap::new()).unwrap();
        assert!(result[0].1.is_err());
        assert!(result[1].1.is_ok());
    }
}
