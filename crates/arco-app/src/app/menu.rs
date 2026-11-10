use super::{App, keyboard};
use crate::{CommandRow, MainWindow, MenuCategory, MenuSection, shortcuts};
use slint::{ModelRc, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

type Action = (&'static str, &'static str);

struct Category {
    title: &'static str,
    icon: &'static str,
    primary: &'static [Action],
    sections: &'static [(&'static str, &'static str, &'static [Action])],
}

const MENU: &[Category] = &[
    Category {
        title: "Dateien",
        icon: "file",
        primary: &[
            ("Öffnen", "open"),
            ("Anzeigen", "view"),
            ("Bearbeiten", "edit"),
            ("Kopieren", "copy"),
            ("Verschieben", "move"),
            ("Umbenennen", "rename"),
            ("Neuer Ordner", "mkdir"),
            ("Papierkorb", "delete"),
            ("Eigenschaften", "properties"),
        ],
        sections: &[
            (
                "Öffnen und anzeigen",
                "eye",
                &[
                    ("Intern anzeigen", "view-internal"),
                    ("Lister in eigenem Fenster", "view-separate"),
                    ("Mit Windows öffnen", "system-open"),
                    ("Öffnen mit …", "open-with"),
                    ("Interne Dateiaktionen …", "internal-verbs"),
                    ("Windows-Kontextmenü", "context-menu"),
                ],
            ),
            (
                "Erstellen und löschen",
                "folderplus",
                &[
                    ("Neue Datei", "new-file"),
                    ("Windows-Verknüpfung erstellen …", "create-shortcut"),
                    ("Endgültig löschen", "delete-permanent"),
                ],
            ),
            (
                "Zwischenablage",
                "copy",
                &[
                    ("In Zwischenablage kopieren", "clipboard-copy"),
                    ("Ausschneiden", "clipboard-cut"),
                    ("Einfügen", "clipboard-paste"),
                    ("Dateinamen als Text kopieren", "copy-names"),
                    ("Dateipfade kopieren", "copy-paths"),
                    ("Dateidetails als TSV kopieren", "copy-details"),
                ],
            ),
            (
                "Transfers",
                "move",
                &[
                    ("Transfermanager", "transfer-manager"),
                    ("Auftragsfortschritt anzeigen", "job-details"),
                    ("In markierte Zielordner kopieren", "copy-to-marked"),
                    (
                        "Dateiauftrag mit Windows-Rechteabfrage …",
                        "shell-elevation",
                    ),
                ],
            ),
            (
                "Eigenschaften und Drucken",
                "edit",
                &[
                    ("Dateikommentar bearbeiten", "edit-comment"),
                    ("Attribute und Zeitstempel ändern", "edit-metadata"),
                    ("Textdatei drucken", "print-file"),
                    ("Dateiliste drucken", "print-directory"),
                    (
                        "Dateiliste mit Unterordnern drucken",
                        "print-directory-recursive",
                    ),
                ],
            ),
            (
                "Windows-Aktionen",
                "drive",
                &[
                    ("Als Administrator starten", "run-admin"),
                    ("Als anderer Benutzer starten …", "run-other-user"),
                    ("Datenträgerbezeichnung ändern …", "volume-label"),
                ],
            ),
        ],
    },
    Category {
        title: "Markieren",
        icon: "check",
        primary: &[
            ("Alles markieren", "select-all"),
            ("Nach Maske markieren", "mask"),
            ("Auswahl umkehren", "invert"),
            ("Markierung aufheben", "clear-selection"),
            ("Auswahl wiederherstellen", "restore-selection"),
        ],
        sections: &[
            (
                "Auswahl nach Maske",
                "search",
                &[
                    ("Auswahl nach Maske erweitern", "mask-add"),
                    ("Auswahl nach Maske verkleinern", "mask-remove"),
                ],
            ),
            (
                "Gespeicherte Auswahl",
                "file",
                &[
                    ("Auswahl in Dateiliste speichern", "selection-save"),
                    ("Auswahl aus Dateiliste laden", "selection-load"),
                    ("Auswahl aus Zwischenablage laden", "selection-paste"),
                ],
            ),
            (
                "Dateifenster vergleichen",
                "compare",
                &[
                    ("Dateifenster vergleichen", "compare-dirs"),
                    (
                        "Dateifenster einschließlich Ordner vergleichen",
                        "compare-dirs-folders",
                    ),
                    ("Vergleichen und gleiche Dateien ausblenden", "dir-match"),
                ],
            ),
        ],
    },
    Category {
        title: "Navigation",
        icon: "folder",
        primary: &[
            ("Zurück", "back"),
            ("Weiter", "forward"),
            ("Elternordner", "up"),
            ("Neu laden", "refresh"),
            ("Laufwerk links wählen", "drive-left"),
            ("Laufwerk rechts wählen", "drive-right"),
            ("Ordnerfavoriten", "favorites"),
        ],
        sections: &[
            (
                "Ordnerfavoriten",
                "star",
                &[
                    ("Aktuellen Ordner zu Favoriten hinzufügen …", "add-favorite"),
                    ("Ordnerfavoriten verwalten …", "favorites-manage"),
                ],
            ),
            (
                "Orte und Pfade",
                "drive",
                &[
                    ("Laufwerksstamm", "root"),
                    ("Windows-Ordner öffnen …", "known-folder"),
                    ("Windows-Desktop im Dateifenster", "shell-desktop"),
                    ("Dieser PC im Dateifenster", "shell-computer"),
                    ("Weitere Windows-Orte …", "shell-virtual-folders"),
                    ("Letzten lokalen Ordner öffnen", "shell-local"),
                    ("Pfad bearbeiten", "focus-path"),
                    ("Terminal im aktiven Ordner öffnen", "terminal"),
                    ("Befehlszeile fokussieren", "focus-command-line"),
                ],
            ),
            (
                "Ordner-Tabs",
                "folder",
                &[
                    ("Neuer Ordner-Tab", "new-tab"),
                    ("Ordner-Tab schließen", "close-tab"),
                    ("Liste der Ordner-Tabs", "tab-list"),
                    ("Kürzlich geschlossene Tabs …", "tab-reopen"),
                    ("Markierte Ordner in Tabs öffnen", "tabs-open-folders"),
                    (
                        "Markierte Ordner auf Gegenseite in Tabs öffnen",
                        "tabs-open-folders-other",
                    ),
                    ("Tab-Gruppen verwalten …", "tab-groups"),
                    ("Total-Commander-Tabs importieren", "tab-import"),
                    ("Ordner-Tabs als .tab exportieren …", "tab-export"),
                ],
            ),
        ],
    },
    Category {
        title: "Werkzeuge",
        icon: "settings",
        primary: &[
            ("Suchen", "search"),
            ("Suchen in separatem Fenster", "search-separate"),
            ("Schnellsuche", "quick-find"),
            ("Dateien vergleichen", "compare"),
            ("Verzeichnisse synchronisieren", "sync"),
            ("Mehrfach umbenennen", "multi-rename"),
            ("Befehlspalette", "palette"),
        ],
        sections: &[
            (
                "Suche und Metadaten",
                "search",
                &[
                    ("Nach Plugin-Feld suchen", "search-wdx"),
                    ("WDX-Suchprofile", "wdx-search-profiles"),
                    ("WDX-Feld ändern …", "wdx-edit"),
                    ("Duplikate suchen", "duplicates"),
                ],
            ),
            (
                "Vergleich und Abgleich",
                "compare",
                &[
                    ("Binär vergleichen", "compare-binary"),
                    ("Synchronisierungsprofile", "sync-profiles"),
                ],
            ),
            (
                "Umbenennen",
                "edit",
                &[
                    ("Umbenennvorlagen …", "rename-templates"),
                    ("Namensliste bearbeiten …", "rename-names"),
                    (
                        "Namensliste im externen Editor bearbeiten …",
                        "rename-external",
                    ),
                    ("Umbenennung zurücknehmen", "undo-rename"),
                ],
            ),
            (
                "Prüfsummen",
                "check",
                &[
                    ("Prüfsummen erzeugen", "hash"),
                    ("Prüfsummendatei prüfen", "verify"),
                    ("Prüfsumme aus Zwischenablage prüfen", "verify-clipboard"),
                ],
            ),
            (
                "Teilen und Kodieren",
                "file",
                &[
                    ("Datei teilen", "split"),
                    ("Dateien verbinden", "join"),
                    ("Dateien kodieren (MIME, UUE, XXE)", "encode-files"),
                    (
                        "Dateien dekodieren (MIME, UUE, XXE, BinHex)",
                        "decode-files",
                    ),
                ],
            ),
        ],
    },
    Category {
        title: "Archive",
        icon: "archive",
        primary: &[
            ("Öffnen", "zip-open"),
            ("Packen", "zip-pack"),
            ("Entpacken", "zip-extract"),
            ("Prüfen", "zip-test"),
            ("Mit Profil packen …", "external-pack"),
        ],
        sections: &[],
    },
    Category {
        title: "Netz",
        icon: "network",
        primary: &[
            ("FTP-Verbindungen …", "ftp-profiles"),
            ("FTP-Schnellverbindung …", "ftp-connect"),
            ("WFX-Dateisystem öffnen …", "wfx-inspect"),
        ],
        sections: &[
            (
                "FTP-Dateioperationen",
                "network",
                &[
                    ("FTP-Verzeichnis anzeigen", "ftp-list"),
                    ("FTP-Ordner wechseln", "ftp-directory"),
                    ("FTP herunterladen", "ftp-download"),
                    ("FTP hochladen", "ftp-upload"),
                    ("FTP-Unix-Rechte ändern …", "ftp-permissions"),
                    ("Offene FTP-Bearbeitungen …", "ftp-edits"),
                    ("FTP mit lokalem Ordner abgleichen", "ftp-sync"),
                    ("FTP trennen", "ftp-close"),
                ],
            ),
            (
                "Server-zu-Server (FXP)",
                "move",
                &[
                    ("FTP-Ordner als FXP-Quelle merken", "ftp-fxp-source"),
                    ("FTP direkt zwischen Servern übertragen", "ftp-fxp"),
                    ("FXP-Quelle verwerfen", "ftp-fxp-clear"),
                ],
            ),
            (
                "Windows-Netzwerk",
                "drive",
                &[
                    ("Netzlaufwerk verbinden …", "map-drive"),
                    ("Netzlaufwerk trennen …", "unmap-drive"),
                    ("Lokale Windows-Freigaben …", "shares"),
                    ("Ordner freigeben …", "share-create"),
                ],
            ),
            (
                "Dateisystem-Plugins",
                "folder",
                &[("WFX verlassen", "wfx-local")],
            ),
        ],
    },
    Category {
        title: "Ansicht",
        icon: "eye",
        primary: &[
            ("Kurzansicht", "short-view"),
            ("Detailansicht", "long-view"),
            ("Miniaturansicht", "thumbnail-view"),
            ("Kommentaransicht", "comment-view"),
            ("Schnellansicht", "quick-view"),
            ("Ordnerbaum umschalten", "directory-tree"),
        ],
        sections: &[
            (
                "Spalten und Sortierung",
                "file",
                &[
                    ("Spaltensätze …", "column-sets"),
                    ("Ansichtsmodi …", "view-modes"),
                    ("Automatische Ansichten umschalten", "auto-view-toggle"),
                    ("Nach Name", "sort-name"),
                    ("Nach Erweiterung", "sort-extension"),
                    ("Nach Datum", "sort-time"),
                    ("Nach Größe", "sort-size"),
                    ("Nach Attributen", "sort-attributes"),
                    ("Mehrfachsortierung …", "multi-sort"),
                    ("Unsortierte Dateiliste", "sort-unsorted"),
                ],
            ),
            (
                "Ordnerbäume",
                "folder",
                &[
                    ("Gemeinsamer Ordnerbaum", "separate-tree-one"),
                    ("Ordnerbaum je Fenster", "separate-tree-two"),
                    ("Separate Ordnerbäume ausblenden", "separate-tree-off"),
                ],
            ),
            (
                "Filter und Zweigansicht",
                "search",
                &[
                    ("Schnellfilter fokussieren", "focus-filter"),
                    ("Dateitypen im Tab filtern …", "tab-file-filter"),
                    ("Alle Dateien anzeigen", "clear-filter"),
                    ("Versteckte Dateien umschalten", "hidden"),
                    ("Ignorierliste umschalten", "ignore-toggle"),
                    ("Nur markierte Einträge umschalten", "only-marked"),
                    ("Nur Programme anzeigen", "only-programs"),
                    ("Zweigansicht", "branch"),
                    ("Zweigansicht der Auswahl", "branch-selected"),
                ],
            ),
            (
                "Fenster und Größen",
                "move",
                &[
                    ("Kurz- und Detailansicht wechseln", "details"),
                    ("Fenster tauschen", "swap"),
                    ("Fenster angleichen", "match"),
                    ("Anordnung wechseln", "stacked"),
                    ("Ordnergrößen berechnen", "folder-sizes"),
                    ("Größe der Auswahl berechnen", "space-usage"),
                ],
            ),
            (
                "Farbschema",
                "sun",
                &[
                    ("Farbschema wechseln", "theme"),
                    ("Windows-Farbschema umschalten", "system-theme"),
                ],
            ),
        ],
    },
    Category {
        title: "Konfigurieren",
        icon: "settings",
        primary: &[
            ("Einstellungen", "settings"),
            ("Sprache …", "language"),
            ("Plugin-Verwaltung …", "plugins"),
        ],
        sections: &[
            (
                "Oberfläche",
                "eye",
                &[
                    ("Eigene Buttonbars", "buttonbars"),
                    ("Buttonbar- und Startervorlagen …", "buttonbar-templates"),
                    ("Automatische Ansichtsregeln …", "auto-view-rules"),
                    ("TC-Ansichten importieren …", "view-rules-import"),
                    ("Datei- und Auswahlfarben …", "file-colors"),
                ],
            ),
            (
                "Dateien und Programme",
                "file",
                &[
                    ("Editor einstellen", "configure-editor"),
                    ("Interne Dateizuordnungen …", "associations"),
                    ("NTFS-Kopieroptionen …", "ntfs-options"),
                    ("Externe Packerprofile …", "packer-profiles"),
                    ("Ignorierliste bearbeiten …", "ignore-edit"),
                    ("Ignorierliste importieren …", "ignore-import"),
                ],
            ),
            (
                "Suche und Migration",
                "search",
                &[
                    ("Everything-Suche", "everything-configure"),
                    ("WDX-Spalten verwalten …", "wdx-columns"),
                    ("Total-Commander-Profil importieren", "import-tc"),
                ],
            ),
        ],
    },
    Category {
        title: "Starter",
        icon: "terminal",
        primary: &[
            ("Programme und Befehle …", "starter"),
            ("Starter bearbeiten …", "starter-edit"),
        ],
        sections: &[],
    },
    Category {
        title: "Hilfe",
        icon: "file",
        primary: &[
            ("Tastatur und Befehle", "help"),
            ("Systeminformationen", "system-information"),
            ("Windows-Version", "windows-version"),
            ("Über Arco Commander", "about"),
        ],
        sections: &[],
    },
];

pub(crate) fn actions() -> impl Iterator<Item = Action> {
    MENU.iter()
        .flat_map(|category| {
            category
                .primary
                .iter()
                .chain(category.sections.iter().flat_map(|section| section.2))
        })
        .copied()
}

fn icon(command: &str, fallback: &'static str) -> &'static str {
    match command {
        "open" | "view" | "view-internal" | "view-separate" | "quick-view" => "eye",
        "edit" | "rename" | "multi-rename" => "edit",
        "mkdir" | "new-file" => "folderplus",
        "delete" | "delete-permanent" => "trash",
        "copy" | "clipboard-copy" | "copy-to-marked" => "copy",
        "move" | "transfer-manager" => "move",
        "back" => "back",
        "forward" => "forward",
        "up" => "up",
        "refresh" => "refresh",
        "drive-left" | "drive-right" => "drive",
        "favorites" | "add-favorite" => "star",
        "search" | "search-separate" | "quick-find" | "palette" => "search",
        "sync" => "sync",
        "compare" | "compare-binary" => "compare",
        "zip-extract" => "archive-extract",
        _ => fallback,
    }
}

fn image(name: &'static str) -> slint::Image {
    thread_local! {
        static ICONS: RefCell<std::collections::HashMap<&'static str, slint::Image>> =
            RefCell::new(std::collections::HashMap::new());
    }
    ICONS.with_borrow_mut(|icons| {
        icons
            .entry(name)
            .or_insert_with(|| {
                macro_rules! svg {
                    ($file:literal) => {
                        include_str!(concat!("../../ui/icons/", $file, ".svg"))
                    };
                }
                let data = match name {
                    "eye" => svg!("eye"),
                    "edit" => svg!("edit"),
                    "copy" => svg!("copy"),
                    "move" => svg!("move"),
                    "folder" => svg!("folder"),
                    "folderplus" => svg!("folderplus"),
                    "trash" => svg!("trash"),
                    "search" => svg!("search"),
                    "archive" => svg!("archive"),
                    "archive-extract" => svg!("archive-extract"),
                    "compare" => svg!("compare"),
                    "network" => svg!("network"),
                    "sync" => svg!("sync"),
                    "refresh" => svg!("refresh"),
                    "drive" => svg!("drive"),
                    "star" => svg!("star"),
                    "settings" => svg!("settings"),
                    "terminal" => svg!("terminal"),
                    "check" => svg!("check"),
                    "sun" => svg!("sun"),
                    "back" => svg!("back"),
                    "forward" => svg!("forward"),
                    "up" => svg!("up"),
                    _ => svg!("file"),
                };
                // Native Windows menus display the original image colors. Use
                // a mid-tone stroke that stays visible on light and dark menus.
                let data = data
                    .replace("#f4fbff", "#258171")
                    .replace("#fff", "#258171");
                slint::Image::load_from_svg_data(data.as_bytes()).unwrap_or_default()
            })
            .clone()
    })
}

fn command_row(
    bindings: &shortcuts::Bindings,
    (label, key): Action,
    fallback: &'static str,
) -> CommandRow {
    let label = crate::locale::tr(label);
    let shortcut = shortcuts::keys_for(bindings, key)
        .first()
        .map(|key| shortcuts::display(key))
        .unwrap_or_default();
    CommandRow {
        key: key.into(),
        label: label.into(),
        menu_label: if shortcut.is_empty() {
            label.into()
        } else {
            format!("{label}\t{shortcut}").into()
        },
        icon: image(icon(key, fallback)),
        shortcut: shortcut.into(),
    }
}

fn model<T: Clone + 'static>(rows: impl Iterator<Item = T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(rows.collect::<Vec<_>>()))
}

pub(super) fn connect(window: &MainWindow, app: &Rc<RefCell<App>>) {
    let weak = Rc::downgrade(app);
    window.on_menu_action(move |key| {
        if let Some(app) = weak.upgrade() {
            keyboard::activate(&app, key.as_str());
        }
    });
    let weak = Rc::downgrade(app);
    window.on_palette_query(move |query| {
        if let Some(app) = weak.upgrade() {
            app.borrow().publish_palette(&query);
        }
    });
    let weak = Rc::downgrade(app);
    window.on_palette_pick(move |key| {
        if let Some(app) = weak.upgrade() {
            let ui = app.borrow().ui();
            if ui.get_dialog_kind() != "palette" {
                return;
            }
            app.borrow_mut().close_dialog();
            keyboard::activate(&app, key.as_str());
        }
    });
}

impl App {
    pub(super) fn sync_command_menu(&self) {
        self.ui()
            .set_command_menu(model(MENU.iter().map(|category| {
                let rows = |actions: &[Action], fallback| {
                    model(
                        actions
                            .iter()
                            .map(|action| command_row(&self.bindings, *action, fallback)),
                    )
                };
                MenuCategory {
                    title: crate::locale::tr(category.title).into(),
                    primary: rows(category.primary, category.icon),
                    sections: model(category.sections.iter().map(|&(title, icon, actions)| {
                        MenuSection {
                            title: crate::locale::tr(title).into(),
                            icon: image(icon),
                            rows: rows(actions, icon),
                        }
                    })),
                }
            })));
    }

    pub(crate) fn show_palette(&mut self) {
        self.publish_palette("");
        self.dialog(
            "palette",
            crate::commands::Pending::Palette,
            crate::commands::Dialog::default(),
        );
    }

    fn publish_palette(&self, query: &str) {
        let alias = crate::commands::compatibility_command(query.trim());
        let words: Vec<_> = query.split_whitespace().map(str::to_lowercase).collect();
        let mut seen = std::collections::HashSet::new();
        let rows = MENU
            .iter()
            .flat_map(|category| {
                category
                    .primary
                    .iter()
                    .map(move |action| (*action, category.icon))
                    .chain(category.sections.iter().flat_map(|&(_, icon, actions)| {
                        actions.iter().map(move |action| (*action, icon))
                    }))
            })
            .chain(shortcuts::commands().map(|action| (action, "file")))
            .filter_map(|(action @ (_, key), icon)| {
                if key == "palette" || !seen.insert(key) {
                    return None;
                }
                let row = command_row(&self.bindings, action, icon);
                let raw_keys = shortcuts::keys_for(&self.bindings, key).join(" ");
                let searchable =
                    format!("{} {} {} {raw_keys}", row.label, row.key, row.shortcut).to_lowercase();
                (alias == Some(key) || words.iter().all(|word| searchable.contains(word)))
                    .then_some(row)
            });
        self.ui().set_palette_rows(model(rows));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_commands_are_unique_and_shortcut_hints_follow_the_profile() {
        let commands = actions().map(|(_, key)| key).collect::<Vec<_>>();
        let unique: std::collections::HashSet<_> = commands.iter().copied().collect();
        assert_eq!(commands.len(), unique.len());
        assert!(
            [
                "open",
                "copy",
                "zip-pack",
                "transfer-manager",
                "palette",
                "context-menu"
            ]
            .into_iter()
            .all(|key| unique.contains(key))
        );
        let mut overrides = shortcuts::Overrides::new();
        shortcuts::change(&mut overrides, "Ctrl+Shift+J".into(), Some("copy"));
        shortcuts::change(&mut overrides, "F5".into(), None);
        let bindings = shortcuts::effective(&overrides);
        assert_eq!(
            command_row(&bindings, ("Kopieren", "copy"), "file").shortcut,
            "Strg+Umschalt+J"
        );
        assert!(
            command_row(&bindings, ("Neu", "some-unbound-command"), "file")
                .shortcut
                .is_empty()
        );
    }
}
