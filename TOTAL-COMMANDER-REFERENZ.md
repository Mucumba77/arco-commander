# Total Commander als Referenz für Arco Commander

Für die Windows-Funktionsparität verwenden wir das Herstellerpaket 11.58.1. Die deutsche CHM-Hilfe der Reihe 11.58 ist die umfassendste gefundene Dokumentation der Oberfläche. Das geprüfte Hilfe-Paket enthält 206 HTML-Themen, davon 68 mit dem Präfix `dlg_`. Die Screenshots dienen als visuelle Beispiele. Der Hilfetext und der jeweilige Referenzdialog bestimmen zusammen das Verhalten. [Download 11.58.1](https://www.ghisler.com/ddownload.htm), [Deutsche Herstellerhilfe als CHM](https://www.totalcommander.ch/help/tcmdd1158_chm.zip), [Herstellerstartseite mit Bildergalerie](https://www.ghisler.com/deutsch.htm)

## Wo welche Information steht

| Frage | Quelle |
| --- | --- |
| Wie sieht ein häufiger Ablauf aus? | [Hersteller-Galerie](https://www.ghisler.com/screenshots_new/de/) und [Funktionsübersicht](https://www.ghisler.com/featurel.htm) |
| Welcher Schalter gehört zu welchem Dialog? | CHM-Themen unter `dlg_*.htm`, `menu_*.htm` und der integrierte Hilfeindex. Im Referenzprogramm öffnet `F1` die Hilfe für den aktuellen Kontext. |
| Welche Taste löst welche Aktion aus? | `keyboard.htm` in der CHM-Hilfe, ergänzt durch die Befehlsdefinitionen in `TOTALCMD.INC` und die deutschen Menüs in `WCMD_DEU.MNU`. Die Wiki-Tabelle beschreibt als zusätzliche Quelle die Version 11.57. [Interne Befehle und Parameter](https://www.ghisler.ch/wiki/index.php/List_of_internal_commands) |
| Wie wird eine Einstellung oder ein Shortcut gespeichert? | `inisettings*.htm` in der CHM-Hilfe und die Sektionen `[Layout]`, `[Configuration]` und `[Shortcuts]`. [INI-Referenz](https://www.ghisler.ch/wiki/index.php/Wincmd.ini) |
| Wie sehen Sonderfälle aus? | [Deutsche Hersteller-FAQ](https://www.ghisler.com/dfaq.htm), [Versionshistorie](https://www.ghisler.com/whatsnew.htm) und [Plugin-SDKs](https://www.ghisler.com/plugins.htm) |

## Einstellungsseiten

Die Dateinamen sind Suchanker für die CHM-Hilfe. Mehrere Seiten unterscheiden sich nach Version, Ansicht, Plugin oder installiertem Hilfsprogramm.

| Bereich | Themen in der Hilfe | Abzugleichende Optionen |
| --- | --- | --- |
| Layout und Symbolleisten | `dlg_config.htm`, `dlg_configbuttonbar.htm`, `dlg_mainmenu.htm`, `dlg_usermenu.htm` | Horizontale und vertikale Leisten, Laufwerksschalter, Pfad- und Statuszeilen, Menü- und Starterkonfiguration |
| Darstellung | `dlg_display.htm`, `dlg_configicons.htm`, `dlg_configfont.htm`, `dlg_configcolor.htm`, `dlg_configtabs.htm` | Sichtbarkeit und Anzeigeformate, System- und Dateitypsymbole, Schriften, Farbregeln, Spaltenabstände und Zeitformat |
| Ansichten und Ordner-Tabs | `dlg_configfoldertabs.htm`, `dlg_configcustomcolumns.htm`, `dlg_configviewmode.htm`, `dlg_configautomodeswitch.htm`, `dlg_configthumbs.htm` | Tab-Sperren, Metadatenspalten, Ansichtsvorlagen, automatische Regeln und Miniaturcache |
| Bedienung und Sprache | `dlg_configlanguage.htm`, `dlg_config2.htm`, `dlg_configedit.htm`, `dlg_listerconfig.htm` | Sprache, Maus- und Auswahlregeln, interne und externe Viewer, Editor und Zuordnungen |
| Dateioperationen | `dlg_configcopydelete.htm`, `dlg_configrefresh.htm`, `dlg_configignorelist.htm`, `dlg_configlogfile.htm`, `dlg_configconfirmation.htm`, `dlg_confighistory.htm` | Bestätigungen, Kopiermethoden, Aktualisierung, Ignorierregeln, Logs, Verlauf, Tasten und Aliase |
| Suche, FTP und Erweiterungen | `dlg_configquicksearch.htm`, `dlg_configftp.htm`, `dlg_configplugins.htm`, `dlg_contextmenuexts.htm` | Schnellfilter, Verbindungsvorgaben, Plugin-Reihenfolge und Shell-Erweiterungen |
| Archive | `dlg_configpack.htm`, `dlg_internalzipconfig.htm`, `dlg_internal7zipconfig.htm` | Eingebaute und externe Packer, Formatoptionen und Verschlüsselung |

Die [Wiki-Übersicht zum Einstellungsdialog](https://www.ghisler.ch/wiki/index.php/Configuration_dialog) ergänzt die CHM-Seiten. Bei Abweichungen prüfen wir die Option in der festgelegten Referenzversion.

## Arbeitsdialoge und Funktionsbereiche

| Bereich | Einstieg in der CHM-Hilfe | Was beim Abgleich zählt |
| --- | --- | --- |
| Hauptfenster und Menüs | `menu_files.htm`, `menu_mark.htm`, `menu_show.htm`, `menu_configuration.htm`, `menu_starter.htm`, `menu_help.htm`, `foldertabs.htm`, `drag_drop.htm` | Aktives Fenster, Tab- und Cursorzustand, Tastaturfokus, Mausauswahl und Drag-and-drop |
| Navigation und Auswahl | `dlg_changetree.htm`, `dlg_dirmenu.htm`, `dlg_quicksearch.htm`, `keyboard.htm` | Laufwerkswechsel pro Seite, Favoriten und Verlauf, Suchsprung gegenüber Schnellfilter |
| Dateien und Konflikte | `menu_fileactions.htm`, `menu_fileoperations.htm`, `dlg_attrib.htm`, `dlg_overwrite.htm`, `dlg_edit_comment.htm` | Attribute und Zeitstempel, Kopieren, Verschieben, Papierkorb, Überschreiben und Kommentare |
| Dateizuordnungen | `dlg_assotiate.htm`, `dlg_edit_file_type.htm`, `dlg_edit_internal_file_type.htm`, `dlg_edit_action.htm` | Windows-Standardaktion, eigene Zuordnungen, Befehlsparameter und Kontextmenü |
| Suche und Umbenennung | `dlg_search.htm`, `dlg_advancedsearch.htm`, `dlg_searchplugins.htm`, `dlg_srch_loadsave.htm`, `dlg_duplicates.htm`, `dlg_choosepluginfield.htm`, `dlg_multi_rename_tool.htm` | Suchkriterien und gespeicherte Abfragen, Metadatenfilter, Ergebnislisten, Vorschau und Rücknahme |
| Vorschau und Vergleich | `lister.htm`, `lister_menu*.htm`, `dlg_printoptions_lister.htm`, `dlg_compare_contents.htm`, `dlg_synchronize.htm`, `dlg_synchronize_dirs.htm`, `synchronize_mlsd.htm` | Text- und Binärmodi, Kodierung, Plugins, Druck, Vergleich und richtungsbezogene Synchronisierung |
| Archive und Dateiformate | `packedfiles.htm`, `dlg_pack.htm`, `dlg_unpack.htm`, `dlg_createsfv.htm`, `dlg_verifysfv.htm`, `dlg_filessplit.htm`, `dlg_filescombine.htm`, `dlg_filesencode.htm`, `dlg_filesdecode.htm` | Öffnen und Bearbeiten pro Format, Passwörter, Prüfsummen, kompatible Teilstücke und Kodierung |
| Netzwerk und System | `menu_net.htm`, `dlg_connect.htm`, `dlg_newconnection.htm`, `dlg_connectdetails.htm`, `dlg_firewallsettings.htm`, `dlg_network.htm`, `dlg_sysinfo.htm`, `dlg_sysinfosoft.htm` | FTP/FTPS, Proxy und Servervarianten, Netzlaufwerke und Systeminformationen |
| Befehle und Integration | `dlg_choosecommand.htm`, `commandlineparameters.htm`, `inisettings*.htm` | Befehlsnamen und IDs, Argumente, Startparameter, importierte Profile und externe Steuerung |

## Tastaturprüfung für Arco

| Belegung der Referenz | Erwarteter Prüffall in Arco |
| --- | --- |
| `Alt+F1` und `Alt+F2` | Linke beziehungsweise rechte Laufwerkswahl. Pfeiltasten, Enter, Laufwerksbuchstabe und Abbruch separat prüfen. |
| `Strg+F1` und `Strg+F2` | Kurz- und Detailansicht wechseln auf der aktiven Seite. |
| `Strg+F3` bis `Strg+F6` | Name, Erweiterung, Datum und Größe in dieser Reihenfolge. Die gewählte Richtung ist an der Spalte sichtbar. |
| `Strg+Bild↑` und `Strg+Bild↓` | Zum Elternordner wechseln beziehungsweise den aktuellen Eintrag öffnen. |
| `Umschalt+F10` | Windows-Kontextmenü am fokussierten Eintrag öffnen. |
| `Strg+S` und `Strg+F10` | Schnellfilter fokussieren beziehungsweise entfernen. |
| `Alt+←/→` | Im Ordnerverlauf zurück beziehungsweise vorwärts navigieren. |
| `Num +`, `Num −`, `Num *` und `Num /` | Maskenauswahl ergänzen oder verkleinern, Auswahl umkehren und vorherige Auswahl wiederherstellen. Die Option für Ordner und die Umschaltvarianten separat prüfen. |
| Tastenzuordnung unter Diverses | In Arco unter **Konfigurieren → Einstellungen → Tastatur** aufzeichnen. Konflikte, Entfernen, Zurücksetzen, Abbrechen und erneutes Laden des Profils prüfen. |

Für die vollständige Prüfung braucht jede Aktion zusätzlich Fokusvarianten, Konflikte mit anderen Belegungen und gegebenenfalls Verzeichnisse, Archive oder FTP als Kontext. Der Import der TC-Tastaturbelegung, Befehlsketten und zahlreiche Dialogbelegungen stehen in Arco noch aus. [Arco-Funktionsmatrix](FUNKTIONSMATRIX.md)
