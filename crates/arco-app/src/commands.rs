use crate::app::{
    App, ArchiveInput, FtpEdit, FtpEndpoint, FtpView, Navigation, Outcome, PluginFileAction,
    PromptTicket, SyncReview, SyncSetup, Viewer, WdxSearchTarget, ftp_modified, strings,
};
use crate::panel::{ArchiveTab, FtpLocation, Location, Tab, WfxLocation};
use crate::preview;
use crate::shortcuts;
use crate::{FtpRowData, display_path, human_size};
use arco_core::archive_view::{self, Selection, is_zip};
use arco_core::archives;
use arco_core::external_archives::{self, ExternalArchiver, Format as ExternalFormat};
use arco_core::files::{self, Conflict, Kind, SortKey, Transfer, TransferReport};
use arco_core::plugin::{self, Interface};
use arco_core::profile::{self, TcImport};
use arco_core::sync::MarkRule;
use arco_core::tools::{Hash, Preview, PreviewMode};
use arco_core::{rename, tools};
use arco_windows::ftp;
use regex::Regex;
use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

pub enum Pending {
    WfxPrompt(PromptTicket),
    WfxPanelEdit(WfxLocation, Option<String>),
    WfxPanelBatch(WfxLocation, Vec<(String, plugin::Operation)>),
    WfxContext,
    WdxChoose(PathBuf, Vec<String>),
    WdxSearchPlugin(WdxSearchTarget, Vec<plugin::Registration>),
    WdxSearchField(WdxSearchTarget, Box<plugin::Registration>, Vec<String>),
    WdxSearchCriteria(WdxSearchTarget, Box<plugin::Registration>, String),
    WdxSearchReview(WdxSearchTarget),
    WdxSearchSave(WdxSearchTarget),
    WdxSearchReplace(WdxSearchTarget, String),
    WdxSearchProfiles(Vec<String>),
    WdxSearchProfileAction(String),
    WdxSearchProfileDelete(String),
    WdxRemove(Vec<plugin::WdxColumn>),
    Create(PathBuf, bool),
    Rename(Vec<PathBuf>, bool),
    ApplyRename(rename::Plan),
    Transfer(Vec<PathBuf>, PathBuf, Transfer),
    Delete(Vec<PathBuf>, bool),
    Search(usize, u64, Location, Option<ArchiveTab>),
    ArchiveSearchPassword(usize, u64, ArchiveTab, tools::Search),
    SyncSetup(SyncSetup),
    SyncReview(Box<SyncReview>),
    Hash(Vec<PathBuf>, PathBuf, bool),
    Split(PathBuf, PathBuf),
    Join(Vec<PathBuf>, PathBuf),
    ZipPack(Vec<PathBuf>, PathBuf, PathBuf),
    ZipRead(PathBuf, PathBuf, bool),
    ArchiveExtract(ArchiveTab, PathBuf, Selection),
    ArchiveInsert(ArchiveInput, ArchiveTab),
    ArchivePreview(ArchiveTab, PathBuf),
    ArchiveRemove(ArchiveTab, Vec<PathBuf>),
    ArchiveRename(ArchiveTab, PathBuf),
    ArchiveContext,
    FtpConnect(usize),
    FtpPassword(usize, ftp::Connection),
    FtpProfilePassword(usize, ftp::Connection, Option<PathBuf>),
    FtpProfileDelete(String),
    FtpProfileImport,
    ApplyFtpImport(std::collections::BTreeMap<String, profile::FtpProfile>),
    FtpDirectory(FtpEndpoint, Option<usize>),
    FtpDownload(FtpEndpoint, PathBuf),
    FtpUpload(FtpEndpoint, PathBuf),
    FtpPanelEdit(FtpLocation, FtpEdit),
    FtpPanelContext,
    FtpEditChoose(Vec<PathBuf>),
    FtpEditAction(PathBuf),
    FtpEditDelete(PathBuf),
    TabMenu,
    TabRename,
    TabList,
    TabClose,
    ReopenTab,
    TabGroups(Vec<String>),
    TabGroupSave,
    TabGroupAction(String),
    TabGroupReplace(String),
    TabGroupDelete(String),
    TabImport,
    TabExport(Option<String>),
    ApplyTabImport(String, Box<profile::TcTabs>),
    FtpFxp(FtpEndpoint, FtpEndpoint),
    Mask(MaskEdit),
    Favorite,
    Editor,
    Palette,
    ImportTc,
    ApplyTc(TcImport, Vec<(String, &'static str)>),
    InspectPlugin(Interface, PathBuf),
    RegisteredPlugin(Interface, PathBuf, Vec<usize>),
    RegisteredWfx(usize),
    PluginAdd,
    PluginEdit(usize),
    PluginRemove(usize),
    PluginFiles(usize),
    PluginFileInput(usize, PluginFileAction, PathBuf),
    ApplyPluginFile(usize, PathBuf, Box<plugin::Operation>),
    PluginPackSelection(usize, Vec<PathBuf>),
    ApplyPluginBatch(usize, PathBuf, Vec<(String, plugin::Operation)>),
    PluginPackage,
    ApplyPluginPackage(Box<plugin::Package>),
    PluginImport,
    ApplyPluginImport(Vec<plugin::Registration>),
}

pub enum MaskEdit {
    Replace,
    Add { folders: bool },
    Remove { folders: bool },
}

#[derive(Default)]
pub(crate) struct Dialog<'a> {
    pub(crate) title: &'a str,
    pub(crate) description: &'a str,
    pub(crate) value: &'a str,
    pub(crate) second: &'a str,
    pub(crate) placeholder: &'a str,
    pub(crate) second_placeholder: &'a str,
    pub(crate) options: &'a [&'a str],
    pub(crate) secret: bool,
    pub(crate) second_secret: bool,
}

const HASHES: &[&str] = &[
    "SHA256", "BLAKE3", "CRC32", "MD5", "SHA1", "SHA224", "SHA384", "SHA512", "SHA3_224",
    "SHA3_256", "SHA3_384", "SHA3_512",
];

pub const COMMANDS: &[(&str, &str)] = &[
    ("Suchen", "search"),
    ("Nach Plugin-Feld suchen", "search-wdx"),
    ("WDX-Suchprofile", "wdx-search-profiles"),
    ("Vergleichen", "compare"),
    ("Binär vergleichen", "compare-binary"),
    ("Dateifenster vergleichen", "compare-dirs"),
    (
        "Dateifenster einschließlich Ordner vergleichen",
        "compare-dirs-folders",
    ),
    (
        "Nur einseitig vorhandene Dateien markieren",
        "compare-dirs-unique",
    ),
    (
        "Nur einseitig vorhandene Dateien und Ordner markieren",
        "compare-dirs-folders-unique",
    ),
    ("Dateifenster nach Größe vergleichen", "compare-dirs-size"),
    (
        "Dateifenster nach Größe einschließlich Ordner vergleichen",
        "compare-dirs-folders-size",
    ),
    (
        "Dateifenster vergleichen und gleiche Dateien ausblenden",
        "dir-match",
    ),
    ("Synchronisieren", "sync"),
    ("Größe der Auswahl berechnen", "space-usage"),
    ("Ordnergrößen in der Dateiliste anzeigen", "folder-sizes"),
    ("Favoriten", "favorites"),
    ("Befehlspalette", "palette"),
    ("Zweigansicht", "branch"),
    ("Zweigansicht der Auswahl", "branch-selected"),
    ("Mehrfach umbenennen", "multi-rename"),
    ("Umbenennung zurücknehmen", "undo-rename"),
    ("Prüfsummen erzeugen", "hash"),
    ("Prüfsummen prüfen", "verify"),
    ("Duplikate suchen", "duplicates"),
    ("Datei teilen", "split"),
    ("Dateien verbinden", "join"),
    ("Archiv öffnen", "zip-open"),
    ("Dateien packen", "zip-pack"),
    ("Archiv entpacken", "zip-extract"),
    ("Archiv prüfen", "zip-test"),
    ("FTP verbinden", "ftp-connect"),
    ("FTP-Verbindungsprofile", "ftp-profiles"),
    ("FTP-Unix-Rechte ändern", "ftp-permissions"),
    ("Offene FTP-Bearbeitungen", "ftp-edits"),
    ("Ordner-Tab verwalten", "tab-menu"),
    ("Liste der Ordner-Tabs", "tab-list"),
    ("Geschlossenen Ordner-Tab wieder öffnen", "tab-reopen"),
    ("Markierte Ordner in Tabs öffnen", "tabs-open-folders"),
    (
        "Markierte Ordner in Tabs der Gegenseite öffnen",
        "tabs-open-folders-other",
    ),
    ("Tab-Gruppen verwalten", "tab-groups"),
    ("Total-Commander-Tabs importieren", "tab-import"),
    ("Ordner-Tabs als .tab exportieren", "tab-export"),
    ("Ordner-Tab sperren oder freigeben", "tab-lock"),
    ("Ordner-Tab mit Verzeichniswechsel sperren", "tab-lock-base"),
    ("Zum Stammordner des gesperrten Tabs", "tab-base"),
    ("FTP-Verzeichnis anzeigen", "ftp-list"),
    ("FTP-Ordner wechseln", "ftp-directory"),
    ("FTP herunterladen", "ftp-download"),
    ("FTP hochladen", "ftp-upload"),
    ("FTP mit lokalem Ordner abgleichen", "ftp-sync"),
    ("FTP-Ordner als FXP-Quelle merken", "ftp-fxp-source"),
    ("FTP direkt zwischen Servern übertragen", "ftp-fxp"),
    ("FXP-Quelle verwerfen", "ftp-fxp-clear"),
    ("FTP trennen", "ftp-close"),
    ("Total-Commander-Profil importieren", "import-tc"),
    ("WCX-Archiv auflisten", "wcx-inspect"),
    ("Plugin-Verwaltung", "plugins"),
    ("Datei intern anzeigen", "view-internal"),
    ("WDX-Metadaten abfragen", "wdx-inspect"),
    ("WDX-Spalten verwalten", "wdx-columns"),
    ("WFX-Verzeichnis auflisten", "wfx-inspect"),
    ("WFX verlassen und lokalen Ordner anzeigen", "wfx-local"),
    ("Datei mit WLX anzeigen", "wlx-view"),
    ("Neuen Favoriten merken", "add-favorite"),
    ("Dateipfade kopieren", "copy-paths"),
    ("Dateinamen als Text kopieren", "copy-names"),
    ("Dateidetails als TSV kopieren", "copy-details"),
    ("Pfad des aktiven Fensters kopieren", "copy-source-path"),
    ("Pfad des anderen Fensters kopieren", "copy-target-path"),
    ("Auswahl nach Maske", "mask"),
    ("Auswahl umkehren", "invert"),
    ("Auswahl wiederherstellen", "restore-selection"),
    ("Detailansicht umschalten", "details"),
    ("Schnellansicht umschalten", "quick-view"),
    ("Versteckte Dateien umschalten", "hidden"),
    ("Fenster tauschen", "swap"),
    ("Fenster angleichen", "match"),
    ("Anordnung wechseln", "stacked"),
    ("Helles/Dunkles Thema", "theme"),
    ("Terminal öffnen", "terminal"),
    ("Editor einstellen", "configure-editor"),
    ("Einstellungen", "settings"),
    ("Explorer öffnen", "explorer"),
    ("Eigenschaften", "properties"),
    ("Hilfe", "help"),
];

impl App {
    pub fn command_line(&mut self, line: &str) -> bool {
        let line = line.trim();
        let command = line.split_once(char::is_whitespace);
        if let Some((verb, file)) = command
            && self.tc_tab_command(verb, file)
        {
            self.ui().invoke_focus_panels();
            return true;
        }
        if let Some((verb, path)) = command
            && verb.eq_ignore_ascii_case("cd")
            && !path.trim().is_empty()
        {
            self.navigate_text(path);
            self.ui().invoke_focus_panels();
            return true;
        }
        if let Some(command) = compatibility_command(line) {
            self.action(command);
            return true;
        }
        if let Some((_, command)) = COMMANDS.iter().find(|(label, command)| {
            command.eq_ignore_ascii_case(line) || label.eq_ignore_ascii_case(line)
        }) {
            self.action(command);
            return true;
        }
        if !line.is_empty() {
            self.status(
                "Unbekannter Befehl. Verwende cd <Pfad> oder einen Befehl aus der Befehlspalette.",
            );
        }
        false
    }

    pub fn action(&mut self, command: &str) {
        let command = compatibility_command(command).unwrap_or(command);
        let command = COMMANDS
            .iter()
            .find(|(label, _)| *label == command)
            .map_or(command, |(_, id)| *id);
        if self.navigate_key(command) {
            return;
        }
        if self.wfx_command(command) {
            return;
        }
        let side = self.active;
        let hidden = self.profile.show_hidden;
        let ftp = self.current().ftp.is_some();
        let passive_ftp = self.panels[1 - side].current().ftp.is_some();
        if ftp && command == "edit" {
            self.edit_ftp_file();
            return;
        }
        if matches!(command, "copy" | "zip-extract")
            && self.panels.iter().any(|panel| panel.loading)
        {
            self.status("Warte, bis beide Dateifenster geladen sind.");
            return;
        }
        if ftp
            && matches!(
                command,
                "search" | "quick-view" | "zip-extract" | "zip-test"
            )
        {
            self.status("Verwende F3 für die FTP-Vorschau oder wechsle für diesen Befehl in einen lokalen Ordner.");
            return;
        }
        if !self.current().is_local()
            && !(ftp && command == "mkdir")
            && !matches!(
                command,
                "switch-panel"
                    | "cursor-up"
                    | "cursor-down"
                    | "select-up"
                    | "select-down"
                    | "page-up"
                    | "page-down"
                    | "cursor-first"
                    | "cursor-last"
                    | "mark"
                    | "select-all"
                    | "clear-selection"
                    | "select-all-numberpad"
                    | "invert"
                    | "invert-numberpad"
                    | "select-extension"
                    | "unselect-extension"
                    | "restore-selection"
                    | "sort-name"
                    | "sort-extension"
                    | "sort-size"
                    | "sort-time"
                    | "sort-attributes"
                    | "details"
                    | "short-view"
                    | "long-view"
                    | "clear-filter"
                    | "quick-view"
                    | "search"
                    | "sync"
                    | "open"
                    | "refresh"
                    | "new-tab"
                    | "close-tab"
                    | "next-tab"
                    | "previous-tab"
                    | "tab-menu"
                    | "tab-list"
                    | "tab-reopen"
                    | "tab-groups"
                    | "tab-import"
                    | "tab-export"
                    | "tab-lock"
                    | "tab-lock-base"
                    | "tab-base"
                    | "up"
                    | "root"
                    | "back"
                    | "forward"
                    | "swap"
                    | "match"
                    | "stacked"
                    | "theme"
                    | "hidden"
                    | "settings"
                    | "close-settings"
                    | "close-drives"
                    | "view"
                    | "viewer-back"
                    | "viewer-next"
                    | "viewer-toggle"
                    | "properties"
                    | "copy"
                    | "rename"
                    | "delete"
                    | "delete-permanent"
                    | "zip-extract"
                    | "zip-test"
                    | "mask"
                    | "mask-add"
                    | "mask-add-folders"
                    | "mask-remove"
                    | "mask-remove-files"
                    | "favorites"
                    | "palette"
                    | "help"
                    | "cancel-job"
                    | "copy-result"
                    | "copy-names"
                    | "copy-details"
                    | "copy-source-path"
                    | "copy-target-path"
                    | "close-dialog"
                    | "ftp-connect"
                    | "ftp-profiles"
                    | "ftp-permissions"
                    | "ftp-edits"
                    | "plugins"
                    | "wdx-columns"
                    | "wfx-inspect"
                    | "ftp-list"
                    | "ftp-directory"
                    | "ftp-download"
                    | "ftp-upload"
                    | "ftp-sync"
                    | "ftp-close"
                    | "ftp-fxp-source"
                    | "ftp-fxp"
                    | "ftp-fxp-clear"
            )
        {
            self.status("Dieser Befehl benötigt einen lokalen Dateieintrag.");
            return;
        }
        if !self.panels[1 - side].current().is_local()
            && matches!(
                command,
                "move" | "split" | "join" | "zip-pack" | "zip-extract"
            )
        {
            self.status("Wähle einen lokalen Zielordner im anderen Fenster.");
            return;
        }
        match command {
            "switch-panel" => self.set_active(1 - side),
            "mark" => self.current_mut().toggle_current(),
            "select-all" => self.current_mut().adjust_selection(|_| true, true),
            "clear-selection" => self.current_mut().adjust_selection(|_| true, false),
            "select-all-numberpad" => {
                let folders = self.profile.layout.numpad_folders;
                self.current_mut().adjust_selection(|entry| folders || entry.kind != Kind::Directory, true);
            }
            "invert" => self.current_mut().invert(),
            "invert-numberpad" => {
                let folders = self.profile.layout.numpad_folders;
                self.current_mut().invert_where(|entry| folders || entry.kind != Kind::Directory);
            }
            "select-extension" | "unselect-extension" => {
                if let Some(entry) = self.current().display_row(self.current().cursor).filter(|entry| entry.kind != Kind::Directory) {
                    let extension = entry.path.extension().map(|value| value.to_os_string());
                    self.current_mut().adjust_selection(|entry| {
                        entry.kind != Kind::Directory && match (entry.path.extension(), extension.as_deref()) {
                            (Some(left), Some(right)) => left.eq_ignore_ascii_case(right),
                            (None, None) => true,
                            _ => false,
                        }
                    }, command == "select-extension");
                }
            }
            "restore-selection" => self.current_mut().restore_selection(),
            "sort-name" => self.current_mut().set_sort(SortKey::Name, hidden),
            "sort-extension" => self.current_mut().set_sort(SortKey::Extension, hidden),
            "sort-size" => self.current_mut().set_sort(SortKey::Size, hidden),
            "sort-time" => self.current_mut().set_sort(SortKey::Modified, hidden),
            "sort-attributes" => self.current_mut().set_sort(SortKey::Attributes, hidden),
            "sort-wdx-first" => self.sort_wdx(0),
            "sort-wdx-second" => self.sort_wdx(1),
            "sort-wdx-third" => self.sort_wdx(2),
            "details" => self.current_mut().detailed = !self.current().detailed,
            "short-view" => self.current_mut().detailed = false,
            "long-view" => self.current_mut().detailed = true,
            "clear-filter" => self.current_mut().filter(String::new(), hidden),
            "quick-view" => self.toggle_quick_preview(),
            "open" => self.open_row(self.current().cursor),
            "refresh" => self.refresh(side),
            "new-tab" => self.new_tab(),
            "close-tab" => self.request_close_tab(),
            "next-tab" => self.select_tab((self.panels[side].tab + 1) % self.panels[side].tabs.len()),
            "previous-tab" => self.select_tab((self.panels[side].tab + self.panels[side].tabs.len() - 1) % self.panels[side].tabs.len()),
            "tab-menu" => self.show_tab_menu(),
            "tab-list" => self.show_tab_list(),
            "tab-reopen" => self.show_closed_tabs(),
            "tabs-open-folders" => self.open_selected_folders(side),
            "tabs-open-folders-other" => self.open_selected_folders(1 - side),
            "tab-groups" => self.show_tab_groups(),
            "tab-import" => self.tab_import_dialog(),
            "tab-export" => self.tab_export_dialog(None),
            "tab-lock" => self.toggle_tab_lock(profile::TabLock::Fixed),
            "tab-lock-base" => self.toggle_tab_lock(profile::TabLock::ReturnToBase),
            "tab-base" => { self.return_to_tab_base(); }
            "root" if self.return_to_tab_base() => (),
            "root" if ftp => self.go_ftp_root(),
            "root" => self.load(side, self.current().path.ancestors().last().unwrap_or(&self.current().path).to_path_buf(), Navigation::Jump),
            "back" => {
                if let Some(path) = self.current().back.last().cloned() { self.load_location(side, path, Navigation::Back); }
            }
            "forward" => {
                if let Some(path) = self.current().forward.last().cloned() { self.load_location(side, path, Navigation::Forward); }
            }
            "swap" => {
                self.panels.swap(0, 1);
                self.set_active(1 - side);
                self.refresh_both();
            }
            "match" => self.load_location(1 - side, self.current().location(), Navigation::Jump),
            "stacked" => {
                self.profile.stacked = !self.profile.stacked;
                self.sync_ui_settings();
            }
            "theme" => {
                self.profile.light_mode = !self.profile.light_mode;
                self.sync_ui_settings();
            }
            "hidden" => {
                self.set_hidden(!hidden);
                self.sync_ui_settings();
            }
            "settings" => self.open_settings(),
            "close-settings" => self.close_settings(),
            "close-drives" => self.ui().set_drive_side(-1),
            "edit" => self.with_file(|app, path| app.report(arco_windows::edit(&path, app.profile.editor.as_deref()))),
            "view" if ftp => self.view_ftp_panel_entry(),
            "view" if self.current().archive.is_some() => {
                if let Some(entry) = self.current().display_row(self.current().cursor).filter(|entry| entry.kind == Kind::File) {
                    self.archive_preview_dialog(entry.path.clone());
                } else {
                    self.status("Wähle eine Datei im Archiv.");
                }
            }
            "view" => self.with_file(|app, path| {
                if !app.preview_registered(&path) { app.view_file(path); }
            }),
            "view-internal" => self.with_file(|app, path| app.view_file(path)),
            "viewer-back" | "viewer-next" | "viewer-toggle" => self.viewer_action(command),
            "properties" if ftp => self.ftp_panel_properties(),
            "properties" if self.current().archive.is_some() => {
                let tab = self.current();
                let description = tab.display_row(tab.cursor).map(|entry| {
                    format!("{}\n{}", display_path(&entry.path), entry.size.map(human_size).unwrap_or_else(|| "Ordner".into()))
                }).unwrap_or_else(|| tab.location().display());
                self.text("Archiv-Eigenschaften", &description);
            }
            "properties" => {
                let tab = self.current();
                if tab.parent_row(tab.cursor) { self.report(arco_windows::properties(&tab.path)); }
                else if let Some(entry) = tab.current() { self.report(arco_windows::properties(&entry.path)); }
            }
            "explorer" => self.report(arco_windows::open(&self.current().path)),
            "terminal" => self.report(arco_windows::terminal(&self.current().path)),
            "copy-result" => self.report(arco_windows::copy_text(self.ui().get_dialog_value().as_str())),
            "copy-paths" => {
                let paths = self.current().paths().iter().map(|path| display_path(path)).collect::<Vec<_>>().join("\r\n");
                self.report(arco_windows::copy_text(&paths));
            }
            "copy-names" | "copy-details" => self.copy_entry_text(command == "copy-details"),
            "copy-source-path" => self.report(arco_windows::copy_text(&self.current().location().display())),
            "copy-target-path" => self.report(arco_windows::copy_text(&self.panels[1 - side].current().location().display())),
            "clipboard-copy" | "clipboard-cut" => self.report(arco_windows::copy_paths(&self.current().paths(), command == "clipboard-cut")),
            "clipboard-paste" => match arco_windows::paste_paths() {
                Ok((paths, cut)) => self.transfer_dialog(paths, self.current().path.clone(), if cut { Transfer::Move } else { Transfer::Copy }),
                Err(error) => self.status(error.to_string()),
            },
            "copy" if ftp || passive_ftp => self.open_ftp_copy(),
            "copy" if self.panels[1 - side].current().archive.is_some() => self.archive_insert_dialog(),
            "copy" | "zip-extract" if self.current().archive.is_some() => self.archive_extract_dialog(),
            "copy" | "move" => self.transfer_dialog(self.current().paths(), self.target().to_path_buf(), if command == "copy" { Transfer::Copy } else { Transfer::Move }),
            "mkdir" | "delete" | "delete-permanent" | "rename" if ftp => self.ftp_panel_edit_dialog(command),
            "mkdir" | "new-file" => {
                let folder = command == "mkdir";
                self.dialog("input", Pending::Create(self.current().path.clone(), folder), Dialog {
                    title: if folder { "Neuer Ordner" } else { "Neue Datei" }, placeholder: "Name", ..Default::default()
                });
            }
            "delete" | "delete-permanent" | "rename" if self.current().archive.is_some() => self.archive_edit_dialog(command == "rename"),
            "delete" | "delete-permanent" => self.delete_dialog(command == "delete-permanent"),
            "rename" | "multi-rename" => self.rename_dialog(command == "multi-rename"),
            "undo-rename" => {
                if let Some(plan) = self.undo.clone() { self.rename_review(plan); }
                else { self.status("Keine Umbenennung in dieser Sitzung verfügbar."); }
            }
            "search" => {
                let archive = self.current().archive.clone();
                let search_text = archive.as_ref().is_none_or(|archive| is_zip(&archive.file));
                self.dialog("input", Pending::Search(side, self.panels[side].request, self.current().location(), archive), Dialog {
                    title: "Dateien suchen", description: "Durchsucht den aktiven Ordner und seine Unterordner. Mehrere Dateimasken werden mit Semikolon getrennt.",
                    value: "*", placeholder: "Dateimaske", second_placeholder: if search_text { "Text als RegEx (optional)" } else { "" }, ..Default::default()
                });
            }
            "search-wdx" => self.show_wdx_search(),
            "wdx-search-profiles" => self.show_wdx_search_profiles(),
            "branch" | "branch-selected" => {
                let path = self.current().path.clone();
                let request = self.panels[side].request;
                let selected = (command == "branch-selected").then(|| self.current().paths());
                if selected.is_some() && self.current().extra.starts_with(" · Zweigansicht der Auswahl") {
                    self.refresh(side);
                    return;
                }
                if selected.as_ref().is_some_and(Vec::is_empty) {
                    self.status("Wähle Dateien oder Ordner für die Zweigansicht.");
                    return;
                }
                self.start_job("Zweigansicht wird geladen …", false, move |job| {
                    let (entries, label) = match selected {
                        Some(paths) => (files::branch_selected(&path, paths, &job.cancel)?, "Zweigansicht der Auswahl"),
                        None => (files::branch(&path, &job.cancel)?, "Zweigansicht"),
                    };
                    Ok(Outcome::Entries { side, location: Location::Local(path), request, entries, label: label.into() })
                });
            }
            "compare" | "compare-binary" => self.compare(command == "compare-binary"),
            "compare-dirs" | "compare-dirs-folders" | "dir-match" => {
                self.compare_panels(command == "compare-dirs-folders", MarkRule::Newer, command == "dir-match");
            }
            "compare-dirs-unique" | "compare-dirs-folders-unique" => {
                self.compare_panels(command == "compare-dirs-folders-unique", MarkRule::Unique, false);
            }
            "compare-dirs-size" | "compare-dirs-folders-size" => {
                self.compare_panels(command == "compare-dirs-folders-size", MarkRule::Size, false);
            }
            "sync" => self.open_sync(),
            "space-usage" | "folder-sizes" => self.measure_files(command == "folder-sizes"),
            "hash" | "verify" => self.hash_dialog(command == "verify"),
            "duplicates" => {
                let paths = vec![self.current().path.clone()];
                self.start_job("Suche nach Duplikaten …", false, move |job| {
                    let groups = tools::duplicates(&paths, &job.cancel)?;
                    let mut text = String::new();
                    for (index, group) in groups.iter().enumerate() {
                        writeln!(text, "Gruppe {}", index + 1).unwrap();
                        for path in group { writeln!(text, "  {}", display_path(path)).unwrap(); }
                    }
                    Ok(Outcome::Text(format!("{} Duplikatgruppen", groups.len()), text))
                });
            }
            "split" => self.with_file(|app, path| app.dialog("input", Pending::Split(path, app.target().to_path_buf()), Dialog {
                title: "Datei teilen", description: "Die Teile werden mit .001, .002 usw. im anderen Fenster gespeichert.",
                value: "64", placeholder: "Teilgröße in MiB", ..Default::default()
            })),
            "join" => {
                let paths = self.current().paths();
                if paths.is_empty() { self.status("Wähle die zu verbindenden Dateien."); }
                else { self.dialog("input", Pending::Join(paths, self.target().to_path_buf()), Dialog {
                    title: "Dateien verbinden", description: "Verbindet die ausgewählten Dateien in lexikografischer Reihenfolge. Ausgabe im anderen Fenster.",
                    placeholder: "Name der Ausgabedatei", ..Default::default()
                }); }
            }
            "zip-open" => self.with_file(|app, path| {
                if archive_view::is_supported(&path) { app.open_archive(path); }
                else { app.status("Wähle ein unterstütztes Archiv."); }
            }),
            "zip-pack" => self.zip_pack_dialog(),
            "zip-test" if self.current().archive.is_some() => {
                if let Some(archive) = &self.current().archive { self.archive_read_dialog(archive.file.clone(), true); }
            }
            "zip-extract" | "zip-test" => self.with_file(|app, path| app.archive_read_dialog(path, command == "zip-test")),
            "ftp-connect" | "ftp-list" | "ftp-directory" | "ftp-download" | "ftp-upload" | "ftp-sync" | "ftp-close"
                | "ftp-fxp-source" | "ftp-fxp" | "ftp-fxp-clear" => self.ftp_action(command),
            "ftp-profiles" => self.open_ftp_profiles(None),
            "ftp-permissions" => self.ftp_panel_edit_dialog(command),
            "ftp-edits" => self.choose_ftp_draft(),
            "import-tc" => self.dialog("input", Pending::ImportTc, Dialog {
                title: "Total-Commander-Profil importieren",
                description: "Liest lokale Tabs und Verzeichnisfavoriten aus wincmd.ini. Programme und Zugangsdaten werden beim Import übersprungen.",
                placeholder: "Absoluter Pfad zur wincmd.ini", ..Default::default()
            }),
            "plugins" => self.show_plugin_manager(None),
            "wdx-columns" => self.show_wdx_columns(),
            "wcx-inspect" => self.with_file(|app, path| app.plugin_dialog(Interface::Wcx, path)),
            "wdx-inspect" => self.with_file(|app, path| app.plugin_dialog(Interface::Wdx, path)),
            "wfx-inspect" => self.plugin_dialog(Interface::Wfx, PathBuf::new()),
            "wlx-view" => self.with_file(|app, path| app.plugin_dialog(Interface::Wlx, path)),
            "mask" | "mask-add" | "mask-add-folders" | "mask-remove" | "mask-remove-files" => {
                let mode = match command {
                    "mask-add" | "mask-add-folders" => MaskEdit::Add {
                        folders: command == "mask-add-folders" || self.profile.layout.numpad_folders,
                    },
                    "mask-remove" | "mask-remove-files" => MaskEdit::Remove { folders: command == "mask-remove" },
                    _ => MaskEdit::Replace,
                };
                let title = match mode {
                    MaskEdit::Replace => "Auswahl nach Maske",
                    MaskEdit::Add { .. } => "Auswahl nach Maske erweitern",
                    MaskEdit::Remove { .. } => "Auswahl nach Maske verkleinern",
                };
                self.dialog("input", Pending::Mask(mode), Dialog { title, value: "*", placeholder: "Dateimaske", ..Default::default() });
            }
            "add-favorite" => {
                let path = self.current().path.clone();
                if !self.profile.favorites.contains(&path) { self.profile.favorites.push(path); }
                self.status("Ordner als Favorit gespeichert.");
            }
            "favorites" => {
                if self.profile.favorites.is_empty() { self.status("Merke den aktiven Ordner über Werkzeuge → Neuen Favoriten merken."); }
                else {
                    let list = self.profile.favorites.iter().map(|path| display_path(path)).collect::<Vec<_>>();
                    let options = list.iter().map(String::as_str).collect::<Vec<_>>();
                    self.dialog("confirm", Pending::Favorite, Dialog { title: "Favoriten", options: &options, ..Default::default() });
                }
            }
            "configure-editor" => {
                let editor = self.profile.editor.as_deref().map(display_path).unwrap_or_default();
                self.dialog("input", Pending::Editor, Dialog { title: "Editor einstellen", value: &editor,
                    description: "Absoluter Pfad zum Editor. Ein leeres Feld stellt den Windows-Editor wieder her.",
                    placeholder: "Editorprogramm", ..Default::default() });
            }
            "palette" => {
                let options = COMMANDS.iter().filter(|(_, command)| *command != "palette").map(|(label, _)| *label).collect::<Vec<_>>();
                self.dialog("input", Pending::Palette, Dialog { title: "Befehlspalette", placeholder: "Befehlsname oder cm_-Alias (optional)", options: &options, ..Default::default() });
            }
            "help" => self.text("Arco Commander · Tastatur", &self.keyboard_help()),
            "cancel-job" => self.cancel(),
            "close-dialog" => self.close_dialog(),
            _ => (),
        }
        self.publish(self.active);
        if self.ui().get_dialog_kind().is_empty()
            && !self.ui().get_settings_open()
            && self.ui().get_drive_side() < 0
        {
            self.ui().invoke_focus_panels();
        }
    }

    fn navigate_key(&mut self, command: &str) -> bool {
        let mark = matches!(command, "select-up" | "select-down");
        let tab = self.current_mut();
        let old = tab.cursor;
        match command {
            "cursor-up" | "select-up" => tab.cursor_step(-1, mark),
            "cursor-down" | "select-down" => tab.cursor_step(1, mark),
            "page-up" => tab.cursor_step(-18, false),
            "page-down" => tab.cursor_step(18, false),
            "cursor-first" => tab.cursor = 0,
            "cursor-last" => tab.cursor_end(),
            "up" => {
                self.go_up();
                return true;
            }
            _ => return false,
        }
        if mark || tab.cursor != old {
            self.publish_cursor(self.active, mark);
        }
        true
    }

    pub(crate) fn dialog(&mut self, kind: &str, pending: Pending, data: Dialog<'_>) {
        self.pending = Some(pending);
        let ui = self.ui();
        ui.set_dialog_title(data.title.into());
        ui.set_dialog_description(data.description.into());
        ui.set_dialog_value(data.value.into());
        ui.set_dialog_second(data.second.into());
        ui.set_dialog_placeholder(data.placeholder.into());
        ui.set_dialog_second_placeholder(data.second_placeholder.into());
        ui.set_dialog_secret(data.secret);
        ui.set_dialog_second_secret(data.second_secret);
        ui.set_dialog_options(strings(data.options.iter().copied()));
        ui.set_dialog_kind(kind.into());
        ui.set_dialog_revision(ui.get_dialog_revision().wrapping_add(1));
    }

    pub(crate) fn close_dialog(&mut self) {
        if let Some(Pending::SyncReview(review)) = self.pending.take() {
            review.close();
        }
        self.viewer = None;
        self.staged_preview = None;
        let ui = self.ui();
        ui.set_dialog_kind("".into());
        ui.set_dialog_value("".into());
        ui.set_dialog_second("".into());
        ui.set_viewer_image(slint::Image::default());
        ui.set_sync_rows(Default::default());
        ui.invoke_focus_panels();
    }

    pub fn submit(&mut self, value: &str, second: &str, option: i32) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        self.close_dialog();
        if let Err(error) = self.execute(pending, value, second, option.max(0) as usize) {
            self.text("Aktion konnte nicht ausgeführt werden", &error.to_string());
        }
        self.publish(self.active);
    }

    fn execute(
        &mut self,
        pending: Pending,
        value: &str,
        second: &str,
        option: usize,
    ) -> io::Result<()> {
        match pending {
            Pending::WfxPrompt(ticket) => ticket.answer(value),
            Pending::WfxPanelEdit(location, source) => self.edit_wfx_panel(location, source, value),
            Pending::WfxPanelBatch(location, tasks) => self.run_wfx_batch(location, tasks),
            Pending::WfxContext => {
                if let Some(command) = [
                    "open",
                    "view",
                    "copy",
                    "move",
                    "rename",
                    "mkdir",
                    "delete",
                    "properties",
                ]
                .get(option)
                {
                    self.action(command);
                }
            }
            Pending::WdxChoose(plugin, fields) => {
                if let Some(field) = fields.get(option) {
                    self.add_wdx_column(plugin, field)?;
                }
            }
            Pending::WdxSearchPlugin(target, plugins) => {
                if let Some(entry) = plugins.into_iter().nth(option) {
                    self.read_wdx_fields(entry, Some(target));
                }
            }
            Pending::WdxSearchField(target, plugin, fields) => {
                if let Some(field) = fields.into_iter().nth(option) {
                    self.show_wdx_criteria(target, plugin, field);
                }
            }
            Pending::WdxSearchCriteria(target, plugin, field) => {
                self.add_wdx_condition(target, *plugin, field, value, second);
            }
            Pending::WdxSearchReview(target) => {
                self.handle_wdx_search_review(target, option)?;
            }
            Pending::WdxSearchSave(target) => self.save_wdx_search(target, value, false)?,
            Pending::WdxSearchReplace(target, name) => self.save_wdx_search(target, &name, true)?,
            Pending::WdxSearchProfiles(names) => {
                if let Some(name) = names.into_iter().nth(option) {
                    self.wdx_search_profile_actions(name);
                }
            }
            Pending::WdxSearchProfileAction(name) => match option {
                0 => self.load_wdx_search(&name)?,
                1 => self.confirm_wdx_search_profile_delete(name),
                _ => (),
            },
            Pending::WdxSearchProfileDelete(name) => self.delete_wdx_search_profile(&name)?,
            Pending::WdxRemove(columns) => {
                if let Some(column) = columns.get(option) {
                    self.remove_wdx_column(column)?;
                }
            }
            Pending::Create(base, folder) => {
                let name = tools::output_name(value)?;
                let path = base.join(name);
                let editor = self.profile.editor.clone();
                self.start_job("Erstelle Eintrag …", true, move |_| {
                    if folder {
                        fs::create_dir(&path)?;
                    } else {
                        OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(&path)?;
                        arco_windows::edit(&path, editor.as_deref())?;
                    }
                    Ok(Outcome::Notice(format!(
                        "Erstellt: {}",
                        display_path(&path)
                    )))
                });
            }
            Pending::Rename(paths, multi) => {
                let plan = if multi {
                    let pattern = if second.is_empty() {
                        None
                    } else {
                        let (find, replacement) = second
                            .split_once("=>")
                            .ok_or_else(|| io::Error::other("Ersetzung benötigt Muster=>Ersatz"))?;
                        Some((Regex::new(find).map_err(io::Error::other)?, replacement))
                    };
                    if !rename::wdx_fields(value)?.is_empty() {
                        self.plan_wdx_rename(
                            paths,
                            value,
                            pattern.map(|(pattern, replace)| (pattern, replace.to_owned())),
                        )?;
                        return Ok(());
                    }
                    rename::Plan::new(
                        &paths,
                        value,
                        pattern
                            .as_ref()
                            .map(|(pattern, replace)| (pattern, *replace)),
                    )?
                } else {
                    rename::Plan::single(&paths[0], tools::output_name(value)?)?
                };
                self.rename_review(plan);
            }
            Pending::ApplyRename(plan) => {
                self.start_job("Benenne Dateien um …", true, move |_| {
                    plan.apply(arco_windows::rename_noreplace)?;
                    Ok(Outcome::Renamed(plan))
                })
            }
            Pending::Transfer(paths, target, operation) => {
                let policy = match option {
                    1 => Conflict::Skip,
                    2 => Conflict::Replace,
                    _ => Conflict::Fail,
                };
                self.current_mut().remember_selection();
                let label = if operation == Transfer::Copy {
                    "Kopieren"
                } else {
                    "Verschieben"
                };
                self.start_job(label, true, move |job| {
                    let mut last = Instant::now() - Duration::from_secs(1);
                    let report = files::transfer(
                        &paths,
                        &target,
                        operation,
                        policy,
                        &job.cancel,
                        Some(arco_windows::rename_noreplace),
                        |progress| {
                            if last.elapsed() >= Duration::from_millis(100) {
                                last = Instant::now();
                                job.progress(format!(
                                    "{label} · {} · {} / {}",
                                    display_path(progress.path),
                                    human_size(progress.copied),
                                    human_size(progress.total)
                                ));
                            }
                        },
                    )?;
                    Ok(Outcome::Report(label.into(), report))
                });
            }
            Pending::Delete(paths, permanent) => {
                self.current_mut().remember_selection();
                self.start_job("Löschen …", true, move |job| {
                    let mut report = TransferReport::default();
                    for path in paths {
                        if job.cancel.load(Ordering::Relaxed) {
                            report.cancelled = true;
                            break;
                        }
                        let result = if permanent {
                            delete_path(&path)
                        } else {
                            arco_windows::recycle(&path)
                        };
                        match result {
                            Ok(()) => report.completed += 1,
                            Err(error) => report
                                .errors
                                .push(format!("{}: {error}", display_path(&path))),
                        }
                    }
                    Ok(Outcome::Report("Löschen".into(), report))
                });
            }
            Pending::Search(side, request, location, archive) => {
                let query = tools::Search {
                    names: files::compile_masks(value)?,
                    text: (!second.is_empty())
                        .then(|| Regex::new(second))
                        .transpose()
                        .map_err(io::Error::other)?,
                    min_size: None,
                    max_size: None,
                    max_depth: None,
                    show_hidden: self.profile.show_hidden,
                };
                if let Some(archive) = archive {
                    if query.text.is_some() && archive.catalog.contains_encrypted_entries() {
                        self.dialog("input", Pending::ArchiveSearchPassword(side, request, archive, query), Dialog {
                            title: "Textsuche im Archiv", placeholder: "ZIP-Passwort (optional)",
                            description: "Ohne Passwort werden verschlüsselte Einträge übersprungen.",
                            secret: true, ..Default::default()
                        });
                    } else {
                        self.search_archive(side, request, archive, query, None);
                    }
                } else if let Location::Local(path) = location {
                    self.start_job("Suche läuft …", false, move |job| {
                        let entries =
                            tools::search(std::slice::from_ref(&path), &query, &job.cancel)?;
                        Ok(Outcome::Entries {
                            side,
                            location: Location::Local(path),
                            request,
                            entries,
                            label: "Suchtreffer".into(),
                        })
                    });
                }
            }
            Pending::ArchiveSearchPassword(side, request, archive, query) => {
                self.search_archive(
                    side,
                    request,
                    archive,
                    query,
                    (!value.is_empty()).then(|| value.to_owned()),
                );
            }
            Pending::SyncSetup(_) | Pending::SyncReview(_) => (),
            Pending::Hash(paths, base, verify) => {
                let method = HASHES
                    .get(option)
                    .and_then(|name| Hash::parse(name))
                    .ok_or_else(|| io::Error::other("unknown hash algorithm"))?;
                let output = if verify || value.is_empty() {
                    None
                } else {
                    Some(base.join(tools::output_name(value)?))
                };
                self.start_job("Berechne Prüfsummen …", output.is_some(), move |job| {
                    let body = if verify {
                        tools::verify_checksums(&paths[0], method, &job.cancel)?
                    } else {
                        tools::checksum_list(&paths, &base, method, &job.cancel)?
                    };
                    if let Some(path) = output {
                        use std::io::Write;
                        OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(&path)?
                            .write_all(body.as_bytes())?;
                    }
                    Ok(Outcome::Text("Prüfsummen".into(), body))
                });
            }
            Pending::Split(path, target) => {
                let bytes = value
                    .trim()
                    .parse::<u64>()
                    .ok()
                    .and_then(|mib| mib.checked_mul(1024 * 1024))
                    .filter(|value| *value > 0)
                    .ok_or_else(|| io::Error::other("Teilgröße muss eine positive Zahl sein"))?;
                self.start_job("Teile Datei …", true, move |job| {
                    let parts = tools::split(&path, &target, bytes, &job.cancel)?;
                    Ok(Outcome::Notice(format!(
                        "{} Teile geschrieben.",
                        parts.len()
                    )))
                });
            }
            Pending::Join(mut parts, target) => {
                let target = target.join(tools::output_name(value)?);
                parts.sort();
                self.start_job("Verbinde Dateien …", true, move |job| {
                    tools::join(&parts, &target, &job.cancel)?;
                    Ok(Outcome::Notice(format!(
                        "Erstellt: {}",
                        display_path(&target)
                    )))
                });
            }
            Pending::ZipPack(paths, base, destination) => {
                let (extension, format) = match option {
                    1 => (".7z", Some(ExternalFormat::SevenZip)),
                    2 => (".tar", Some(ExternalFormat::Tar)),
                    3 => (".gz", Some(ExternalFormat::Gzip)),
                    _ => (".zip", None),
                };
                if value.trim().is_empty() {
                    return Err(io::Error::other("Ein Archivname ist erforderlich."));
                }
                if format.is_some() && !second.is_empty() {
                    return Err(io::Error::other(
                        "Die Passworteingabe ist für ZIP vorgesehen.",
                    ));
                }
                let name = if value.to_ascii_lowercase().ends_with(extension) {
                    value.to_string()
                } else {
                    format!("{value}{extension}")
                };
                let destination = destination.join(tools::output_name(&name)?);
                let password = (!second.is_empty()).then(|| second.to_string());
                self.start_job("Erstelle Archiv …", true, move |job| {
                    if let Some(format) = format {
                        ExternalArchiver::discover(&job.cancel)?.pack(
                            &paths,
                            &base,
                            &destination,
                            format,
                            &job.cancel,
                        )?;
                    } else {
                        archives::create_zip(
                            &paths,
                            &base,
                            &destination,
                            password.as_deref(),
                            &job.cancel,
                        )?;
                    }
                    Ok(Outcome::Notice(format!(
                        "Erstellt: {}",
                        display_path(&destination)
                    )))
                });
            }
            Pending::ArchiveExtract(archive, target, selection) => {
                self.extract_archive_selection(
                    archive,
                    target,
                    selection,
                    (!value.is_empty()).then(|| value.to_owned()),
                );
            }
            Pending::ArchiveInsert(input, target) => {
                self.insert_into_zip(
                    input,
                    target,
                    (!value.is_empty()).then(|| value.to_owned()),
                    (!second.is_empty()).then(|| second.to_owned()),
                );
            }
            Pending::ArchivePreview(archive, member) => {
                self.preview_archive_member(
                    archive,
                    member,
                    (!value.is_empty()).then(|| value.to_owned()),
                )?;
            }
            Pending::ArchiveRemove(archive, paths) => {
                self.edit_zip(archive, archives::ZipEdit::Remove(paths))
            }
            Pending::ArchiveRename(archive, path) => self.edit_zip(
                archive,
                archives::ZipEdit::Rename(path, tools::output_name(value)?.to_os_string()),
            ),
            Pending::ArchiveContext => {
                if let Some(command) =
                    ["open", "copy", "zip-test", "properties", "rename", "delete"].get(option)
                {
                    self.action(command);
                }
            }
            Pending::ZipRead(path, target, test) => {
                let password = (!value.is_empty()).then(|| value.to_string());
                self.start_job(
                    if test {
                        "Prüfe Archiv …"
                    } else {
                        "Entpacke Archiv …"
                    },
                    !test,
                    move |job| {
                        let count = if !is_zip(&path) {
                            let engine = ExternalArchiver::discover(&job.cancel)?;
                            if test {
                                engine.test(&path, &job.cancel)?
                            } else {
                                engine.extract(&path, &target, &job.cancel)?
                            }
                        } else if test {
                            archives::test_zip(&path, password.as_deref(), &job.cancel)?
                        } else {
                            archives::extract_zip(&path, &target, password.as_deref(), &job.cancel)?
                        };
                        Ok(Outcome::Notice(format!(
                            "{count} Einträge {}.",
                            if test { "geprüft" } else { "entpackt" }
                        )))
                    },
                );
            }
            Pending::FtpConnect(side) => {
                let security = match option {
                    1 => ftp::Security::ImplicitTls,
                    2 => ftp::Security::Plain,
                    _ => ftp::Security::ExplicitTls,
                };
                let connection = ftp::Connection::new(value.trim(), second, security)?;
                self.dialog(
                    "input",
                    Pending::FtpPassword(side, connection),
                    Dialog {
                        title: "FTP-Anmeldung",
                        description: if security == ftp::Security::Plain {
                            "Klartext-FTP überträgt Anmeldung und Daten unverschlüsselt."
                        } else {
                            "FTPS prüft die Serveridentität über den Windows-Zertifikatsspeicher."
                        },
                        placeholder: "Passwort",
                        second_placeholder: "HTTP-/SOCKS-Proxy (optional)",
                        secret: true,
                        ..Default::default()
                    },
                );
            }
            Pending::FtpPassword(side, connection) => {
                self.mount_ftp(side, connection.with_proxy(second)?, value, None)?;
            }
            Pending::FtpProfilePassword(side, connection, local) => {
                self.mount_ftp(side, connection, value, local)?;
            }
            Pending::FtpProfileDelete(name) => self.delete_ftp_profile(&name)?,
            Pending::FtpProfileImport => self.import_ftp_profiles(value.trim())?,
            Pending::ApplyFtpImport(profiles) => self.apply_ftp_import(profiles)?,
            Pending::FtpDirectory((session, _), side) => {
                let directory = session.directory(value.trim())?;
                if let Some(side) = side {
                    self.load_location(
                        side,
                        Location::Ftp(FtpLocation { session, directory }),
                        Navigation::Jump,
                    );
                } else {
                    self.load_ftp(session, directory);
                }
            }
            Pending::FtpDownload((session, directory), target) => {
                let target = target.join(tools::output_name(if second.is_empty() {
                    value
                } else {
                    second
                })?);
                let name = value.to_string();
                self.start_job("FTP-Download …", true, move |job| {
                    if option == 1 {
                        session.download_resuming(&directory, &name, &target, &job.cancel)?;
                    } else {
                        session.download(&directory, &name, &target, &job.cancel)?;
                    }
                    Ok(Outcome::Notice(format!(
                        "Heruntergeladen: {}",
                        display_path(&target)
                    )))
                });
            }
            Pending::FtpUpload((session, directory), source) => {
                let name = value.to_string();
                let in_panel = self.panels.iter().any(|panel| {
                    panel.current().ftp.as_ref().is_some_and(|ftp| {
                        Arc::ptr_eq(&ftp.location.session, &session)
                            && ftp.location.directory == directory
                    })
                });
                self.start_job("FTP-Upload …", in_panel, move |job| {
                    session.upload(&directory, &source, &name, option == 1, &job.cancel)?;
                    if in_panel {
                        return Ok(Outcome::Notice(format!(
                            "FTP-Upload abgeschlossen · {name}"
                        )));
                    }
                    let listing = session.list(&directory, &job.cancel)?;
                    Ok(Outcome::Ftp(FtpView {
                        session,
                        directory,
                        listing,
                    }))
                });
            }
            Pending::FtpPanelEdit(location, edit) => {
                self.execute_ftp_edit(location, edit, value)?
            }
            Pending::FtpEditChoose(paths) => {
                if let Some(path) = paths.get(option) {
                    self.review_ftp_draft(path);
                }
            }
            Pending::FtpEditAction(path) => self.ftp_draft_action(&path, option)?,
            Pending::FtpEditDelete(path) => self.delete_ftp_draft(&path)?,
            Pending::TabMenu => self.tab_menu_action(option),
            Pending::TabRename => self.rename_tab(value)?,
            Pending::TabList => self.select_tab(option),
            Pending::TabClose => self.close_tab(),
            Pending::ReopenTab => self.reopen_tab(option),
            Pending::TabGroups(names) => self.choose_tab_group(&names, option),
            Pending::TabGroupSave => self.save_tab_group(value)?,
            Pending::TabGroupAction(name) => self.tab_group_action(&name, option)?,
            Pending::TabGroupReplace(name) => self.load_tab_group(&name, true)?,
            Pending::TabGroupDelete(name) => self.delete_tab_group(&name)?,
            Pending::TabImport => self.read_tab_import(value, second)?,
            Pending::TabExport(group) => self.write_tab_export(group.as_deref(), value, option)?,
            Pending::ApplyTabImport(name, import) => self.save_imported_tabs(&name, *import)?,
            Pending::FtpPanelContext => {
                if let Some(command) = [
                    "open",
                    "view",
                    "copy",
                    "rename",
                    "mkdir",
                    "delete",
                    "properties",
                    "ftp-permissions",
                    "edit",
                ]
                .get(option)
                {
                    self.action(command);
                }
            }
            Pending::FtpFxp((source, source_dir), (target, target_dir)) => {
                let source_name = value.to_owned();
                let target_name = if second.is_empty() { value } else { second }.to_owned();
                self.start_job("FTP-Server übertragen direkt …", false, move |job| {
                    let size = source.fxp_to(
                        (&source_dir, &source_name),
                        &target,
                        (&target_dir, &target_name),
                        &job.cancel,
                    )?;
                    Ok(Outcome::Notice(format!(
                        "FXP abgeschlossen · {}",
                        human_size(size)
                    )))
                });
            }
            Pending::Mask(mode) => {
                let masks = files::compile_masks(value)?;
                let tab = self.current_mut();
                match mode {
                    MaskEdit::Replace => {
                        tab.remember_selection();
                        tab.replace_selection(|entry| masks.is_match(&entry.name));
                    }
                    MaskEdit::Add { folders } | MaskEdit::Remove { folders } => {
                        tab.adjust_selection(
                            |entry| {
                                (folders || entry.kind != Kind::Directory)
                                    && masks.is_match(&entry.name)
                            },
                            matches!(mode, MaskEdit::Add { .. }),
                        );
                    }
                }
            }
            Pending::Favorite => {
                if let Some(path) = self.profile.favorites.get(option).cloned() {
                    self.load(self.active, path, Navigation::Jump);
                }
            }
            Pending::Editor => {
                if value.trim().is_empty() {
                    self.profile.editor = None;
                } else {
                    let path = PathBuf::from(value.trim().trim_matches('"'));
                    if !path.is_absolute() || !path.is_file() {
                        return Err(io::Error::other(
                            "Editor benötigt einen absoluten Programmpfad",
                        ));
                    }
                    self.profile.editor = Some(path);
                }
                self.status("Editor-Einstellung gespeichert.");
            }
            Pending::Palette => {
                let value = value.trim();
                let command = if value.is_empty() {
                    COMMANDS
                        .iter()
                        .filter(|(_, command)| *command != "palette")
                        .nth(option)
                        .map(|(_, command)| *command)
                } else if let Some(command) = compatibility_command(value) {
                    Some(command)
                } else {
                    let value = value.to_lowercase();
                    let mut matches = COMMANDS.iter().filter(|(label, command)| {
                        label.to_lowercase().contains(&value) || command.contains(&value)
                    });
                    let first = matches.next();
                    if matches.next().is_some() {
                        return Err(io::Error::other(
                            "Mehrere Befehle passen. Gib einen genaueren Namen ein.",
                        ));
                    }
                    first.map(|(_, command)| *command)
                };
                if let Some(command) = command {
                    self.action(command);
                } else {
                    return Err(io::Error::other("Befehl wurde nicht gefunden"));
                }
            }
            Pending::ImportTc => {
                let path = PathBuf::from(value.trim().trim_matches('"'));
                if !path.is_absolute() {
                    return Err(io::Error::other(
                        "Die INI-Datei benötigt einen absoluten Pfad",
                    ));
                }
                self.start_job("Lese Total-Commander-Profil …", false, move |_| {
                    Ok(Outcome::Imported(profile::read_tc_ini(&path)?))
                });
            }
            Pending::ApplyTc(import, bindings) => {
                for (panel, paths) in self.panels.iter_mut().zip(import.panels) {
                    for path in paths {
                        if !panel.tabs.iter().any(|tab| tab.path == path) {
                            panel.tabs.push(Tab::new(path));
                        }
                    }
                }
                for path in import.favorites {
                    if !self.profile.favorites.contains(&path) {
                        self.profile.favorites.push(path);
                    }
                }
                let count = bindings.len();
                for (key, command) in bindings {
                    shortcuts::change(&mut self.profile.shortcuts, key, Some(command));
                }
                self.reload_shortcuts();
                self.publish(0);
                self.publish(1);
                self.save();
                self.status(format!(
                    "Lokale Tabs und Favoriten ergänzt, {count} Tastenkürzel importiert."
                ));
            }
            Pending::InspectPlugin(interface, file) => {
                let plugin = PathBuf::from(value.trim().trim_matches('"'));
                if !plugin.is_absolute() {
                    return Err(io::Error::other("Der Plugin-Pfad muss absolut sein."));
                }
                let file = if matches!(interface, Interface::Wfx) {
                    PathBuf::from(if second.trim().is_empty() {
                        "\\"
                    } else {
                        second.trim()
                    })
                } else {
                    file
                };
                let name = plugin
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                self.run_plugin(interface, plugin, file, name);
            }
            Pending::RegisteredPlugin(interface, file, choices) => {
                self.choose_registered_plugin(interface, file, &choices, option)
            }
            Pending::RegisteredWfx(index) => self.run_registered_wfx(index, value),
            Pending::PluginAdd => self.register_plugin(value, second)?,
            Pending::PluginEdit(index) => self.edit_plugin(index, value, second)?,
            Pending::PluginRemove(index) => self.remove_plugin(index)?,
            Pending::PluginFiles(index) => self.choose_plugin_file_action(index, option)?,
            Pending::PluginFileInput(index, action, archive) => {
                self.prepare_plugin_file_action(index, action, archive, value, second)?
            }
            Pending::ApplyPluginFile(index, archive, operation) => {
                self.run_plugin_file_action(index, archive, *operation)?
            }
            Pending::PluginPackSelection(index, paths) => {
                self.pack_plugin_selection(index, paths, value)?
            }
            Pending::ApplyPluginBatch(index, archive, tasks) => {
                self.start_registered_batch(index, archive, tasks)?
            }
            Pending::PluginPackage => self.read_plugin_package(value)?,
            Pending::ApplyPluginPackage(package) => self.install_plugin_package(*package)?,
            Pending::PluginImport => self.read_plugin_import(value, second)?,
            Pending::ApplyPluginImport(import) => self.apply_plugin_import(import)?,
        }
        Ok(())
    }

    pub(crate) fn report(&self, result: io::Result<()>) {
        match result {
            Ok(()) => self.status("Aktion ausgeführt."),
            Err(error) => self.status(error.to_string()),
        }
    }

    fn copy_entry_text(&mut self, details: bool) {
        use slint::Model;
        let tab = self.current();
        let selected: std::collections::BTreeSet<_> = tab.selected_paths().into_iter().collect();
        if selected.is_empty() {
            self.status("Wähle einen Eintrag zum Kopieren.");
            return;
        }
        let model = tab.file_model();
        let (entries, lines): (Vec<_>, Vec<_>) = (0..model.row_count())
            .filter_map(|index| {
                let entry = tab.display_row(index)?;
                if !selected.contains(&entry.path) {
                    return None;
                }
                let row = model.row_data(index)?;
                if details && tab.detailed {
                    Some((
                        entry.clone(),
                        [row.name, row.extension, row.info, row.time, row.attributes]
                            .iter()
                            .map(|value| value.as_str())
                            .collect::<Vec<_>>()
                            .join("\t"),
                    ))
                } else {
                    Some((entry.clone(), row.full_name.to_string()))
                }
            })
            .unzip();
        if details && tab.detailed && tab.is_local() {
            self.copy_details_with_wdx(entries, lines);
            return;
        }
        self.report(arco_windows::copy_text(&lines.join("\r\n")));
    }

    fn with_file(&mut self, action: impl FnOnce(&mut Self, PathBuf)) {
        if let Some(path) = self
            .current()
            .current()
            .filter(|entry| entry.kind == Kind::File)
            .map(|entry| entry.path.clone())
        {
            action(self, path);
        } else {
            self.status("Wähle eine Datei.");
        }
    }

    pub(crate) fn transfer_dialog(
        &mut self,
        paths: Vec<PathBuf>,
        target: PathBuf,
        operation: Transfer,
    ) {
        if paths.is_empty() {
            self.status("Wähle Dateien oder Ordner.");
            return;
        }
        let description = format!("{} Einträge → {}", paths.len(), display_path(&target));
        self.dialog(
            "confirm",
            Pending::Transfer(paths, target, operation),
            Dialog {
                title: if operation == Transfer::Copy {
                    "Kopieren"
                } else {
                    "Verschieben"
                },
                description: &description,
                options: &[
                    "Bei Namenskonflikten Fehler melden",
                    "Vorhandene Dateien überspringen",
                    "Vorhandene Dateien überschreiben",
                ],
                ..Default::default()
            },
        );
    }

    fn delete_dialog(&mut self, permanent: bool) {
        let paths = self.current().paths();
        if paths.is_empty() {
            self.status("Wähle Dateien oder Ordner.");
            return;
        }
        let description = format!(
            "{} Einträge {}?",
            paths.len(),
            if permanent {
                "unwiderruflich löschen"
            } else {
                "in den Windows-Papierkorb verschieben"
            }
        );
        self.dialog(
            "confirm",
            Pending::Delete(paths, permanent),
            Dialog {
                title: if permanent {
                    "Endgültig löschen"
                } else {
                    "Papierkorb"
                },
                description: &description,
                ..Default::default()
            },
        );
    }

    fn rename_dialog(&mut self, multi: bool) {
        let paths = if multi {
            self.current().paths()
        } else {
            self.current()
                .current()
                .map(|entry| entry.path.clone())
                .into_iter()
                .collect()
        };
        if paths.is_empty() {
            self.status("Wähle Dateien oder Ordner.");
            return;
        }
        let old = paths[0]
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let data = if multi {
            Dialog {
                title: "Mehrfach umbenennen",
                description: "[N] Name, [E] Erweiterung, [F] ganzer Name, [C] Zähler, [=Plugin.Feld] WDX-Wert. Optional RegEx-Ersetzung als Muster=>Ersatz.",
                value: "[F]",
                placeholder: "Namensmaske",
                second_placeholder: "RegEx=>Ersetzung (optional)",
                ..Default::default()
            }
        } else {
            Dialog {
                title: "Umbenennen",
                value: &old,
                placeholder: "Neuer Name",
                ..Default::default()
            }
        };
        self.dialog("input", Pending::Rename(paths, multi), data);
    }

    pub(crate) fn rename_review(&mut self, plan: rename::Plan) {
        if plan.entries().is_empty() {
            self.status("Keine Namensänderungen.");
            return;
        }
        let mut body = String::new();
        for (from, to) in plan.entries() {
            writeln!(body, "{}  →  {}", display_path(from), display_path(to)).unwrap();
        }
        self.dialog(
            "review",
            Pending::ApplyRename(plan),
            Dialog {
                title: "Umbenennung prüfen",
                value: &body,
                ..Default::default()
            },
        );
    }

    fn compare(&mut self, binary: bool) {
        let pair = self.panels.each_ref().map(|panel| {
            panel
                .current()
                .current()
                .filter(|entry| entry.kind == Kind::File)
                .map(|entry| entry.path.clone())
        });
        let [Some(left), Some(right)] = pair else {
            self.status("Setze den Cursor in beiden Fenstern auf eine Datei.");
            return;
        };
        self.start_job("Vergleiche Dateiinhalt …", false, move |job| {
            Ok(Outcome::Text(
                "Inhaltsvergleich · links und rechts".into(),
                if binary {
                    tools::compare_binary(&left, &right, &job.cancel)?
                } else {
                    tools::compare_text(&left, &right)?
                },
            ))
        });
    }

    fn hash_dialog(&mut self, verify: bool) {
        let paths = if verify {
            self.current()
                .current()
                .map(|entry| entry.path.clone())
                .into_iter()
                .collect()
        } else {
            self.current().paths()
        };
        if paths.is_empty() {
            self.status("Wähle Dateien.");
            return;
        }
        self.dialog(if verify { "confirm" } else { "input" }, Pending::Hash(paths, self.current().path.clone(), verify), Dialog {
            title: if verify { "Prüfsummendatei prüfen" } else { "Prüfsummen erzeugen" },
            description: if verify { "Wähle den Algorithmus der Datei." } else { "Optionaler Dateiname für die Ausgabe. Ohne Namen erscheint das Ergebnis nur in der Vorschau." },
            placeholder: "Ausgabedatei (optional)", options: HASHES, ..Default::default()
        });
    }

    fn zip_pack_dialog(&mut self) {
        let paths = self.current().paths();
        if paths.is_empty() {
            self.status("Wähle Dateien oder Ordner.");
            return;
        }
        self.dialog("input", Pending::ZipPack(paths, self.current().path.clone(), self.target().to_path_buf()), Dialog {
            title: "Dateien packen", description: "Das Archiv wird im anderen Fenster erstellt. Bei ZIP aktiviert ein Passwort AES-256.",
            value: "archiv", placeholder: "Archivname", second_placeholder: "ZIP-Passwort (optional)", second_secret: true,
            options: &["ZIP", "7z über 7-Zip", "TAR über 7-Zip", "GZIP über 7-Zip"], ..Default::default()
        });
    }

    fn archive_read_dialog(&mut self, path: PathBuf, test: bool) {
        let zip = is_zip(&path);
        if !zip && !external_archives::is_supported(&path) {
            self.status("Wähle ein unterstütztes Archiv.");
            return;
        }
        self.dialog(if zip { "input" } else { "confirm" }, Pending::ZipRead(path, self.target().to_path_buf(), test), Dialog {
            title: if test { "Archiv prüfen" } else { "Archiv entpacken" },
            description: if zip { "Verschlüsselte Einträge benötigen das Archivpasswort. Vorhandene Dateien im Ziel bleiben erhalten." }
                else { "Der Archiv-Adapter verwendet 7-Zip. Vorhandene Dateien im Ziel bleiben erhalten." },
            placeholder: "ZIP-Passwort (optional)", secret: true, ..Default::default()
        });
    }

    fn archive_extract_dialog(&mut self) {
        let tab = self.current();
        let Some(archive) = tab.archive.as_ref() else {
            return;
        };
        let paths = tab.selected_paths();
        let count = paths.len();
        let selection = match Selection::new(archive.folder.clone(), paths) {
            Ok(selection) => selection,
            Err(error) => {
                self.status(error.to_string());
                return;
            }
        };
        let file = archive.file.clone();
        let password = is_zip(&file) && archive.catalog.contains_encrypted_entries();
        let target = self.target().to_path_buf();
        let description = format!(
            "{count} Archiveinträge → {}\nVorhandene Dateien bleiben erhalten.",
            display_path(&target)
        );
        self.dialog(
            if password { "input" } else { "confirm" },
            Pending::ArchiveExtract(archive.clone(), target, selection),
            Dialog {
                title: "Archivauswahl entpacken",
                description: &description,
                placeholder: "ZIP-Passwort (bei verschlüsselten Einträgen)",
                secret: true,
                ..Default::default()
            },
        );
    }

    fn archive_insert_dialog(&mut self) {
        let Some(target) = self.panels[1 - self.active].current().archive.as_ref() else {
            return;
        };
        if !target
            .file
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
        {
            self.status("Als Archivziel für F5 wird ZIP unterstützt.");
            return;
        }
        let paths = self.current().selected_paths();
        if paths.is_empty() {
            self.status("Wähle Dateien oder Ordner.");
            return;
        }
        let count = paths.len();
        let (input, source_secret) = if let Some(source) = self.current().archive.as_ref() {
            let selection = match Selection::new(source.folder.clone(), paths) {
                Ok(selection) => selection,
                Err(error) => {
                    self.status(error.to_string());
                    return;
                }
            };
            (
                ArchiveInput::Archive {
                    archive: source.clone(),
                    selection,
                },
                is_zip(&source.file) && source.catalog.contains_encrypted_entries(),
            )
        } else {
            (
                ArchiveInput::Local {
                    base: self.current().path.clone(),
                    paths,
                },
                false,
            )
        };
        let description = format!(
            "{count} Einträge → {}::{}\nBei Namenskonflikten wird der gesamte Kopiervorgang abgebrochen.",
            display_path(&target.file),
            display_path(&target.folder),
        );
        self.dialog(
            "input",
            Pending::ArchiveInsert(input, target.clone()),
            Dialog {
                title: "In ZIP kopieren",
                description: &description,
                placeholder: if target.catalog.contains_encrypted_entries() {
                    "Passwort für neue ZIP-Einträge (erforderlich)"
                } else {
                    "Passwort für neue ZIP-Einträge (optional)"
                },
                second_placeholder: if source_secret {
                    "Quellarchiv-Passwort"
                } else {
                    ""
                },
                secret: true,
                second_secret: source_secret,
                ..Default::default()
            },
        );
    }

    pub(crate) fn archive_preview_dialog(&mut self, member: PathBuf) {
        let Some(archive) = self.current().archive.as_ref() else {
            return;
        };
        let file = archive.file.clone();
        let encrypted = self
            .current()
            .display_row(self.current().cursor)
            .is_some_and(|entry| entry.path == member && entry.attributes & 0x4000 != 0);
        if encrypted && is_zip(&file) {
            self.dialog(
                "input",
                Pending::ArchivePreview(archive.clone(), member),
                Dialog {
                    title: "Verschlüsselte Archivdatei anzeigen",
                    placeholder: "ZIP-Passwort",
                    secret: true,
                    ..Default::default()
                },
            );
        } else if let Err(error) = self.preview_archive_member(archive.clone(), member, None) {
            self.status(error.to_string());
        }
    }

    fn archive_edit_dialog(&mut self, rename: bool) {
        let Some(archive) = self.current().archive.clone() else {
            return;
        };
        if !archive
            .file
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
            || archive.catalog.contains_encrypted_entries()
        {
            self.status("Löschen und Umbenennen sind für unverschlüsselte ZIP-Dateien verfügbar.");
            return;
        }
        if rename {
            let Some(entry) = self.current().display_row(self.current().cursor) else {
                return;
            };
            let name = entry.name.to_string_lossy().into_owned();
            self.dialog(
                "input",
                Pending::ArchiveRename(archive, entry.path.clone()),
                Dialog {
                    title: "ZIP-Eintrag umbenennen",
                    value: &name,
                    placeholder: "Neuer Name",
                    ..Default::default()
                },
            );
        } else {
            let paths = self.current().selected_paths();
            if paths.is_empty() {
                return;
            }
            let description = format!(
                "{} markierte Einträge einschließlich ihrer Unterordner dauerhaft aus dem ZIP entfernen?",
                paths.len()
            );
            self.dialog(
                "confirm",
                Pending::ArchiveRemove(archive, paths),
                Dialog {
                    title: "ZIP-Einträge löschen",
                    description: &description,
                    ..Default::default()
                },
            );
        }
    }

    pub(crate) fn archive_context_dialog(&mut self) {
        self.dialog(
            "confirm",
            Pending::ArchiveContext,
            Dialog {
                title: "Archiveintrag",
                options: &[
                    "Öffnen oder anzeigen",
                    "Auswahl entpacken",
                    "Archiv prüfen",
                    "Eigenschaften",
                    "Umbenennen (ZIP)",
                    "Löschen (ZIP)",
                ],
                ..Default::default()
            },
        );
    }

    pub(crate) fn ftp_panel_context_dialog(&mut self) {
        self.dialog(
            "confirm",
            Pending::FtpPanelContext,
            Dialog {
                title: "FTP-Eintrag",
                options: &[
                    "Öffnen",
                    "Vorschau",
                    "In anderes Fenster kopieren",
                    "Umbenennen",
                    "Neuer Ordner",
                    "Dauerhaft löschen",
                    "Eigenschaften",
                    "Unix-Rechte ändern",
                    "Datei bearbeiten",
                ],
                ..Default::default()
            },
        );
    }

    fn ftp_panel_edit_dialog(&mut self, command: &str) {
        if self.panels[self.active].loading || self.job.is_some() {
            self.status("Warte, bis das FTP-Verzeichnis und der laufende Auftrag fertig sind.");
            return;
        }
        let tab = self.current();
        let Some(ftp) = &tab.ftp else {
            if command == "ftp-permissions" {
                self.status("Wähle einen Eintrag im FTP-Dateifenster.");
            }
            return;
        };
        if ftp.listing.format != ftp::ListFormat::Mlsd {
            self.status("FTP-Schreiboperationen im Dateifenster benötigen MLSD.");
            return;
        }
        let location = ftp.location.clone();
        match command {
            "ftp-permissions" => {
                let entries: Vec<_> = tab
                    .selected_paths()
                    .iter()
                    .filter_map(|path| ftp.entry(path).cloned())
                    .collect();
                if entries.is_empty() {
                    return;
                }
                let description = format!(
                    "Unix-Rechte für {} FTP-Einträge setzen. Der Server muss SITE CHMOD unterstützen.",
                    entries.len()
                );
                self.dialog(
                    "input",
                    Pending::FtpPanelEdit(location, FtpEdit::Permissions(entries)),
                    Dialog {
                        title: "FTP-Unix-Rechte ändern",
                        description: &description,
                        placeholder: "Oktal, z. B. 644 oder 755",
                        ..Dialog::default()
                    },
                );
            }
            "mkdir" => self.dialog(
                "input",
                Pending::FtpPanelEdit(location, FtpEdit::Create),
                Dialog {
                    title: "Neuer FTP-Ordner",
                    placeholder: "Ordnername",
                    ..Default::default()
                },
            ),
            "rename" => {
                if let Some(entry) = tab
                    .display_row(tab.cursor)
                    .and_then(|entry| ftp.entry(&entry.path))
                    .cloned()
                {
                    let name = entry.name.clone();
                    self.dialog(
                        "input",
                        Pending::FtpPanelEdit(location, FtpEdit::Rename(entry)),
                        Dialog {
                            title: "FTP-Eintrag umbenennen",
                            value: &name,
                            placeholder: "Neuer Name",
                            ..Default::default()
                        },
                    );
                }
            }
            _ => {
                let entries = tab
                    .selected_paths()
                    .iter()
                    .filter_map(|path| ftp.entry(path).cloned())
                    .collect::<Vec<_>>();
                if entries.is_empty() {
                    return;
                }
                let description = format!(
                    "{} Einträge einschließlich ihrer Unterordner dauerhaft aus {} löschen?",
                    entries.len(),
                    location.directory
                );
                self.dialog(
                    "confirm",
                    Pending::FtpPanelEdit(location, FtpEdit::Delete(entries)),
                    Dialog {
                        title: "FTP-Auswahl löschen",
                        description: &description,
                        ..Default::default()
                    },
                );
            }
        }
    }

    fn ftp_action(&mut self, command: &str) {
        if command == "ftp-fxp-clear" {
            self.fxp_source = None;
            self.status("FXP-Quelle entfernt.");
            return;
        }
        if command == "ftp-connect" {
            self.dialog(
                "input",
                Pending::FtpConnect(self.active),
                Dialog {
                    title: "FTP verbinden",
                    placeholder: "FTP-Adresse oder Servername:Port",
                    second_placeholder: "Benutzername (optional)",
                    options: &[
                        "Explizites FTPS",
                        "Implizites FTPS (ftps://)",
                        "Klartext-FTP",
                    ],
                    ..Default::default()
                },
            );
            return;
        }
        let Some((session, directory)) = self.ftp_endpoint() else {
            self.status("Öffne zuerst eine FTP-Verbindung über Werkzeuge.");
            return;
        };
        match command {
            "ftp-sync" => self.open_ftp_sync(),
            "ftp-list" => self.load_ftp(session, directory),
            "ftp-directory" => {
                let side = if self.ui().get_dialog_kind().as_str() == "ftp-browser" {
                    None
                } else {
                    [self.active, 1 - self.active].into_iter().find(|&side| {
                        self.panels[side].current().ftp.as_ref().is_some_and(|ftp| {
                            Arc::ptr_eq(&ftp.location.session, &session)
                                && ftp.location.directory == directory
                        })
                    })
                };
                self.dialog("input", Pending::FtpDirectory((session, directory), side), Dialog {
                    title: "FTP-Ordner wechseln", description: "Pfad ab dem Login-Verzeichnis des Servers. / öffnet das Login-Verzeichnis.",
                    placeholder: "Entfernter Ordnerpfad", ..Default::default()
                });
            }
            "ftp-download" => self.ftp_download_dialog(""),
            "ftp-upload" => {
                let local = if self.current().ftp.is_some() {
                    self.panels[1 - self.active].current()
                } else {
                    self.current()
                };
                let Some(source) = local
                    .current()
                    .filter(|entry| entry.kind == Kind::File)
                    .map(|entry| entry.path.clone())
                else {
                    self.status("Wähle eine lokale Datei für den FTP-Upload.");
                    return;
                };
                let name = source
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                self.dialog("input", Pending::FtpUpload((session, directory), source), Dialog {
                    title: "FTP hochladen", value: &name, placeholder: "Entfernter Dateiname",
                    description: "Die Datei wird direkt an den Server übertragen. Nach einem Abbruch kann dort eine Teildatei liegen.",
                    options: &["Vorhandene Namen im Verzeichnis erkennen", "Überschreiben erlauben"], ..Default::default()
                });
            }
            "ftp-close" if self.job.is_none() => self.disconnect_ftp(),
            "ftp-close" => self.status("Beende zuerst den laufenden Auftrag."),
            "ftp-fxp-source" => {
                self.fxp_source = Some((session, directory));
                self.status("FXP-Quelle gemerkt. Verbinde jetzt den Zielserver.");
            }
            "ftp-fxp" => {
                let Some(source) = self.fxp_source.clone() else {
                    self.status("Merke zuerst einen FTP-Ordner als FXP-Quelle.");
                    return;
                };
                let target = (session, directory);
                let description = format!(
                    "{} → {}. Beide Server müssen direkten Transfer über Klartext-FTP erlauben.",
                    source.1, target.1
                );
                self.dialog(
                    "input",
                    Pending::FtpFxp(source, target),
                    Dialog {
                        title: "Datei direkt per FXP übertragen",
                        description: &description,
                        placeholder: "Dateiname auf dem Quellserver",
                        second_placeholder: "Name auf dem Zielserver (optional)",
                        ..Default::default()
                    },
                );
            }
            _ => (),
        }
    }

    fn ftp_download_dialog(&mut self, name: &str) {
        let Some(endpoint) = self.ftp_endpoint() else {
            return;
        };
        let target = if self.current().ftp.is_some() {
            self.panels[1 - self.active].current()
        } else {
            self.current()
        };
        if !target.is_local() {
            self.status("Wähle einen lokalen Zielordner.");
            return;
        }
        self.dialog(
            "input",
            Pending::FtpDownload(endpoint, target.path.clone()),
            Dialog {
                title: "FTP herunterladen",
                description: "Speichert eine Datei aus dem FTP-Ordner im aktiven lokalen Fenster.",
                value: name,
                placeholder: "Entfernter Dateiname",
                second_placeholder: "Lokaler Name (optional)",
                options: &["Neu herunterladen", "Fortsetzbaren Download verwenden"],
                ..Default::default()
            },
        );
    }

    pub(crate) fn browse_ftp(&mut self, operation: &str, index: i32) {
        if self.job.is_some() || self.ui().get_dialog_kind().as_str() != "ftp-browser" {
            return;
        }
        let Some(view) = &self.ftp else {
            return;
        };
        let entry = usize::try_from(index)
            .ok()
            .and_then(|index| view.listing.entries.get(index));
        match operation {
            "up" => match view.session.parent_directory(&view.directory) {
                Ok(parent) if parent != view.directory => {
                    self.load_ftp(view.session.clone(), parent)
                }
                Ok(_) => (),
                Err(error) => self.status(error.to_string()),
            },
            "open" if entry.is_some_and(|entry| entry.directory == Some(true)) => {
                let name = &entry.expect("selected FTP directory").name;
                match view
                    .session
                    .relative_directory(&view.directory, Path::new(name))
                {
                    Ok(folder) => self.load_ftp(view.session.clone(), folder),
                    Err(error) => self.status(error.to_string()),
                }
            }
            "open" | "download" => {
                if let Some(entry) = entry.filter(|entry| entry.directory != Some(true)) {
                    let name = entry.name.clone();
                    self.ftp_download_dialog(&name);
                }
            }
            "refresh" => self.load_ftp(view.session.clone(), view.directory.clone()),
            "path" => self.ftp_action("ftp-directory"),
            "sync" => self.open_ftp_sync(),
            _ => (),
        }
    }

    pub fn show_tc_import(&mut self, import: TcImport) {
        let mut body = String::new();
        for (label, items) in [
            ("Links", &import.panels[0]),
            ("Rechts", &import.panels[1]),
            ("Favoriten", &import.favorites),
        ] {
            writeln!(body, "{label}").unwrap();
            for path in items {
                writeln!(body, "  {}", display_path(path)).unwrap();
            }
        }
        writeln!(body, "\nTastenkürzel").unwrap();
        let mut seen = std::collections::BTreeSet::new();
        let mut bindings = Vec::new();
        let mut skipped = import.skipped;
        let current = shortcuts::effective(&self.profile.shortcuts);
        for (key, command) in &import.shortcuts {
            let Some((key, command, label)) = shortcuts::tc_chord(key)
                .zip(tc_shortcut_command(command))
                .and_then(|(key, command)| {
                    shortcuts::label(command).map(|label| (key, command, label))
                })
            else {
                skipped += 1;
                continue;
            };
            if self.profile.shortcuts.contains_key(&key) || !seen.insert(key.clone()) {
                skipped += 1;
                continue;
            }
            if current.get(&key) == Some(&command) {
                continue;
            }
            write!(body, "  {} → {label}", shortcuts::display(&key)).unwrap();
            if let Some(previous) = current.get(&key).and_then(|id| shortcuts::label(id)) {
                write!(body, " (ersetzt Standard: {previous})").unwrap();
            }
            body.push('\n');
            bindings.push((key, command));
        }
        let description = format!(
            "{} Tastenkürzel zur Übernahme, {skipped} Einträge übersprungen. Eigene Arco-Belegungen bleiben erhalten.",
            bindings.len(),
        );
        self.dialog(
            "review",
            Pending::ApplyTc(import, bindings),
            Dialog {
                title: "Total-Commander-Import prüfen",
                description: &description,
                value: &body,
                ..Default::default()
            },
        );
    }

    fn plugin_dialog(&mut self, interface: Interface, file: PathBuf) {
        if self.offer_registered_plugins(interface, &file) {
            return;
        }
        self.manual_plugin_dialog(interface, file);
    }

    pub(crate) fn manual_plugin_dialog(&mut self, interface: Interface, file: PathBuf) {
        let (title, placeholder) = match interface {
            Interface::Wcx => ("WCX-Archiv auflisten", "Pfad zur .wcx oder .wcx64"),
            Interface::Wdx => ("WDX-Metadaten abfragen", "Pfad zur .wdx oder .wdx64"),
            Interface::Wfx => ("WFX-Verzeichnis auflisten", "Pfad zur .wfx oder .wfx64"),
            Interface::Wlx => ("Datei mit WLX anzeigen", "Pfad zur .wlx oder .wlx64"),
        };
        let wfx = matches!(interface, Interface::Wfx);
        self.dialog("input", Pending::InspectPlugin(interface, file), Dialog {
            title, placeholder,
            description: "Wähle ein vertrauenswürdiges Plugin. Es läuft in einem separaten Prozess mit deinen Windows-Rechten.",
            second: if wfx { "\\" } else { "" },
            second_placeholder: if wfx { "Pfad im Plugin-Dateisystem" } else { "" },
            ..Default::default()
        });
    }

    fn load_ftp(&mut self, session: Arc<ftp::Session>, directory: ftp::Url) {
        self.start_job("FTP-Verzeichnis wird geladen …", false, move |job| {
            let listing = session.list(&directory, &job.cancel)?;
            Ok(Outcome::Ftp(FtpView {
                session,
                directory,
                listing,
            }))
        });
    }

    pub fn show_ftp(&mut self, mut view: FtpView) {
        use slint::{ModelRc, VecModel};
        let ui = self.ui();
        let previous = self
            .ftp
            .as_ref()
            .filter(|old| old.directory == view.directory)
            .and_then(|old| {
                usize::try_from(ui.get_ftp_cursor())
                    .ok()
                    .and_then(|index| old.listing.entries.get(index))
            });
        view.listing
            .entries
            .sort_by_cached_key(|entry| (entry.directory != Some(true), entry.name.to_lowercase()));
        let cursor = previous
            .and_then(|entry| {
                view.listing
                    .entries
                    .iter()
                    .position(|next| entry.name == next.name)
            })
            .unwrap_or(0);
        let rows = view
            .listing
            .entries
            .iter()
            .map(|entry| {
                let modified = ftp_modified(entry)
                    .ok()
                    .flatten()
                    .map(|time| {
                        chrono::DateTime::<chrono::Local>::from(time)
                            .format("%d.%m.%y %H:%M")
                            .to_string()
                    })
                    .unwrap_or_default();
                FtpRowData {
                    name: entry.name.as_str().into(),
                    info: if entry.directory == Some(true) {
                        "Ordner".into()
                    } else {
                        entry.size.map(human_size).unwrap_or_default().into()
                    },
                    modified: modified.into(),
                    directory: entry.directory == Some(true),
                }
            })
            .collect::<Vec<_>>();
        ui.set_ftp_rows(ModelRc::new(VecModel::from(rows)));
        ui.set_ftp_cursor(cursor as i32);
        ui.set_ftp_url(view.directory.as_str().into());
        ui.set_ftp_has_parent(view.directory.path() != "/");
        ui.set_ftp_detailed(view.listing.format != ftp::ListFormat::Names);
        self.ftp = Some(view);
        self.pending = None;
        ui.set_dialog_kind("ftp-browser".into());
    }

    fn view_file(&mut self, path: PathBuf) {
        self.viewer = None;
        self.staged_preview = None;
        self.start_job("Lese Vorschau …", false, move |_| {
            match preview::read(&path)? {
                preview::Content::Page(mode, page) => Ok(Outcome::Preview { path, mode, page }),
                preview::Content::Image(image) => Ok(Outcome::Image(path, image)),
            }
        });
    }

    fn read_preview(&mut self, path: PathBuf, offset: u64, mode: PreviewMode) {
        let storage = self
            .staged_preview
            .as_ref()
            .filter(|archive| archive.path == path)
            .map(|archive| archive.storage.clone());
        self.start_job("Lese Vorschau …", false, move |_| {
            let _storage = storage;
            let page = tools::preview(&path, offset, mode)?;
            Ok(Outcome::Preview { path, mode, page })
        });
    }

    fn viewer_action(&mut self, command: &str) {
        if self.job.is_some() {
            return;
        }
        let Some(viewer) = &mut self.viewer else {
            return;
        };
        let (offset, mode) = match command {
            "viewer-back" => (viewer.previous.pop().unwrap_or(0), viewer.mode),
            "viewer-next" => {
                viewer.previous.push(viewer.offset);
                (viewer.next, viewer.mode)
            }
            _ => {
                viewer.previous.clear();
                (
                    0,
                    if viewer.mode == PreviewMode::Text {
                        PreviewMode::Hex
                    } else {
                        PreviewMode::Text
                    },
                )
            }
        };
        let path = viewer.path.clone();
        self.read_preview(path, offset, mode);
    }

    pub fn show_preview(&mut self, path: PathBuf, mode: PreviewMode, page: Preview) {
        let previous = self
            .viewer
            .take()
            .filter(|viewer| viewer.path == path)
            .map(|viewer| viewer.previous)
            .unwrap_or_default();
        let ui = self.ui();
        let title = self
            .staged_preview
            .as_ref()
            .filter(|archive| archive.path == path)
            .map_or_else(|| display_path(&path), |archive| archive.title.clone());
        ui.set_dialog_title(title.into());
        ui.set_dialog_description(
            format!("Bytes {}–{} / {}", page.offset, page.next, page.length).into(),
        );
        ui.set_dialog_value(page.text.into());
        ui.set_dialog_options(strings(std::iter::empty::<&str>()));
        ui.set_viewer_back(!previous.is_empty());
        ui.set_viewer_next(page.next < page.length);
        ui.set_viewer_mode(if mode == PreviewMode::Hex {
            "hex".into()
        } else {
            "text".into()
        });
        ui.set_dialog_kind("viewer".into());
        self.viewer = Some(Viewer {
            path,
            mode,
            offset: page.offset,
            next: page.next,
            previous,
        });
    }
}

fn delete_path(path: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if files::is_link(&meta) {
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if meta.file_attributes() & 0x10 != 0 {
                return fs::remove_dir(path);
            }
        }
        fs::remove_file(path)
    } else if meta.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

fn tc_shortcut_command(name: &str) -> Option<&'static str> {
    match name.to_ascii_lowercase().as_str() {
        "cm_editpath" => Some("focus-path"),
        "cm_focuscmdline" => Some("focus-command-line"),
        "cm_contextmenu" => Some("context-menu"),
        "cm_leftopendrives" => Some("drive-left"),
        "cm_rightopendrives" => Some("drive-right"),
        _ => compatibility_command(name),
    }
}

fn compatibility_command(name: &str) -> Option<&'static str> {
    let name = name.to_ascii_lowercase();
    if let Some((command, parameter)) = name.split_once(char::is_whitespace) {
        return match (command, parameter.trim()) {
            ("cm_comparedirs" | "533", "0") => Some("compare-dirs"),
            ("cm_comparedirs" | "533", "1") => Some("compare-dirs-unique"),
            ("cm_comparedirs" | "533", "2") => Some("compare-dirs-size"),
            ("cm_comparedirswithsubdirs" | "536", "0") => Some("compare-dirs-folders"),
            ("cm_comparedirswithsubdirs" | "536", "1") => Some("compare-dirs-folders-unique"),
            ("cm_comparedirswithsubdirs" | "536", "2") => Some("compare-dirs-folders-size"),
            _ => None,
        };
    }
    match name.as_str() {
        "cm_copy" | "cm_copyotherpanel" => Some("copy"),
        "cm_renmov" => Some("move"),
        "cm_mkdir" => Some("mkdir"),
        "cm_delete" => Some("delete"),
        "cm_list" => Some("view"),
        "cm_edit" => Some("edit"),
        "cm_renameonly" => Some("rename"),
        "cm_switchhidsys" => Some("hidden"),
        "cm_verticalpanels" => Some("stacked"),
        "cm_executedos" => Some("terminal"),
        "cm_copynamestoclip" | "2017" => Some("copy-names"),
        "cm_copyfullnamestoclip" | "2018" => Some("copy-paths"),
        "cm_copysrcpathtoclip" | "2029" => Some("copy-source-path"),
        "cm_copytrgpathtoclip" | "2030" => Some("copy-target-path"),
        "cm_copyfiledetailstoclip" | "2036" => Some("copy-details"),
        "cm_rereadsource" => Some("refresh"),
        "cm_searchfor" => Some("search"),
        "cm_multirenamefiles" => Some("multi-rename"),
        "cm_filesync" => Some("sync"),
        "cm_ftpconnect" | "550" => Some("ftp-profiles"),
        "cm_ftpnew" | "551" => Some("ftp-connect"),
        "cm_ftpdisconnect" | "552" => Some("ftp-close"),
        "cm_comparefilesbycontent" => Some("compare"),
        "cm_comparedirs" | "533" => Some("compare-dirs"),
        "cm_comparedirswithsubdirs" | "536" => Some("compare-dirs-folders"),
        "cm_dirmatch" | "534" => Some("dir-match"),
        "cm_getfilespace" | "503" => Some("space-usage"),
        "cm_countdircontent" | "2014" => Some("folder-sizes"),
        "cm_packfiles" => Some("zip-pack"),
        "cm_unpackfiles" => Some("zip-extract"),
        "cm_crccheck" => Some("verify"),
        "cm_crccreate" => Some("hash"),
        "cm_exchange" => Some("swap"),
        "cm_matchsrc" => Some("match"),
        "cm_opennewtab" | "3001" => Some("new-tab"),
        "cm_switchtonexttab" | "3005" => Some("next-tab"),
        "cm_switchtoprevioustab" | "3006" => Some("previous-tab"),
        "cm_closecurrenttab" | "3007" => Some("close-tab"),
        "cm_dirtabsshowmenu" | "3009" => Some("tab-menu"),
        "cm_togglelockcurrenttab" | "3010" => Some("tab-lock"),
        "cm_gotolockeddir" | "3011" => Some("tab-base"),
        "cm_togglelockdcacurrenttab" | "3012" => Some("tab-lock-base"),
        "cm_dirbranch" => Some("branch"),
        "cm_dirbranchsel" => Some("branch-selected"),
        "cm_directoryhotlist" => Some("favorites"),
        "cm_gotoparent" => Some("up"),
        "cm_gotoroot" => Some("root"),
        "cm_srcquickview" => Some("quick-view"),
        "cm_config" | "cm_layoutconfig" => Some("settings"),
        "cm_srcshort" => Some("short-view"),
        "cm_srclong" => Some("long-view"),
        _ => None,
    }
}
