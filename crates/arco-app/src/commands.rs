use crate::app::{
    App, ArchiveInput, FtpEdit, FtpEndpoint, FtpView, Navigation, Outcome, PluginFileAction,
    PromptTicket, SyncReview, SyncSetup, Viewer, ViewerRequest, WdxSearchTarget, ftp_modified,
    strings,
};
use crate::panel::{ArchiveTab, FtpLocation, Location, Tab, WfxLocation};
use crate::preview;
use crate::shortcuts;
use crate::{FtpRowData, display_path, human_size};
use arco_core::archive_view::{self, Selection, is_zip};
use arco_core::archives;
use arco_core::external_archives::{ExternalArchiver, Format as ExternalFormat};
use arco_core::files::{self, Conflict, Kind, SortKey, Transfer, TransferReport};
use arco_core::plugin::{self, Interface};
use arco_core::profile::{self, TabView, TcImport};
use arco_core::sync::MarkRule;
use arco_core::tools::{Hash, Preview, PreviewMode};
use arco_core::{rename, tc_split, tools, transport_encoding};
use arco_windows::ftp;
use regex::Regex;
use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use zeroize::Zeroizing;

pub enum Pending {
    Language([profile::Language; 3]),
    ShellTransfer(Vec<PathBuf>, arco_windows::ShellAction, bool),
    ShellName(PathBuf, bool, bool),
    ShellElevation,
    KnownFolder,
    VirtualShellFolder,
    QuickFind,
    TabFileFilter(usize, u64),
    SelectionFile(PathBuf, Option<Vec<PathBuf>>),
    DuplicateSearch(PathBuf),
    DuplicateFields(PathBuf),
    HashClipboard(PathBuf, String, Vec<Hash>),
    AdvancedSearch(crate::app::AdvancedSearchSetup),
    EverythingPath,
    SearchReview(Box<crate::app::AdvancedSearchReview>),
    EditComment(PathBuf, Option<String>),
    EditMetadata(Vec<PathBuf>),
    PrintDirectory(PathBuf, Vec<PathBuf>),
    PrintText(String, String),
    TransferManager,
    TransferQueueChoose(Vec<u64>),
    TransferQueueOrder(u64),
    TransferSettings,
    NtfsOptions,
    TransferConflict(crate::app::ConflictTicket),
    ViewSetup(crate::app::ViewSetup),
    Associations(crate::app::AssociationSetup),
    Administration(crate::app::AdministrationSetup),
    TextCompare(Arc<arco_core::text_compare::Session>, usize),
    BinaryCompare(Arc<tools::BinaryComparison>, Option<(u64, u64)>),
    WfxPrompt(PromptTicket),
    WfxPanelEdit(WfxLocation, Option<String>),
    WfxPanelBatch(WfxLocation, Vec<(String, plugin::Operation)>),
    WfxContext,
    WdxChoose(PathBuf, Vec<plugin::WdxField>),
    WdxEditPlugin(Vec<plugin::Registration>, Vec<PathBuf>),
    WdxEditField(crate::app::WdxEditTarget, Vec<plugin::WdxEditableField>),
    WdxEditMode(crate::app::WdxEditTarget, plugin::WdxEditableField),
    WdxEditValue(crate::app::WdxEditTarget, plugin::WdxEditableField),
    WdxEditApply(
        crate::app::WdxEditTarget,
        plugin::WdxField,
        crate::app::WdxEditInput,
    ),
    IgnoreList,
    IgnoreImport,
    Buttonbar,
    Starter,
    ButtonbarTemplates(Vec<String>),
    ButtonbarTemplate(String),
    ButtonbarTemplateSave,
    ButtonbarList(usize),
    ButtonbarAction(usize, usize),
    ButtonbarEdit(usize, Option<usize>),
    ButtonbarFile(usize, bool),
    ButtonbarImport(usize, Vec<arco_core::profile::ButtonbarButton>),
    ButtonbarMenu(
        usize,
        Vec<(String, Vec<arco_core::profile::ButtonbarButton>)>,
    ),
    ButtonbarProgram {
        path: PathBuf,
        working: Option<PathBuf>,
        window: profile::ProgramWindow,
        list_file: Option<tempfile::TempPath>,
    },
    WdxSearchPlugin(WdxSearchTarget, Vec<plugin::Registration>),
    WdxSearchField(
        WdxSearchTarget,
        Box<plugin::Registration>,
        Vec<plugin::WdxField>,
    ),
    WdxSearchCriteria(WdxSearchTarget, Box<plugin::Registration>, plugin::WdxField),
    WdxSearchReview(WdxSearchTarget),
    WdxSearchSave(WdxSearchTarget),
    WdxSearchReplace(WdxSearchTarget, String),
    WdxSearchProfiles(Vec<String>),
    WdxSearchProfileAction(String),
    WdxSearchProfileDelete(String),
    WdxRemove(Vec<plugin::WdxColumn>),
    Create(PathBuf, bool),
    Rename(Vec<PathBuf>, bool, rename::Times),
    RenameSetup(crate::app::RenameSetup),
    ApplyRename(rename::Plan),
    Transfer(Vec<PathBuf>, TransferDestination, Transfer),
    Delete(Vec<PathBuf>, bool),
    Search(usize, u64, Location, Option<ArchiveTab>),
    ArchiveSearchPassword(usize, u64, ArchiveTab, tools::Search),
    SyncSetup(SyncSetup),
    SyncProfilePicker(Vec<String>),
    SyncReview(Box<SyncReview>),
    Hash(Vec<PathBuf>, PathBuf, bool),
    Split(PathBuf, PathBuf),
    Join(Vec<PathBuf>, PathBuf, Option<tc_split::Manifest>),
    TransportEncoding {
        paths: Vec<PathBuf>,
        directory: PathBuf,
        decode: bool,
    },
    ZipPack(Vec<PathBuf>, PathBuf, PathBuf),
    PackerProfiles(Vec<String>),
    PackerProfileAction(String),
    PackerProfileEdit(Option<String>),
    ExternalPack(Vec<PathBuf>, PathBuf, PathBuf, Vec<String>),
    ArchiveVolumes(
        Vec<PathBuf>,
        PathBuf,
        PathBuf,
        VolumePacking,
        Option<Zeroizing<String>>,
    ),
    ZipRead(PathBuf, PathBuf, bool),
    ArchiveUnlock(usize, Location, Navigation),
    ArchiveExtract(ArchiveTab, PathBuf, Selection),
    ArchiveInsert(ArchiveInput, ArchiveTab),
    ArchivePreview(ArchiveTab, PathBuf, bool),
    ArchiveRemove(ArchiveTab, Vec<PathBuf>),
    ArchiveRename(ArchiveTab, PathBuf),
    ArchiveContext,
    FtpConnect(usize),
    FtpPassword(usize, ftp::Connection),
    FtpProfilePassword(usize, ftp::Connection, Option<PathBuf>, String),
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
    Favorites(crate::app::FavoriteSetup),
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

#[derive(Clone)]
pub enum TransferDestination {
    Folder(PathBuf),
    Marked(Vec<PathBuf>),
}

pub enum MaskEdit {
    Replace,
    Add { folders: bool },
    Remove { folders: bool },
}

pub enum VolumePacking {
    Sas,
    Sea,
    Zip,
    SevenZip,
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

const KNOWN_FOLDERS: &[(&str, arco_windows::KnownFolder)] = &[
    ("Desktop", arco_windows::KnownFolder::Desktop),
    ("Dokumente", arco_windows::KnownFolder::Documents),
    ("Downloads", arco_windows::KnownFolder::Downloads),
    ("Bilder", arco_windows::KnownFolder::Pictures),
    ("Musik", arco_windows::KnownFolder::Music),
    ("Videos", arco_windows::KnownFolder::Videos),
    ("Benutzerordner", arco_windows::KnownFolder::Profile),
];

const VIRTUAL_SHELL_FOLDERS: &[(&str, &str)] = &[
    ("Windows-Netzwerk", arco_windows::SHELL_NETWORK),
    ("Windows-Bibliotheken", arco_windows::SHELL_LIBRARIES),
    ("Installierte Anwendungen", arco_windows::SHELL_APPLICATIONS),
    ("Papierkorb", arco_windows::SHELL_RECYCLE_BIN),
];

fn asks_target_password(archive: &ArchiveTab) -> bool {
    is_zip(&archive.file)
        || (archive.catalog.contains_encrypted_entries() && archive.password().is_none())
}

pub const COMMANDS: &[(&str, &str)] = &[
    ("Suchen", "search"),
    ("Suchen in separatem Fenster", "search-separate"),
    ("Nach Plugin-Feld suchen", "search-wdx"),
    ("WDX-Suchprofile", "wdx-search-profiles"),
    ("WDX-Feld ändern", "wdx-edit"),
    ("Ignorierliste bearbeiten", "ignore-edit"),
    ("Externe Packerprofile", "packer-profiles"),
    ("Mit Packerprofil packen", "external-pack"),
    ("Ignorierliste importieren", "ignore-import"),
    ("Ignorierliste umschalten", "ignore-toggle"),
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
    ("Synchronisierungsprofile", "sync-profiles"),
    ("Größe der Auswahl berechnen", "space-usage"),
    ("Ordnergrößen in der Dateiliste anzeigen", "folder-sizes"),
    ("Favoriten", "favorites"),
    ("Ordnerfavoriten verwalten", "favorites-manage"),
    ("Befehlspalette", "palette"),
    ("Zweigansicht", "branch"),
    ("Zweigansicht der Auswahl", "branch-selected"),
    ("Nur markierte Einträge umschalten", "only-marked"),
    ("Nur Programme anzeigen", "only-programs"),
    ("Alle Dateien anzeigen", "clear-filter"),
    ("Dateitypen im Tab filtern", "tab-file-filter"),
    ("Mehrfach umbenennen", "multi-rename"),
    ("Umbenennvorlagen", "rename-templates"),
    ("Namensliste bearbeiten", "rename-names"),
    (
        "Namensliste im externen Editor bearbeiten",
        "rename-external",
    ),
    ("Umbenennung zurücknehmen", "undo-rename"),
    ("Prüfsummen erzeugen", "hash"),
    ("Prüfsummen prüfen", "verify"),
    ("Duplikate suchen", "duplicates"),
    ("Prüfsumme aus Zwischenablage prüfen", "verify-clipboard"),
    ("Auswahl in Dateiliste speichern", "selection-save"),
    ("Auswahl aus Dateiliste laden", "selection-load"),
    ("Auswahl aus Zwischenablage laden", "selection-paste"),
    ("Schnellsuche", "quick-find"),
    ("Everything konfigurieren", "everything-configure"),
    ("Transfermanager", "transfer-manager"),
    ("NTFS-Kopieroptionen", "ntfs-options"),
    ("Datei teilen", "split"),
    ("Dateien verbinden", "join"),
    ("Dateien kodieren (MIME, UUE, XXE)", "encode-files"),
    (
        "Dateien dekodieren (MIME, UUE, XXE, BinHex)",
        "decode-files",
    ),
    ("Archiv öffnen", "zip-open"),
    ("Dateien packen", "zip-pack"),
    ("Archiv entpacken", "zip-extract"),
    ("Archiv prüfen", "zip-test"),
    ("FTP verbinden", "ftp-connect"),
    ("Windows-Ordner öffnen", "known-folder"),
    ("Netzlaufwerk verbinden", "map-drive"),
    ("Netzlaufwerk trennen", "unmap-drive"),
    ("Lokale Windows-Freigaben", "shares"),
    ("Windows-Freigabe erstellen", "share-create"),
    ("Datenträgerbezeichnung ändern", "volume-label"),
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
    ("Lister in eigenem Fenster", "view-separate"),
    ("Windows-Desktop im Dateifenster", "shell-desktop"),
    ("Dieser PC im Dateifenster", "shell-computer"),
    ("Weitere Windows-Orte …", "shell-virtual-folders"),
    ("Letzten lokalen Ordner öffnen", "shell-local"),
    ("WDX-Metadaten abfragen", "wdx-inspect"),
    ("WDX-Spalten verwalten", "wdx-columns"),
    ("WFX-Verzeichnis auflisten", "wfx-inspect"),
    ("WFX verlassen und lokalen Ordner anzeigen", "wfx-local"),
    ("Datei mit WLX anzeigen", "wlx-view"),
    ("Neuen Favoriten merken", "add-favorite"),
    ("Dateipfade kopieren", "copy-paths"),
    ("Dateinamen als Text kopieren", "copy-names"),
    ("Dateidetails als TSV kopieren", "copy-details"),
    ("In markierte Zielordner kopieren", "copy-to-marked"),
    ("Pfad des aktiven Fensters kopieren", "copy-source-path"),
    ("Pfad des anderen Fensters kopieren", "copy-target-path"),
    ("Auswahl nach Maske", "mask"),
    ("Auswahl umkehren", "invert"),
    ("Auswahl wiederherstellen", "restore-selection"),
    ("Detailansicht umschalten", "details"),
    ("Miniaturansicht", "thumbnail-view"),
    ("Kommentaransicht", "comment-view"),
    ("Dateikommentar bearbeiten", "edit-comment"),
    ("Attribute und Zeitstempel ändern", "edit-metadata"),
    ("Textdatei drucken", "print-file"),
    ("Dateiliste drucken", "print-directory"),
    (
        "Dateiliste mit Unterordnern drucken",
        "print-directory-recursive",
    ),
    ("Ordnerbaum umschalten", "directory-tree"),
    ("Spaltensatz auswählen", "column-sets"),
    ("Ansichtsmodus auswählen", "view-modes"),
    ("Automatische Ansichtsregeln", "auto-view-rules"),
    ("TC-Ansichten importieren", "view-rules-import"),
    ("Datei- und Auswahlfarben", "file-colors"),
    ("Automatische Ansichten umschalten", "auto-view-toggle"),
    ("Mehrfachsortierung", "multi-sort"),
    ("Unsortierte Dateiliste", "sort-unsorted"),
    ("Separaten Ordnerbaum umschalten", "separate-tree-cycle"),
    ("Gemeinsamen Ordnerbaum anzeigen", "separate-tree-one"),
    ("Ordnerbaum je Fenster anzeigen", "separate-tree-two"),
    ("Separate Ordnerbäume ausblenden", "separate-tree-off"),
    ("Eigene Buttonbars", "buttonbars"),
    ("Starter", "starter"),
    ("Starter bearbeiten", "starter-edit"),
    ("Buttonbar- und Startervorlagen", "buttonbar-templates"),
    ("Schnellansicht umschalten", "quick-view"),
    ("Versteckte Dateien umschalten", "hidden"),
    ("Fenster tauschen", "swap"),
    ("Fenster angleichen", "match"),
    ("Anordnung wechseln", "stacked"),
    ("Helles/Dunkles Thema", "theme"),
    ("Windows-Farbschema verwenden", "system-theme"),
    ("Terminal öffnen", "terminal"),
    ("Editor einstellen", "configure-editor"),
    ("Einstellungen", "settings"),
    ("Explorer öffnen", "explorer"),
    ("Eigenschaften", "properties"),
    ("Interne Dateizuordnungen", "associations"),
    ("Interne Dateiaktionen", "internal-verbs"),
    ("Mit Windows öffnen", "system-open"),
    ("Öffnen mit", "open-with"),
    ("Als Administrator starten", "run-admin"),
    ("Dateiauftrag mit Windows-Rechteabfrage", "shell-elevation"),
    ("Als anderer Benutzer starten", "run-other-user"),
    ("Windows-Verknüpfung erstellen", "create-shortcut"),
    ("Hilfe", "help"),
    ("Systeminformationen", "system-information"),
    ("Windows-Version", "windows-version"),
];

impl App {
    pub fn command_line(&mut self, line: &str) -> bool {
        let line = line.trim();
        let command = line.split_once(char::is_whitespace);
        if let Some((verb, name)) = command
            && verb.eq_ignore_ascii_case("MULTIRENAME")
        {
            if let Err(error) = self.open_rename_template(name.trim().trim_matches('"')) {
                self.status(error.to_string());
            }
            return true;
        }
        if let Some((verb, name)) = command
            && (verb.eq_ignore_ascii_case("cm_SrcViewModeList")
                || verb.eq_ignore_ascii_case("view-mode"))
        {
            if let Err(error) = self.apply_named_view(name.trim().trim_matches('"'), true) {
                self.status(error.to_string());
            }
            return true;
        }
        if let Some((verb, parameter)) = command
            && compatibility_command(verb) == Some("ignore-toggle")
        {
            let enabled = match parameter.trim() {
                "0" => !self.profile.ignore_enabled,
                "1" => true,
                "-1" | "2" => false,
                _ => {
                    self.status("cm_SwitchIgnoreList erwartet 0, 1, 2 oder -1.");
                    return true;
                }
            };
            if let Err(error) = self.set_ignore_enabled(enabled) {
                self.text("Ignorierliste", &error.to_string());
            }
            return true;
        }
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
        if let Some(index) = command
            .strip_prefix("sort-wdx:")
            .and_then(|index| index.parse().ok())
        {
            self.sort_wdx(index);
            return;
        }
        if self.view_setup_action(command)
            || self.association_action(command)
            || self.favorite_action(command)
            || self.administration_action(command)
            || self.rename_setup_action(command)
        {
            return;
        }
        if command == "print-file" && self.viewer.is_some() {
            self.print_file();
            return;
        }
        match command {
            "tab-file-filter" => {
                let side = self.active;
                let filter = self.current().options.file_filter.clone();
                self.dialog("input", Pending::TabFileFilter(side, self.panels[side].request), Dialog {
                    title: "Dateitypen im Tab filtern",
                    description: "Die Dateitypen bleiben mit dem Tab gespeichert. Ordner bleiben sichtbar. Mehrere Masken mit Leerzeichen trennen, Ausschlüsse nach | angeben.",
                    value: &filter, placeholder: "*.txt *.md | *.bak", ..Default::default()
                });
                return;
            }
            "search-separate" => {
                self.open_separate_search();
                return;
            }
            "shell-desktop" | "shell-computer" => {
                let address = if command == "shell-desktop" {
                    arco_windows::SHELL_DESKTOP
                } else {
                    arco_windows::SHELL_COMPUTER
                };
                self.open_shell_panel(self.active, address);
                return;
            }
            "shell-virtual-folders" => {
                let options = VIRTUAL_SHELL_FOLDERS
                    .iter()
                    .map(|(label, _)| crate::locale::tr(label))
                    .collect::<Vec<_>>();
                self.dialog("confirm", Pending::VirtualShellFolder, Dialog {
                    title: "Weitere Windows-Orte …",
                    description: "Wähle einen virtuellen Windows-Ordner für das aktive Dateifenster.",
                    options: &options,
                    ..Dialog::default()
                });
                return;
            }
            "shell-local" if self.current().shell.is_some() => {
                self.load(self.active, self.current().path.clone(), Navigation::Jump);
                return;
            }
            "view" | "view-internal" | "view-separate" if self.current().shell.is_some() => {
                if let Some(path) = self.current_shell_file().map(Path::to_path_buf) {
                    if command == "view-separate" {
                        self.open_lister(path);
                    } else if command == "view-internal" || !self.preview_registered(&path) {
                        self.view_file(path);
                    }
                } else {
                    self.status("Wähle eine Shell-Datei mit lokalem Dateipfad.");
                }
                return;
            }
            "view-separate" if self.current().is_local() => {
                self.with_file(|app, path| app.open_lister(path));
                return;
            }
            "viewer-detach" => {
                self.detach_lister();
                return;
            }
            _ => (),
        }
        match command {
            "known-folder" => {
                let options: Vec<_> = KNOWN_FOLDERS.iter().map(|(name, _)| *name).collect();
                self.dialog(
                    "confirm",
                    Pending::KnownFolder,
                    Dialog {
                        title: "Windows-Ordner öffnen",
                        options: &options,
                        ..Default::default()
                    },
                );
                return;
            }
            "map-drive" | "unmap-drive" => {
                if let Err(error) = arco_windows::network_drive_dialog(command == "map-drive") {
                    self.status(error.to_string());
                }
                return;
            }
            "system-information" | "windows-version" => {
                if let Err(error) = arco_windows::system_information(command == "windows-version") {
                    self.status(error.to_string());
                }
                return;
            }
            _ => (),
        }
        if self.tree_key(command) || self.navigate_key(command) {
            return;
        }
        if self.wfx_command(command) {
            return;
        }
        let side = self.active;
        let hidden = self.profile.show_hidden;
        let ftp = self.current().ftp.is_some();
        let passive = self.panels[1 - side].current();
        let passive_ftp = passive.ftp.is_some();
        if ((self.current().deferred.is_some() || passive.deferred.is_some())
            && matches!(command, "copy" | "move" | "sync"))
            || (self.current().deferred.is_some() && command == "search")
        {
            self.status("Öffne den gespeicherten virtuellen Ordner zuerst mit Enter.");
            return;
        }
        if self.shell_command(command) {
            return;
        }
        if command == "shell-elevation" {
            self.open_shell_elevation();
            return;
        }
        if ftp && command == "edit" {
            self.edit_ftp_file();
            return;
        }
        if matches!(command, "copy" | "copy-to-marked" | "zip-extract")
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
                    | "sort-comment"
                    | "sort-unsorted"
                    | "details"
                    | "short-view"
                    | "long-view"
                    | "separate-tree-cycle"
                    | "clear-filter"
                    | "only-marked"
                    | "only-programs"
                    | "ignore-edit"
                    | "packer-profiles"
                    | "ignore-import"
                    | "ignore-toggle"
                    | "quick-view"
                    | "search"
                    | "sync"
                    | "sync-profiles"
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
                    | "system-theme"
                    | "hidden"
                    | "settings"
                    | "transfer-manager"
                    | "ntfs-options"
                    | "quick-find"
                    | "selection-paste"
                    | "buttonbars"
                    | "starter"
                    | "starter-edit"
                    | "buttonbar-templates"
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
                "move"
                    | "split"
                    | "join"
                    | "encode-files"
                    | "decode-files"
                    | "zip-pack"
                    | "zip-extract"
                    | "external-pack"
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
            "sort-comment" => self.current_mut().set_sort(SortKey::Comment, hidden),
            "sort-unsorted" => self.current_mut().set_sort(SortKey::Unsorted, hidden),
            "sort-wdx-first" => self.sort_wdx(0),
            "sort-wdx-second" => self.sort_wdx(1),
            "sort-wdx-third" => self.sort_wdx(2),
            "details" => {
                let view = if self.current().detailed() {
                    TabView::Short
                } else {
                    TabView::Details
                };
                self.current_mut().set_view(view, None, None, hidden);
            }
            "short-view" => self.current_mut().set_view(TabView::Short, None, None, hidden),
            "long-view" => self.current_mut().set_view(TabView::Details, None, None, hidden),
            "thumbnail-view" if self.current().is_local() => {
                self.current_mut().set_view(TabView::Thumbnails, None, None, hidden);
            }
            "thumbnail-view" => self.status("Die Miniaturansicht ist für lokale Ordner verfügbar."),
            "comment-view" => {
                let view = if self.current().shows_comments() { TabView::Details } else { TabView::Comments };
                self.current_mut().set_view(view, None, None, hidden);
                self.refresh(side);
            }
            "edit-comment" => self.open_comment(),
            "edit-metadata" => self.open_metadata_edit(),
            "print-file" => self.print_file(),
            "print-directory" | "print-directory-recursive" => self.print_directory(command == "print-directory-recursive"),
            "directory-tree" => self.toggle_tree(),
            "separate-tree-cycle" => self.set_separate_trees((self.profile.separate_tree_mode.min(2) + 1) % 3),
            "separate-tree-one" => self.set_separate_trees(1),
            "separate-tree-two" => self.set_separate_trees(2),
            "separate-tree-off" => self.set_separate_trees(0),
            "separate-tree-toggle-one" => self.set_separate_trees(if self.profile.separate_tree_mode == 1 { 0 } else { 1 }),
            "separate-tree-toggle-two" => self.set_separate_trees(if self.profile.separate_tree_mode == 2 { 0 } else { 2 }),
            "buttonbars" => self.open_buttonbars(),
            "starter" => self.starter(),
            "starter-edit" => self.buttonbar_list(2),
            "buttonbar-templates" => self.buttonbar_templates(),
            "clear-filter" | "only-programs" => {
                self.current_mut().set_programs_only(command == "only-programs", hidden);
                self.save();
            }
            "only-marked" => {
                if !self.current_mut().toggle_selected_filter(hidden) {
                    self.status("Markiere zuerst die Einträge für die Auswahlansicht.");
                }
            }
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
            "root" if self.current().shell.is_some() => self.open_shell_panel(side, arco_windows::SHELL_DESKTOP),
            "root" => self.load(side, self.current().path.ancestors().last().unwrap_or(&self.current().path).to_path_buf(), Navigation::Jump),
            "back" => {
                if let Some(path) = self.current().back.last().cloned() { self.load_location(side, path, Navigation::Back); }
            }
            "forward" => {
                if let Some(path) = self.current().forward.last().cloned() { self.load_location(side, path, Navigation::Forward); }
            }
            "swap" => {
                self.panels.swap(0, 1);
                self.swap_trees();
                self.set_active(1 - side);
                self.refresh_both();
            }
            "match" => self.load_location(1 - side, self.current().location(), Navigation::Jump),
            "stacked" => {
                self.profile.stacked = !self.profile.stacked;
                self.sync_ui_settings();
            }
            "theme" => {
                self.profile.light_mode = !self.effective_light_theme();
                self.profile.system_theme = false;
                self.sync_ui_settings();
                self.publish(0);
                self.publish(1);
                self.save();
            }
            "system-theme" => {
                self.profile.system_theme = !self.profile.system_theme;
                self.sync_ui_settings();
                self.publish(0);
                self.publish(1);
                self.save();
            }
            "hidden" => {
                self.set_hidden(!hidden);
                self.sync_ui_settings();
            }
            "settings" => self.open_settings(),
            "language" => {
                let mut languages = crate::locale::LANGUAGES;
                languages.sort_by_key(|language| *language != self.profile.language);
                self.dialog("confirm", Pending::Language(languages), Dialog {
                    title: "Oberflächensprache",
                    description: "Wähle die Sprache für Arco Commander.",
                    options: &languages.map(crate::locale::label),
                    ..Dialog::default()
                });
            }
            "close-settings" => self.close_settings(),
            "close-drives" => self.ui().set_drive_side(-1),
            "edit" => self.with_file(|app, path| app.report(arco_windows::edit(&path, app.profile.editor.as_deref()))),
            "view" | "view-separate" if ftp => self.view_ftp_panel_entry(command == "view-separate"),
            "view" | "view-separate" if self.current().archive.is_some() => {
                if let Some(entry) = self.current().display_row(self.current().cursor).filter(|entry| entry.kind == Kind::File) {
                    self.archive_preview_dialog(entry.path.clone(), command == "view-separate");
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
            "copy-to-marked" => self.copy_to_marked_dialog(),
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
            "search" if self.current().is_local() || self.current().archive.is_some() => self.open_advanced_search(),
            "search" => {
                let archive = self.current().archive.clone();
                let search_text = archive.as_ref().is_none_or(|archive| archive_view::supports_text_search(&archive.file));
                self.dialog("input", Pending::Search(side, self.panels[side].request, self.current().location(), archive), Dialog {
                    title: "Dateien suchen", description: "Durchsucht den aktiven Ordner und seine Unterordner. Mehrere Dateimasken werden mit Semikolon getrennt.",
                    value: "*", placeholder: "Dateimaske", second_placeholder: if search_text { "Text als RegEx (optional)" } else { "" }, ..Default::default()
                });
            }
            "search-wdx" => self.show_wdx_search(),
            "wdx-search-profiles" => self.show_wdx_search_profiles(),
            "wdx-edit" => self.start_wdx_edit(),
            "ignore-edit" => self.edit_ignore_list(),
            "ignore-import" => self.import_ignore_list(),
            "ignore-toggle" => {
                if let Err(error) = self.toggle_ignore_list() { self.text("Ignorierliste", &error.to_string()); }
            }
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
                    Ok(Outcome::Entries { side, location: Location::Local(path), request, entries, label: label.into(), scope: arco_core::view_settings::Scope::Branch })
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
            "sync-profiles" => self.open_sync_profiles(),
            "space-usage" | "folder-sizes" => self.measure_files(command == "folder-sizes"),
            "hash" | "verify" => self.hash_dialog(command == "verify"),
            "verify-clipboard" => self.hash_clipboard_dialog(),
            "quick-find" => self.quick_find_dialog(),
            "everything-configure" => self.configure_everything_dialog(),
            "transfer-manager" => self.open_transfer_manager(),
            "ntfs-options" => self.open_ntfs_options(),
            "selection-save" | "selection-load" => self.selection_dialog(command == "selection-save"),
            "selection-paste" => {
                if let Err(error) = arco_windows::paste_text().and_then(|value| self.select_from_text(&value)) {
                    self.status(error.to_string());
                }
            }
            "encode-files" | "decode-files" => self.transport_encoding_dialog(command == "decode-files"),
            "duplicates" => {
                self.dialog("confirm", Pending::DuplicateSearch(self.current().path.clone()), Dialog {
                    title: "Duplikate suchen",
                    description: "Durchsucht den aktiven Ordner einschließlich Unterordnern.",
                    options: &["Identischer Inhalt", "Gleicher Name", "Gleiche Größe", "Name und Größe", "Name und Inhalt", "Gleiche Plugin-Felder …"],
                    ..Default::default()
                });
            }
            "split" => self.with_file(|app, path| app.dialog("input", Pending::Split(path, app.target().to_path_buf()), Dialog {
                title: "Datei teilen", description: "Im Total-Commander-Modus entsteht zusätzlich eine CRC-Begleitdatei mit Originalname und Prüfsumme.",
                value: "64", placeholder: "Teilgröße in MiB", options: &["Total Commander (.crc)", "Nur Dateiteile"], ..Default::default()
            })),
            "join" => {
                let paths = self.current().paths();
                if paths.is_empty() { self.status("Wähle die zu verbindenden Dateien."); }
                else {
                    match tc_split::selected_manifest(&paths) {
                        Err(error) => self.status(error.to_string()),
                        Ok(manifest) => {
                            let name = manifest.as_ref().map(|file| file.name.clone()).unwrap_or_default();
                            let description = if manifest.is_some() {
                                "Verbindet den vollständigen Satz und prüft Größe und CRC32 vor dem Speichern."
                            } else {
                                "Verbindet die ausgewählten Dateien in lexikografischer Reihenfolge. Ausgabe im anderen Fenster."
                            };
                            self.dialog("input", Pending::Join(paths, self.target().to_path_buf(), manifest), Dialog {
                                title: "Dateien verbinden", description, value: &name,
                                placeholder: "Name der Ausgabedatei", ..Default::default()
                            });
                        }
                    }
                }
            }
            "zip-open" => self.with_file(|app, path| {
                if archive_view::is_openable(&path) { app.open_archive(path); }
                else { app.status("Wähle ein unterstütztes Archiv."); }
            }),
            "zip-pack" => self.zip_pack_dialog(),
            "packer-profiles" => self.open_packer_profiles(),
            "external-pack" => self.external_pack_dialog(),
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
        ui.set_dialog_transfer_filters(Default::default());
        ui.set_dialog_title(crate::locale::tr(data.title).into());
        ui.set_dialog_description(crate::locale::tr(data.description).into());
        ui.set_dialog_value(data.value.into());
        ui.set_dialog_second(data.second.into());
        ui.set_dialog_placeholder(crate::locale::tr(data.placeholder).into());
        ui.set_dialog_second_placeholder(crate::locale::tr(data.second_placeholder).into());
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
        self.preview_image_path = None;
        let ui = self.ui();
        ui.set_dialog_kind("".into());
        ui.set_dialog_value("".into());
        ui.set_dialog_second("".into());
        ui.set_viewer_image(slint::Image::default());
        ui.set_file_conflict(Default::default());
        ui.set_diff_data(Default::default());
        ui.set_sync_rows(Default::default());
        ui.invoke_focus_panels();
    }

    pub fn submit(&mut self, value: &str, second: &str, option: i32) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        let filter = self.ui().get_dialog_transfer_filter().max(0) as usize;
        self.close_dialog();
        if let Err(error) = self.execute(pending, value, second, option.max(0) as usize, filter) {
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
        transfer_filter: usize,
    ) -> io::Result<()> {
        match pending {
            Pending::Language(languages) => {
                let Some(language) = languages.get(option).copied() else {
                    return Ok(());
                };
                crate::locale::select(language).map_err(io::Error::other)?;
                self.profile.language = language;
                self.sync_tools_menu();
                self.save();
                self.publish(0);
                self.publish(1);
                self.status("Bereit");
            }
            Pending::TabFileFilter(side, request) => {
                if self.panels[side].request != request {
                    return Ok(());
                }
                let value = value.trim();
                profile::validate_tc_tab_filter(value)?;
                self.panels[side]
                    .current_mut()
                    .set_file_filter(value.into(), self.profile.show_hidden);
                self.save();
                self.publish(side);
            }
            Pending::KnownFolder => {
                if let Some((_, folder)) = KNOWN_FOLDERS.get(option) {
                    self.load(
                        self.active,
                        arco_windows::known_folder(*folder)?,
                        Navigation::Jump,
                    );
                }
            }
            Pending::VirtualShellFolder => {
                if let Some((_, address)) = VIRTUAL_SHELL_FOLDERS.get(option) {
                    self.open_shell_panel(self.active, address);
                }
            }
            Pending::TransferManager => self.transfer_manager_action(option)?,
            Pending::TransferQueueChoose(ids) => {
                if let Some(id) = ids.get(option) {
                    self.open_queued_transfer(*id);
                }
            }
            Pending::TransferQueueOrder(id) => self.reorder_transfer(id, option),
            Pending::TransferSettings => self.save_transfer_settings(value, second)?,
            Pending::NtfsOptions => (),
            Pending::TransferConflict(ticket) => ticket.answer(option, second == "all", value),
            Pending::ViewSetup(setup) => self.submit_view_setup(setup, value, option)?,
            Pending::Associations(setup) => self.submit_association(setup, value, option)?,
            Pending::Administration(setup) => {
                self.submit_administration(setup, value, second, option)?
            }
            Pending::TextCompare(_, _) | Pending::BinaryCompare(_, _) => (),
            Pending::EverythingPath => self.save_everything_path(value)?,
            Pending::PrintText(title, body) => {
                if arco_windows::print_text(&title, &body)? {
                    self.status("Druckauftrag übergeben.");
                }
            }
            Pending::PrintDirectory(base, paths) => {
                let options = arco_core::advanced_search::Options {
                    names: value.to_owned(),
                    depth: second.to_owned(),
                    ..Default::default()
                };
                self.prepare_print_directory(base, paths, Some(options.compile()?));
            }
            Pending::EditComment(path, previous) => {
                self.write_comment(path, previous, value.to_owned())
            }
            Pending::EditMetadata(paths) => self.apply_metadata_edit(paths, value)?,
            Pending::AdvancedSearch(_) => (),
            Pending::SearchReview(review) => self.use_search_results(*review, option),
            Pending::QuickFind => self.quick_find(value, option)?,
            Pending::SelectionFile(root, paths) => self.selection_file(root, paths, value)?,
            Pending::HashClipboard(path, expected, methods) => {
                let method = methods
                    .get(option)
                    .copied()
                    .ok_or_else(|| io::Error::other("Wähle ein Prüfsummenverfahren."))?;
                self.start_job("Prüfe Datei …", false, move |job| {
                    let actual = tools::checksum(&path, method, &job.cancel)?;
                    let title = if expected.eq_ignore_ascii_case(&actual) {
                        "Prüfsumme stimmt"
                    } else {
                        "Prüfsumme weicht ab"
                    };
                    Ok(Outcome::Text(
                        title.into(),
                        format!(
                            "{}\nErwartet  {expected}\nBerechnet {actual}",
                            display_path(&path)
                        ),
                    ))
                });
            }
            Pending::DuplicateSearch(path) => {
                use tools::DuplicateMode::*;
                let Some(&mode) = [Content, Name, Size, NameAndSize, NameAndContent].get(option)
                else {
                    self.duplicate_fields_dialog(path);
                    return Ok(());
                };
                let ignore = self.ignore_list();
                let side = self.active;
                let request = self.panels[side].request;
                self.start_job("Suche nach Duplikaten …", false, move |job| {
                    let groups = tools::duplicates(
                        std::slice::from_ref(&path),
                        mode,
                        ignore.as_deref(),
                        &job.cancel,
                    )?;
                    crate::app::duplicate_result(side, request, path, groups, &job.cancel)
                });
            }
            Pending::DuplicateFields(path) => self.find_duplicate_fields(path, value, option)?,
            Pending::Buttonbar => self.buttonbar_list(option),
            Pending::Starter => self.run_button(2, option),
            Pending::ButtonbarTemplates(names) => {
                self.select_buttonbar_template(
                    option
                        .checked_sub(1)
                        .and_then(|index| names.get(index).cloned()),
                );
            }
            Pending::ButtonbarTemplate(name) => self.apply_buttonbar_template(name, option)?,
            Pending::ButtonbarTemplateSave => self.save_buttonbar_template(value)?,
            Pending::ButtonbarList(bar) => self.buttonbar_select(bar, option),
            Pending::ButtonbarFile(bar, importing) => {
                self.buttonbar_file(bar, importing, &value)?
            }
            Pending::ButtonbarImport(bar, buttons) => {
                self.confirm_buttonbar_import(bar, buttons)?
            }
            Pending::ButtonbarAction(bar, index) => self.buttonbar_action(bar, index, option)?,
            Pending::ButtonbarEdit(bar, index) => self.save_button(bar, index, value, second)?,
            Pending::ButtonbarMenu(bar, levels) => self.select_buttonbar_menu(bar, levels, option),
            Pending::ButtonbarProgram {
                path,
                working,
                window,
                list_file,
            } => Self::start_button_program(
                &path,
                arco_windows::parse_windows_arguments(value)?,
                working,
                window,
                list_file,
            )?,
            Pending::PackerProfiles(names) => self.choose_packer_profile(names, option),
            Pending::PackerProfileAction(name) => {
                if option == 1 {
                    self.remove_packer_profile(&name)?;
                } else {
                    self.edit_packer_profile(Some(name), None, None);
                }
            }
            Pending::PackerProfileEdit(previous) => {
                if let Err(error) = self.save_packer_profile(previous.as_deref(), value) {
                    self.edit_packer_profile(previous, Some(value), Some(&error.to_string()));
                }
            }
            Pending::ExternalPack(paths, base, directory, names) => {
                let name = names
                    .get(option)
                    .ok_or_else(|| io::Error::other("Wähle ein Packerprofil."))?;
                self.run_external_packer(paths, base, directory, name, value)?;
            }
            Pending::IgnoreList => {
                if let Err(error) = self.save_ignore_list(value, self.profile.ignore_enabled) {
                    self.edit_ignore_rules(value, Some(&error.to_string()));
                }
            }
            Pending::IgnoreImport => self.read_ignore_list(value)?,
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
                if let Some(field) = fields.into_iter().nth(option) {
                    self.add_wdx_column(plugin, field)?;
                }
            }
            Pending::WdxEditPlugin(plugins, paths) => {
                if let Some(entry) = plugins.into_iter().nth(option) {
                    self.read_wdx_editable(entry, paths);
                }
            }
            Pending::WdxEditField(target, fields) => {
                if let Some(field) = fields.into_iter().nth(option) {
                    self.wdx_edit_value(target, field);
                }
            }
            Pending::WdxEditValue(target, field) => {
                self.confirm_wdx_edit(target, field, value, option);
            }
            Pending::WdxEditMode(target, field) => {
                if option == 0 {
                    self.open_wdx_editor(target, field);
                } else {
                    self.wdx_edit_text(target, field);
                }
            }
            Pending::WdxEditApply(target, field, value) => {
                self.apply_wdx_edit(target, field, value)
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
            Pending::Rename(paths, multi, times) => {
                if multi {
                    let search = rename::SearchOptions::from_legacy(second)?.compile()?;
                    self.plan_multi_rename(paths, value, None, search, times)?;
                } else {
                    self.rename_review(rename::Plan::single(
                        &paths[0],
                        tools::output_name(value)?,
                    )?);
                }
            }
            Pending::RenameSetup(setup) => self.submit_rename_setup(setup, value, option)?,
            Pending::ApplyRename(plan) => {
                self.start_job("Benenne Dateien um …", true, move |_| {
                    plan.apply(arco_windows::rename_noreplace)?;
                    Ok(Outcome::Renamed(plan))
                })
            }
            Pending::Transfer(paths, target, operation) => {
                let filter = transfer_filter
                    .checked_sub(1)
                    .and_then(|index| {
                        self.profile
                            .search_profiles
                            .values()
                            .filter(|options| {
                                options.everything().is_none()
                                    && options.wdx_profile.trim().is_empty()
                            })
                            .nth(index)
                    })
                    .map(arco_core::advanced_search::Options::compile)
                    .transpose()?;
                let target = match target {
                    TransferDestination::Folder(base) if !value.trim().is_empty() => {
                        TransferDestination::Folder(base.join(value.trim()))
                    }
                    target => target,
                };
                let mask = second.trim().to_owned();
                let conflict = match option {
                    2 => Conflict::Skip,
                    3 => Conflict::Replace,
                    4 => Conflict::Newer,
                    5 => Conflict::Larger,
                    6 => Conflict::Rename,
                    7 if operation == Transfer::Copy => Conflict::Append,
                    _ => Conflict::Fail,
                };
                self.current_mut().remember_selection();
                self.enqueue_transfer(crate::app::TransferTask {
                    paths,
                    destination: target,
                    operation,
                    mask,
                    conflict,
                    ask_conflicts: option == 0,
                    ntfs: self.profile.transfer_ntfs,
                    descriptions: self.profile.transfer_descriptions,
                    filter: filter.map(Arc::new),
                    ignore: self.ignore_list(),
                })?;
            }
            Pending::ShellTransfer(paths, action, elevated) => {
                self.queue_shell_transfer(paths, action, elevated)
            }
            Pending::ShellName(path, folder, elevated) => {
                self.submit_shell_name(path, folder, elevated, value)?
            }
            Pending::ShellElevation => self.choose_shell_elevation(option)?,
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
                    ignore: self.ignore_list(),
                };
                if let Some(archive) = archive {
                    if query.text.is_some()
                        && archive.catalog.contains_encrypted_entries()
                        && archive.password().is_none()
                    {
                        self.dialog("input", Pending::ArchiveSearchPassword(side, request, archive, query), Dialog {
                            title: "Textsuche im Archiv", placeholder: "Archivpasswort (optional)",
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
                            scope: arco_core::view_settings::Scope::Search,
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
            Pending::SyncProfilePicker(profiles) => {
                if let Some(profile) = profiles.get(option) {
                    self.open_sync_profile(profile)?;
                }
            }
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
            Pending::TransportEncoding {
                paths,
                directory,
                decode,
            } => {
                use transport_encoding::Format;
                let title = if decode {
                    "Dateien dekodieren"
                } else {
                    "Dateien kodieren"
                };
                let selected = match (decode, option) {
                    (true, 0) => None,
                    (true, 2) | (false, 1) => Some(Format::Xxe),
                    (true, 3) | (false, 2) => Some(Format::Mime),
                    (true, 4) => Some(Format::BinHex),
                    _ => Some(Format::Uue),
                };
                self.start_job(title, true, move |job| {
                    let mut report = TransferReport::default();
                    let mut mime = Vec::new();
                    let mut armored = Vec::new();
                    for path in paths {
                        if job.cancel.load(Ordering::Relaxed) {
                            report.cancelled = true;
                            break;
                        }
                        job.progress(format!("{title} · {}", display_path(&path)));
                        let format = selected.or_else(|| Format::from_path(&path));
                        if decode && let Some(format @ (Format::Uue | Format::Xxe)) = format {
                            armored.push((path, format));
                            continue;
                        }
                        if decode && matches!(format, Some(Format::Mime)) {
                            mime.push(path);
                            continue;
                        }
                        let result = format
                            .ok_or_else(|| {
                                io::Error::other("Wähle die Kodierung für diese Dateiendung.")
                            })
                            .and_then(|format| {
                                if decode {
                                    transport_encoding::decode(
                                        &path,
                                        &directory,
                                        format,
                                        &job.cancel,
                                    )
                                    .map(|_| ())
                                } else {
                                    transport_encoding::encode(
                                        &path,
                                        &directory,
                                        format,
                                        &job.cancel,
                                    )
                                    .map(|_| ())
                                }
                            });
                        match result {
                            Ok(_) => report.completed += 1,
                            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                                report.cancelled = true;
                                break;
                            }
                            Err(error) => report
                                .errors
                                .push(format!("{}: {error}", display_path(&path))),
                        }
                    }
                    if !report.cancelled && !armored.is_empty() {
                        job.progress(format!("{title} · UUE- und XXE-Teildateien"));
                        for (source, result) in transport_encoding::decode_armored_batch(
                            &armored,
                            &directory,
                            &job.cancel,
                        ) {
                            match result {
                                Ok(()) => report.completed += 1,
                                Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                                    report.cancelled = true;
                                    break;
                                }
                                Err(error) => report
                                    .errors
                                    .push(format!("{}: {error}", display_path(&source))),
                            }
                        }
                    }
                    if !report.cancelled && !mime.is_empty() {
                        job.progress(format!("{title} · MIME-Nachrichten und Nachrichtenteile"));
                        for (source, result) in
                            transport_encoding::decode_mime_batch(&mime, &directory, &job.cancel)
                        {
                            match result {
                                Ok(_) => report.completed += 1,
                                Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                                    report.cancelled = true;
                                    break;
                                }
                                Err(error) => report
                                    .errors
                                    .push(format!("{}: {error}", display_path(&source))),
                            }
                        }
                    }
                    Ok(Outcome::Report(title.into(), report))
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
                    let parts = if option == 0 {
                        tc_split::split(&path, &target, bytes, &job.cancel)?
                    } else {
                        tools::split(&path, &target, bytes, &job.cancel)?.len()
                    };
                    Ok(Outcome::Notice(format!("{parts} Teile geschrieben.")))
                });
            }
            Pending::Join(mut parts, target, manifest) => {
                let target = target.join(tools::output_name(value)?);
                parts.sort();
                self.start_job("Verbinde Dateien …", true, move |job| {
                    if let Some(manifest) = manifest {
                        manifest.join(&target, &job.cancel)?;
                    } else {
                        tools::join(&parts, &target, &job.cancel)?;
                    }
                    Ok(Outcome::Notice(format!(
                        "Erstellt: {}",
                        display_path(&target)
                    )))
                });
            }
            Pending::ZipPack(paths, base, destination) => {
                let volumes = match option {
                    8 => Some((
                        VolumePacking::Sas,
                        "",
                        "Die Dateien erhalten Endungen wie .p01 und .p02.",
                    )),
                    9 => Some((
                        VolumePacking::Zip,
                        ".zip",
                        "Die Dateien erhalten Endungen wie .zip.001 und .zip.002.",
                    )),
                    10 => Some((
                        VolumePacking::SevenZip,
                        ".7z",
                        "Die Dateien erhalten Endungen wie .7z.001 und .7z.002.",
                    )),
                    11 => Some((
                        VolumePacking::Sea,
                        ".exe",
                        "DOS-Satz mit .exe und bei Bedarf .001, .002 usw. Verwende einen Archivnamen mit höchstens acht ASCII-Zeichen.",
                    )),
                    _ => None,
                };
                if let Some((format, extension, description)) = volumes {
                    if matches!(format, VolumePacking::Sas | VolumePacking::Sea)
                        && !second.is_empty()
                    {
                        return Err(io::Error::other(
                            "UC2-Mehrteiler werden ohne Passwort erstellt.",
                        ));
                    }
                    let value = value.trim();
                    tools::output_name(value)?;
                    let name = if value.to_ascii_lowercase().ends_with(extension) {
                        value.to_owned()
                    } else {
                        format!("{value}{extension}")
                    };
                    let destination = destination.join(tools::output_name(&name)?);
                    let password = (!second.is_empty()).then(|| Zeroizing::new(second.to_owned()));
                    let sea = matches!(format, VolumePacking::Sea);
                    self.dialog(
                        "input",
                        Pending::ArchiveVolumes(paths, base, destination, format, password),
                        Dialog {
                            title: "Mehrteiler erstellen",
                            description,
                            value: if sea { "1024" } else { "16" },
                            placeholder: if sea {
                                "Teilgröße in KiB"
                            } else {
                                "Teilgröße in MiB"
                            },
                            ..Dialog::default()
                        },
                    );
                    return Ok(());
                }
                let (extension, format) = match option {
                    1 => (".7z", None),
                    2 => (".tar", Some(ExternalFormat::Tar)),
                    3 => (".gz", Some(ExternalFormat::Gzip)),
                    4 => (".exe", Some(ExternalFormat::SevenZipSfx)),
                    5 => (".uc2", None),
                    6 => (".exe", None),
                    7 | 12 | 13 => (".ace", None),
                    _ => (".zip", None),
                };
                if value.trim().is_empty() {
                    return Err(io::Error::other("Ein Archivname ist erforderlich."));
                }
                if option == 14 && second.is_empty() {
                    return Err(io::Error::other("Gib ein ZipCrypto-Passwort ein."));
                }
                if !matches!(extension, ".zip" | ".7z" | ".exe") && !second.is_empty() {
                    return Err(io::Error::other(
                        "Passwörter sind für ZIP und 7z einschließlich SFX vorgesehen.",
                    ));
                }
                let name = if value.to_ascii_lowercase().ends_with(extension) {
                    value.to_string()
                } else {
                    format!("{value}{extension}")
                };
                let destination = destination.join(tools::output_name(&name)?);
                let password =
                    (!second.is_empty()).then(|| zeroize::Zeroizing::new(second.to_owned()));
                if option == 6 && second.encode_utf16().count() > archives::ZIP_SFX_PASSWORD_LIMIT {
                    return Err(io::Error::other("Das Passwort ist für ZIP-SFX zu lang."));
                }
                self.start_job("Erstelle Archiv …", true, move |job| {
                    let password = password.as_ref().map(|password| password.as_str());
                    if option == 6 {
                        let stub = std::env::current_exe()?.with_file_name("arco-zip-sfx.exe");
                        if !stub.is_file() {
                            return Err(io::Error::new(
                                io::ErrorKind::NotFound,
                                "arco-zip-sfx.exe fehlt neben Arco Commander.",
                            ));
                        }
                        archives::create_zip_sfx(
                            &paths,
                            &base,
                            &destination,
                            &stub,
                            password,
                            &job.cancel,
                        )?;
                    } else if let Some(password) = password.filter(|_| option == 4) {
                        archive_view::pack_encrypted_7z_sfx(
                            &paths,
                            &base,
                            &destination,
                            password,
                            &job.cancel,
                        )?;
                    } else if extension == ".ace" {
                        let pack = match option {
                            12 => arco_core::legacy_archives::pack_ace_compressed,
                            13 => arco_core::legacy_archives::pack_ace_blocked,
                            _ => arco_core::legacy_archives::pack_ace,
                        };
                        pack(&paths, &base, &destination, &job.cancel)?;
                    } else if extension == ".7z" {
                        archive_view::pack_7z(&paths, &base, &destination, password, &job.cancel)?;
                    } else if extension == ".uc2" {
                        arco_core::legacy_archives::pack_uc2(
                            &paths,
                            &base,
                            &destination,
                            &job.cancel,
                        )?;
                    } else if let Some(format) = format {
                        ExternalArchiver::discover(&job.cancel)?.pack(
                            &paths,
                            &base,
                            &destination,
                            format,
                            &job.cancel,
                        )?;
                    } else if option == 14 {
                        let password = password
                            .ok_or_else(|| io::Error::other("ZipCrypto-Passwort fehlt."))?;
                        archives::create_zip_zipcrypto(
                            &paths,
                            &base,
                            &destination,
                            password,
                            &job.cancel,
                        )?;
                    } else {
                        archives::create_zip(&paths, &base, &destination, password, &job.cancel)?;
                    }
                    Ok(Outcome::Notice(format!(
                        "Erstellt: {}",
                        display_path(&destination)
                    )))
                });
            }
            Pending::ArchiveVolumes(paths, base, destination, format, password) => {
                let sea = matches!(format, VolumePacking::Sea);
                let size = value
                    .trim()
                    .parse::<u64>()
                    .ok()
                    .and_then(|size| size.checked_mul(if sea { 1024 } else { 1024 * 1024 }))
                    .filter(|&size| {
                        if sea {
                            (32 * 1024..=512 * 1024 * 1024).contains(&size)
                        } else {
                            size > 0 && size <= u64::from(u32::MAX)
                        }
                    })
                    .ok_or_else(|| {
                        io::Error::other(if sea {
                            "Gib eine Teilgröße von 32 bis 524288 KiB ein."
                        } else {
                            "Gib eine Teilgröße von 1 bis 4095 MiB ein."
                        })
                    })?;
                self.start_job("Erstelle Mehrteiler …", true, move |job| {
                    let count = match format {
                        VolumePacking::Sas => arco_core::legacy_archives::pack_uc2_sas(
                            &paths,
                            &base,
                            &destination,
                            size,
                            &job.cancel,
                        ),
                        VolumePacking::Sea => arco_core::legacy_archives::pack_uc2_sea(
                            &paths,
                            &base,
                            &destination,
                            size,
                            &job.cancel,
                        ),
                        VolumePacking::Zip | VolumePacking::SevenZip => archive_view::pack_split(
                            &paths,
                            &base,
                            &destination,
                            size,
                            password.as_ref().map(|password| password.as_str()),
                            &job.cancel,
                        ),
                    }?;
                    Ok(Outcome::Notice(format!("{count} Archivteile erstellt.")))
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
            Pending::ArchiveUnlock(side, target, navigation) => {
                if value.is_empty() {
                    return Err(io::Error::other("Gib das Archivpasswort ein."));
                }
                let password = Arc::new(zeroize::Zeroizing::new(value.to_owned()));
                self.load_location_with_password(side, target, navigation, Some(password));
            }
            Pending::ArchiveInsert(input, target) => {
                let target_secret = asks_target_password(&target);
                let source = if target_secret { second } else { value };
                self.insert_into_archive(
                    input,
                    target,
                    (target_secret && !value.is_empty()).then(|| value.to_owned()),
                    (!source.is_empty()).then(|| source.to_owned()),
                );
            }
            Pending::ArchivePreview(archive, member, separate) => {
                self.preview_archive_member(
                    archive,
                    member,
                    (!value.is_empty()).then(|| value.to_owned()),
                    separate,
                )?;
            }
            Pending::ArchiveRemove(archive, paths) => {
                self.edit_archive(archive, archives::ZipEdit::Remove(paths), value)
            }
            Pending::ArchiveRename(archive, path) => self.edit_archive(
                archive,
                archives::ZipEdit::Rename(path, tools::output_name(value)?.to_os_string()),
                second,
            ),
            Pending::ArchiveContext => {
                if let Some(command) =
                    ["open", "copy", "zip-test", "properties", "rename", "delete"].get(option)
                {
                    self.action(command);
                }
            }
            Pending::ZipRead(path, target, test) => {
                let password = (!value.is_empty()).then(|| Zeroizing::new(value.to_owned()));
                self.start_job(
                    if test {
                        "Prüfe Archiv …"
                    } else {
                        "Entpacke Archiv …"
                    },
                    !test,
                    move |job| {
                        let password = password.as_ref().map(|password| password.as_str());
                        let count = if test {
                            archive_view::test(&path, password, &job.cancel)?
                        } else {
                            archive_view::extract_all(&path, &target, password, &job.cancel)?
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
                self.mount_ftp(side, connection.with_proxy(second)?, value, None, None)?;
            }
            Pending::FtpProfilePassword(side, connection, local, name) => {
                self.mount_ftp(side, connection, value, local, Some(name))?;
            }
            Pending::FtpProfileDelete(name) => self.delete_ftp_profile(&name)?,
            Pending::FtpProfileImport => self.import_ftp_profiles(value.trim())?,
            Pending::ApplyFtpImport(profiles) => self.apply_ftp_import(profiles)?,
            Pending::FtpDirectory((session, _), side) => {
                let directory = session.directory(value.trim())?;
                if let Some(side) = side {
                    let profile = self.panels[side]
                        .current()
                        .ftp
                        .as_ref()
                        .filter(|ftp| Arc::ptr_eq(&ftp.location.session, &session))
                        .and_then(|ftp| ftp.location.profile.clone());
                    self.load_location(
                        side,
                        Location::Ftp(FtpLocation {
                            session,
                            directory,
                            profile,
                        }),
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
                self.enqueue_virtual_transfer("FTP-Download", true, move |job| {
                    if option == 1 {
                        session.download_resuming(&directory, &name, &target, &job.cancel)?;
                    } else {
                        session.download(&directory, &name, &target, &job.cancel)?;
                    }
                    Ok(Outcome::Report(
                        format!("FTP-Download · {}", display_path(&target)),
                        TransferReport {
                            completed: 1,
                            ..Default::default()
                        },
                    ))
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
                self.enqueue_virtual_transfer("FTP-Upload", in_panel, move |job| {
                    session.upload(&directory, &source, &name, option == 1, &job.cancel)?;
                    if in_panel {
                        return Ok(Outcome::Report(
                            format!("FTP-Upload · {name}"),
                            TransferReport {
                                completed: 1,
                                ..Default::default()
                            },
                        ));
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
                self.enqueue_virtual_transfer("FXP-Direktübertragung", false, move |job| {
                    let size = source.fxp_to(
                        (&source_dir, &source_name),
                        &target,
                        (&target_dir, &target_name),
                        &job.cancel,
                    )?;
                    Ok(Outcome::Report(
                        format!("FXP · {}", human_size(size)),
                        TransferReport {
                            completed: 1,
                            ..Default::default()
                        },
                    ))
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
            Pending::Favorites(setup) => self.submit_favorite(setup, value, option)?,
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
                    Ok(Outcome::Imported(crate::app::tc_profile(&path)?))
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
                self.import_favorites(import.favorites);
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
                if details && tab.detailed() {
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
        if details && tab.detailed() && tab.is_local() {
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
        self.show_transfer(paths, TransferDestination::Folder(target), operation);
    }

    fn copy_to_marked_dialog(&mut self) {
        let passive = self.panels[1 - self.active].current();
        if !self.current().is_local() || !passive.is_local() {
            self.status("Öffne in beiden Dateifenstern lokale Ordner.");
            return;
        }
        let sources = self.current().paths();
        if sources.is_empty() {
            self.status("Wähle links oder rechts die Dateien und Ordner zum Kopieren.");
            return;
        }
        let targets = passive.marked_transfer_targets();
        if targets.is_empty() {
            self.status("Markiere die Zielordner im anderen Dateifenster.");
            return;
        }
        self.show_transfer(
            sources,
            TransferDestination::Marked(targets),
            Transfer::Copy,
        );
    }

    fn transport_encoding_dialog(&mut self, decode: bool) {
        if self.panels.iter().any(|panel| panel.loading) {
            self.status("Warte, bis beide Dateifenster geladen sind.");
            return;
        }
        let paths = self.current().paths();
        if paths.is_empty() {
            self.status("Wähle die Dateien für die Textkodierung.");
            return;
        }
        let directory = self.target().to_path_buf();
        let description = format!(
            "{} Dateien · Zielordner {}",
            paths.len(),
            display_path(&directory)
        );
        self.dialog(
            "confirm",
            Pending::TransportEncoding {
                paths,
                directory,
                decode,
            },
            Dialog {
                title: if decode {
                    "Dateien dekodieren"
                } else {
                    "Dateien kodieren"
                },
                description: &description,
                options: if decode {
                    &[
                        "Automatisch nach Dateiendung",
                        "UUE",
                        "XXE",
                        "MIME",
                        "BinHex",
                    ]
                } else {
                    &["UUE", "XXE", "MIME"]
                },
                ..Default::default()
            },
        );
    }

    fn show_transfer(
        &mut self,
        paths: Vec<PathBuf>,
        target: TransferDestination,
        operation: Transfer,
    ) {
        let (kind, description, destinations, placeholder, second_placeholder) = match &target {
            TransferDestination::Folder(target) => (
                "input",
                format!(
                    "{} Einträge. Relative Zielordner werden vom vorgeschlagenen Ordner aus aufgelöst.",
                    paths.len()
                ),
                display_path(target),
                "Zielordner",
                "Optionale Zielmaske, z. B. *.bak oder *-Kopie.*",
            ),
            TransferDestination::Marked(targets) => (
                "review",
                format!(
                    "{} Einträge in jeden der {} markierten Zielordner kopieren.",
                    paths.len(),
                    targets.len()
                ),
                targets
                    .iter()
                    .map(|target| display_path(target))
                    .collect::<Vec<_>>()
                    .join("\n"),
                "",
                "",
            ),
        };
        let mut conflicts = vec![
            "Bei jeder vorhandenen Datei nachfragen",
            "Bei Namenskonflikten Fehler melden",
            "Vorhandene Dateien überspringen",
            "Vorhandene Dateien überschreiben",
            "Nur mit neueren Dateien überschreiben",
            "Nur mit größeren Dateien überschreiben",
            "Kopie unter freiem Namen speichern",
        ];
        if operation == Transfer::Copy {
            conflicts.push("An vorhandene Dateien anhängen");
        }
        self.dialog(
            kind,
            Pending::Transfer(paths, target, operation),
            Dialog {
                title: if operation == Transfer::Copy {
                    "Kopieren"
                } else {
                    "Verschieben"
                },
                description: &description,
                value: &destinations,
                placeholder,
                second_placeholder,
                options: &conflicts,
                ..Default::default()
            },
        );
        let local_profiles: Vec<_> = self
            .profile
            .search_profiles
            .iter()
            .filter(|(_, options)| {
                options.everything().is_none() && options.wdx_profile.trim().is_empty()
            })
            .map(|(name, _)| name.as_str())
            .collect();
        if !local_profiles.is_empty() {
            let ui = self.ui();
            ui.set_dialog_transfer_filter(0);
            ui.set_dialog_transfer_filters(strings(
                std::iter::once("Alle Dateien").chain(local_profiles),
            ));
        }
    }

    fn delete_dialog(&mut self, permanent: bool) {
        let paths = self.current().paths();
        if paths.is_empty() {
            self.status("Wähle Dateien oder Ordner.");
            return;
        }
        let mut description = format!(
            "{} Einträge {}?",
            paths.len(),
            if permanent {
                "unwiderruflich löschen"
            } else {
                "in den Windows-Papierkorb verschieben"
            }
        );
        if permanent && self.ignore_list().is_some() && paths.iter().any(|path| path.is_dir()) {
            description.push_str("\nDer Vorgang löscht auch ausgeblendete Inhalte dieser Ordner.");
        }
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
        self.rename_dialog_with_template(multi, None);
    }

    pub(crate) fn rename_dialog_with_template(
        &mut self,
        multi: bool,
        template: Option<profile::RenameTemplate>,
    ) {
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
        let separate = multi
            && template
                .as_ref()
                .is_none_or(|item| item.extension.is_some());
        let data = if multi {
            Dialog {
                title: "Mehrfach umbenennen",
                description: if separate {
                    "[N] Name, [E] Erweiterung, [A] ganzer Name, [B] Elternordner, [C10+5:3] Zähler, [YMD] Datum, [hms] Uhrzeit, [U]/[L]/[F]/[n] Schreibweise, [X] Zwischenablage, [=Plugin.Feld] WDX-Wert."
                } else {
                    "Vorlage mit gemeinsamer Maske. [N] Name, [E] Erweiterung, [F] ganzer Name."
                },
                value: template.as_ref().map_or("[N]", |item| item.mask.as_str()),
                placeholder: "Namensmaske",
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
        if multi {
            let search = match template
                .as_ref()
                .map(|template| {
                    template.search.clone().map_or_else(
                        || rename::SearchOptions::from_legacy(&template.replacement),
                        Ok,
                    )
                })
                .transpose()
            {
                Ok(search) => search.unwrap_or_else(|| rename::SearchOptions {
                    wildcards: true,
                    ..Default::default()
                }),
                Err(error) => {
                    self.status(error.to_string());
                    return;
                }
            };
            let ui = self.ui();
            ui.set_rename_separate(separate);
            ui.set_rename_extension(
                template
                    .as_ref()
                    .and_then(|item| item.extension.as_deref())
                    .unwrap_or("[E]")
                    .into(),
            );
            ui.set_rename_search(crate::RenameSearch {
                find: search.find.into(),
                replacement: search.replace.into(),
                regex: search.regex,
                wildcards: search.wildcards,
                match_case: search.match_case,
                first_only: search.first_only,
                include_extension: !separate || search.include_extension,
                substitute: search.substitute,
            });
        }
        self.dialog(
            if multi { "multi-rename" } else { "input" },
            Pending::Rename(paths, multi, rename::Times::now()),
            data,
        );
    }

    pub(crate) fn submit_multi_rename(
        &mut self,
        mask: &str,
        extension: &str,
        search: crate::RenameSearch,
    ) {
        if !matches!(self.pending, Some(Pending::Rename(_, true, _))) {
            return;
        }
        let Some(Pending::Rename(paths, true, times)) = self.pending.take() else {
            return;
        };
        let extension = self.ui().get_rename_separate().then_some(extension);
        let search = rename::SearchOptions {
            find: search.find.to_string(),
            replace: search.replacement.to_string(),
            regex: search.regex,
            wildcards: search.wildcards,
            match_case: search.match_case,
            first_only: search.first_only,
            include_extension: search.include_extension,
            substitute: search.substitute,
        };
        match search.compile().and_then(|search| {
            self.plan_multi_rename(paths.clone(), mask, extension, search, times)
        }) {
            Ok(()) => {
                if self.pending.is_none() {
                    self.close_dialog();
                }
            }
            Err(error) => {
                self.pending = Some(Pending::Rename(paths, true, times));
                self.ui().set_dialog_description(error.to_string().into());
            }
        }
    }

    pub(crate) fn edit_multi_rename(&mut self) {
        if let Some(Pending::Rename(_, true, times)) = &mut self.pending {
            times.edited = chrono::Local::now().naive_local();
        }
    }

    fn plan_multi_rename(
        &mut self,
        paths: Vec<PathBuf>,
        mask: &str,
        extension: Option<&str>,
        search: Option<rename::Search>,
        times: rename::Times,
    ) -> io::Result<()> {
        let mut needs_clipboard = false;
        let mut needs_capture = false;
        let mut fields = Vec::new();
        for expression in std::iter::once(mask).chain(extension) {
            needs_clipboard |= rename::uses_clipboard(expression)?;
            needs_capture |= rename::uses_capture_time(expression)?;
            for field in rename::wdx_fields(expression)? {
                if !fields.contains(&field) {
                    fields.push(field);
                }
            }
        }
        let clipboard = if needs_clipboard {
            match arco_windows::paste_text() {
                Ok(text) => text.lines().next().map(str::to_owned),
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => return Err(error),
            }
        } else {
            None
        };
        if !fields.is_empty() {
            self.plan_wdx_rename(paths, mask, extension, &fields, search, times, clipboard)?;
            return Ok(());
        }
        if needs_capture {
            let mask = mask.to_owned();
            let extension = extension.map(str::to_owned);
            self.start_job(
                "Lese Aufnahmedaten für die Umbenennung …",
                false,
                move |job| {
                    let plan = rename::Plan::new_with_search_at(
                        &paths,
                        &mask,
                        extension.as_deref(),
                        search.as_ref(),
                        rename::PreviewContext {
                            times,
                            cancel: Some(&job.cancel),
                        },
                        clipboard.as_deref(),
                        |_, _| None,
                    )?;
                    files::check_cancel(&job.cancel)?;
                    Ok(Outcome::RenamePreview(plan))
                },
            );
            return Ok(());
        }
        let plan = rename::Plan::new_with_search_at(
            &paths,
            mask,
            extension,
            search.as_ref(),
            rename::PreviewContext {
                times,
                cancel: None,
            },
            clipboard.as_deref(),
            |_, _| None,
        )?;
        self.rename_review(plan);
        Ok(())
    }

    pub(crate) fn rename_review(&mut self, plan: rename::Plan) {
        let description = if plan.skipped_capture() == 0 {
            String::new()
        } else {
            format!(
                "{} ohne lesbares Aufnahmedatum übersprungen.",
                plan.skipped_capture()
            )
        };
        if plan.entries().is_empty() {
            if description.is_empty() {
                self.status("Keine Namensänderungen.");
            } else {
                self.status(format!("Keine Namensänderungen. {description}"));
            }
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
                description: &description,
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
        if !binary {
            self.compare_interactive([left, right]);
            return;
        }
        self.compare_binary_interactive([left, right]);
    }

    fn hash_clipboard_dialog(&mut self) {
        self.with_file(|app, path| {
            let text = match arco_windows::paste_text() {
                Ok(text) => text,
                Err(error) => {
                    app.status(error.to_string());
                    return;
                }
            };
            let expected = text.split_whitespace().next().unwrap_or_default();
            let choices: Vec<_> = HASHES
                .iter()
                .filter_map(|name| {
                    let method = Hash::parse(name)?;
                    method.matches_text(expected).then_some((*name, method))
                })
                .collect();
            if choices.is_empty() {
                app.status("Die Zwischenablage enthält keine unterstützte Hex-Prüfsumme.");
                return;
            }
            let options: Vec<_> = choices.iter().map(|(name, _)| *name).collect();
            app.dialog(
                "confirm",
                Pending::HashClipboard(
                    path.clone(),
                    expected.to_owned(),
                    choices.iter().map(|(_, method)| *method).collect(),
                ),
                Dialog {
                    title: "Prüfsumme aus Zwischenablage prüfen",
                    description: &display_path(&path),
                    options: &options,
                    ..Default::default()
                },
            );
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
            title: "Dateien packen", description: "Das Archiv wird im anderen Fenster erstellt. ZIP und 7z unterstützen AES-256 auch als SFX und Mehrteiler. Bei 7z schützt das Passwort auch die Dateinamen.",
            value: "archiv", placeholder: "Archivname", second_placeholder: "ZIP-/7z-Passwort (optional)", second_secret: true,
            options: &["ZIP", "7z", "TAR über 7-Zip", "GZIP über 7-Zip", "Selbstentpackendes 7z (.exe)", "UC2 über Python", "Selbstentpackendes ZIP (.exe)", "ACE (ohne Kompression)", "UC2-SAS-Mehrteiler", "ZIP-Mehrteiler", "7z-Mehrteiler", "Selbstentpackendes DOS-UC2SEA", "ACE (LZ77)", "ACE 2.0 (Blocked)", "ZIP (ZipCrypto)"], ..Default::default()
        });
    }

    fn archive_read_dialog(&mut self, path: PathBuf, test: bool) {
        let password = archive_view::supports_password(&path);
        let brotli = archive_view::is_brotli(&path);
        let legacy = archive_view::uses_legacy_adapter(&path);
        if !archive_view::is_openable(&path) {
            self.status("Wähle ein unterstütztes Archiv.");
            return;
        }
        self.dialog(if password { "input" } else { "confirm" }, Pending::ZipRead(path, self.target().to_path_buf(), test), Dialog {
            title: if test { "Archiv prüfen" } else { "Archiv entpacken" },
            description: if password { "Verschlüsselte Einträge benötigen das Archivpasswort. Vorhandene Dateien im Ziel bleiben erhalten." }
                else if brotli { "Die Brotli-Datei wird ohne Zusatzprogramm gelesen. Vorhandene Dateien im Ziel bleiben erhalten." }
                else if legacy { "Der Legacy-Adapter verwendet die konfigurierte Python-Installation. Vorhandene Dateien im Ziel bleiben erhalten." }
                else { "Der Archiv-Adapter verwendet 7-Zip. Vorhandene Dateien im Ziel bleiben erhalten." },
            placeholder: "Archivpasswort (optional)", secret: true, ..Default::default()
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
        let password = archive_view::supports_password(&file)
            && archive.catalog.contains_encrypted_entries()
            && archive.password().is_none();
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
                placeholder: "Archivpasswort (bei verschlüsselten Einträgen)",
                secret: true,
                ..Default::default()
            },
        );
    }

    fn archive_insert_dialog(&mut self) {
        let Some(target) = self.panels[1 - self.active].current().archive.as_ref() else {
            return;
        };
        if !archive_view::is_editable(&target.file) {
            self.status("Als Archivziel für F5 sind ZIP, 7z, deren Mehrteiler und unterstützte ACE-Archive verfügbar.");
            return;
        }
        if target.catalog.contains_encrypted_entries()
            && !archive_view::supports_password(&target.file)
        {
            self.status("Das ACE-Ziel muss unverschlüsselt sein.");
            return;
        }
        let target_secret = asks_target_password(target);
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
                archive_view::supports_password(&source.file)
                    && source.catalog.contains_encrypted_entries()
                    && source.password().is_none(),
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
            if target_secret || source_secret {
                "input"
            } else {
                "confirm"
            },
            Pending::ArchiveInsert(input, target.clone()),
            Dialog {
                title: "In Archiv kopieren",
                description: &description,
                placeholder: if !target_secret {
                    "Quellarchiv-Passwort"
                } else if target.catalog.contains_encrypted_entries() {
                    "Zielarchiv-Passwort"
                } else {
                    "Passwort für neue ZIP-Einträge (optional)"
                },
                second_placeholder: if target_secret && source_secret {
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

    pub(crate) fn archive_preview_dialog(&mut self, member: PathBuf, separate: bool) {
        let Some(archive) = self.current().archive.as_ref() else {
            return;
        };
        let file = archive.file.clone();
        let encrypted = self
            .current()
            .display_row(self.current().cursor)
            .is_some_and(|entry| entry.path == member && entry.attributes & 0x4000 != 0);
        if encrypted && archive_view::supports_password(&file) && archive.password().is_none() {
            self.dialog(
                "input",
                Pending::ArchivePreview(archive.clone(), member, separate),
                Dialog {
                    title: "Verschlüsselte Archivdatei anzeigen",
                    placeholder: "Archivpasswort",
                    secret: true,
                    ..Default::default()
                },
            );
        } else if let Err(error) =
            self.preview_archive_member(archive.clone(), member, None, separate)
        {
            self.status(error.to_string());
        }
    }

    fn archive_edit_dialog(&mut self, rename: bool) {
        let Some(archive) = self.current().archive.clone() else {
            return;
        };
        let encrypted = archive.catalog.contains_encrypted_entries();
        if !archive_view::is_editable(&archive.file)
            || (encrypted && !archive_view::supports_encrypted_edit(&archive.file))
        {
            self.status(
                "Löschen und Umbenennen sind für ZIP, 7z, deren Mehrteiler und unterstützte ACE-Archive verfügbar.",
            );
            return;
        }
        let ask_password = encrypted && archive.password().is_none();
        if rename {
            let Some(entry) = self.current().display_row(self.current().cursor) else {
                return;
            };
            let name = entry.name.to_string_lossy().into_owned();
            self.dialog(
                "input",
                Pending::ArchiveRename(archive, entry.path.clone()),
                Dialog {
                    title: "Archiveintrag umbenennen",
                    value: &name,
                    placeholder: "Neuer Name",
                    second_placeholder: if ask_password { "Archivpasswort" } else { "" },
                    second_secret: ask_password,
                    ..Default::default()
                },
            );
        } else {
            let paths = self.current().selected_paths();
            if paths.is_empty() {
                return;
            }
            let description = format!(
                "{} markierte Einträge einschließlich ihrer Unterordner dauerhaft aus dem Archiv entfernen?",
                paths.len()
            );
            self.dialog(
                if ask_password { "input" } else { "confirm" },
                Pending::ArchiveRemove(archive, paths),
                Dialog {
                    title: "Archiveinträge löschen",
                    description: &description,
                    placeholder: "Archivpasswort",
                    secret: ask_password,
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
        for (label, items) in [("Links", &import.panels[0]), ("Rechts", &import.panels[1])] {
            writeln!(body, "{label}").unwrap();
            for path in items {
                writeln!(body, "  {}", display_path(path)).unwrap();
            }
        }
        writeln!(body, "Favoriten").unwrap();
        for entry in &import.favorites {
            let group = entry.groups.join(" › ");
            let label = crate::app::favorite_label(entry);
            let path = display_path(&entry.path);
            writeln!(body, "  {group} · {label} → {path}").unwrap();
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
                preview::Content::Rich(content) => Ok(Outcome::RichPreview(path, content)),
            }
        });
    }

    fn read_viewer_page(&mut self, request: ViewerRequest) {
        let storage = self
            .staged_preview
            .as_ref()
            .filter(|archive| archive.path == request.path())
            .map(|archive| archive.storage.clone());
        self.start_job(request.label(), false, move |job| {
            let _storage = storage;
            Ok(match request.load(&job.cancel)? {
                Some((viewer, page)) => Outcome::ViewerPage(viewer, page),
                None => Outcome::Notice("Kein weiterer Treffer.".into()),
            })
        });
    }

    fn viewer_action(&mut self, command: &str) {
        if self.job.is_none()
            && let Some(request) = self
                .viewer
                .clone()
                .and_then(|viewer| viewer.navigate(command))
        {
            self.read_viewer_page(request);
        }
    }

    pub(crate) fn viewer_control(&mut self, operation: i32, value: &str, option: i32) {
        if self.job.is_none()
            && let Some(viewer) = self.viewer.clone()
        {
            match viewer.control(operation, value, option) {
                Ok(Some(request)) => self.read_viewer_page(request),
                Ok(None) => (),
                Err(error) => self.status(error),
            }
        }
    }

    pub fn show_preview(&mut self, path: PathBuf, mode: PreviewMode, page: Preview) {
        self.show_viewer_page(Viewer::new(path, mode), page);
    }

    pub(crate) fn show_viewer_page(&mut self, mut viewer: Viewer, page: Preview) {
        let ui = self.ui();
        let title = self
            .staged_preview
            .as_ref()
            .filter(|archive| archive.path == viewer.path)
            .map_or_else(
                || display_path(&viewer.path),
                |archive| archive.title.clone(),
            );
        ui.set_dialog_title(title.into());
        ui.set_dialog_description(viewer.description(&page).into());
        viewer.update_page(&page);
        ui.set_dialog_value(page.text.into());
        ui.set_dialog_options(strings(std::iter::empty::<&str>()));
        ui.set_viewer_back(!viewer.previous.is_empty());
        ui.set_viewer_next(viewer.next < viewer.length);
        ui.set_viewer_mode(if viewer.mode == PreviewMode::Hex {
            "hex".into()
        } else {
            "text".into()
        });
        ui.set_viewer_encoding(viewer.encoding_index());
        ui.set_dialog_kind("viewer".into());
        self.viewer = Some(viewer);
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

pub(crate) fn valid_button_command(line: &str) -> bool {
    compatibility_command(line).is_some()
        || COMMANDS.iter().any(|(label, command)| {
            command.eq_ignore_ascii_case(line) || label.eq_ignore_ascii_case(line)
        })
        || line
            .split_once(char::is_whitespace)
            .is_some_and(|(verb, arguments)| {
                !arguments.trim().is_empty()
                    && matches!(
                        verb.to_ascii_lowercase().as_str(),
                        "cd" | "opentabs"
                            | "appendtabs"
                            | "savetabs"
                            | "savetabsl"
                            | "savetabsr"
                            | "savetabs2"
                            | "savetabs2l"
                            | "savetabs2r"
                            | "cm_switchignorelist"
                            | "2922"
                    )
            })
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
        "cm_encode" | "562" => Some("encode-files"),
        "cm_decode" | "563" => Some("decode-files"),
        "cm_switchhidsys" => Some("hidden"),
        "cm_switchignorelist" | "2922" => Some("ignore-toggle"),
        "cm_srcexecs" | "311" => Some("only-programs"),
        "cm_srcallfiles" | "312" => Some("clear-filter"),
        "cm_showonlyselected" | "2023" => Some("only-marked"),
        "cm_verticalpanels" => Some("stacked"),
        "cm_executedos" => Some("terminal"),
        "cm_copynamestoclip" | "2017" => Some("copy-names"),
        "cm_copyfullnamestoclip" | "2018" => Some("copy-paths"),
        "cm_copysrcpathtoclip" | "2029" => Some("copy-source-path"),
        "cm_copytrgpathtoclip" | "2030" => Some("copy-target-path"),
        "cm_copyfiledetailstoclip" | "2036" => Some("copy-details"),
        "cm_rereadsource" => Some("refresh"),
        "cm_searchfor" => Some("search"),
        "cm_searchstandalone" | "545" => Some("search-separate"),
        "cm_multirenamefiles" | "multirename" => Some("multi-rename"),
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
        "cm_srcshort" | "301" => Some("short-view"),
        "cm_srclong" | "302" => Some("long-view"),
        "cm_srcthumbs" | "269" => Some("thumbnail-view"),
        "cm_srccomments" | "300" => Some("comment-view"),
        "cm_editcomment" | "2700" => Some("edit-comment"),
        "cm_setattrib" | "502" => Some("edit-metadata"),
        "cm_printfile" => Some("print-file"),
        "cm_printdir" => Some("print-directory"),
        "cm_printdirsub" | "2028" => Some("print-directory-recursive"),
        "cm_srctree" | "303" => Some("directory-tree"),
        "cm_srccustomviewmenu" | "270" => Some("column-sets"),
        "cm_srcviewmodelist" | "333" => Some("view-modes"),
        "cm_toggleautoviewmodeswitch" | "2947" => Some("auto-view-toggle"),
        "cm_separatetreeoff" | "3200" => Some("separate-tree-off"),
        "cm_separatetree1" | "3201" => Some("separate-tree-one"),
        "cm_separatetree2" | "3202" => Some("separate-tree-two"),
        "cm_switchseparatetree" | "3203" => Some("separate-tree-cycle"),
        "cm_toggleseparatetree1" | "3204" => Some("separate-tree-toggle-one"),
        "cm_toggleseparatetree2" | "3205" => Some("separate-tree-toggle-two"),
        _ => None,
    }
}
