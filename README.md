# Arco Commander

Arco Commander ist ein Zwei-Fenster-Dateimanager für Windows, geschrieben in Rust und inspiriert von Norton Commander und Total Commander. Die Desktop-Oberfläche verwendet Slint. Die erste Zielplattform ist Windows 11 x64.

## Entwicklungsstand

Die Oberfläche hat zwei kompakte, virtualisierte Dateilisten mit getrennten Spalten für Name, Erweiterung, Größe, Datum und Attribute. Die sortierte Spalte zeigt ihre Richtung an. Laufwerk und Speicherplatz stehen oberhalb der anklickbaren Pfadsegmente. `[..]` führt in den Elternordner und gehört nie zu einer Dateiauswahl. Das Verhältnis der beiden Fenster lässt sich an der Trennlinie ziehen und wird im Profil gespeichert.

Die Dateiliste verwendet standardmäßig Segoe UI Regular mit 14 logischen Pixeln. Unter **Einstellungen → Schrift** lassen sich Segoe UI, Arial und Verdana sowie 12, 14, 16 oder 18 Pixel auswählen. Zeilenhöhe und Detailspalten passen sich der Schriftgröße an.

Das Hauptmenü und die Symbolleiste gruppieren die häufigen Aktionen. `Alt+D` öffnet die Pfadeingabe, `Strg+D` die Ordnerfavoriten und `Strg+S` den Schnellfilter. `Strg+Q` zeigt Text, Hexdaten oder ein Bild zur fokussierten Datei im gegenüberliegenden Fenster. `Strg+Umschalt+L` setzt den Fokus in die Befehlszeile. Dort funktionieren `cd <Pfad>`, Arco-Befehle aus `Strg+K` und die unterstützten `cm_`-Aliase. Die klassische Funktionsleiste reicht von `F2` bis `F8`. Kopieraufträge laufen im Hintergrund und melden Konflikte.

`Alt+F1` öffnet die linke und `Alt+F2` die rechte Laufwerkswahl. Wähle mit Pfeiltasten und Enter oder tippe den Laufwerksbuchstaben. In der Dateiliste wählt ein Linksklick den Eintrag aus. Doppelklick und `Enter` öffnen Dateien und Ordner. `Strg`-Klick markiert einzeln und `Umschalt`-Klick markiert einen Bereich. Rechtsklick und `Umschalt+F10` öffnen bei lokalen Dateien das klassische Windows-Kontextmenü. Beim Rechtsklick auf eine markierte Datei gilt das Menü für die markierte Auswahl im selben Ordner.

`↑` und `↓` bewegen den Cursor in der Dateiliste. Mit `Rücktaste` oder `Strg+Bild↑` wechselst du zum Elternordner. Wiederholte Rücktasten überspringen noch laufende Ladevorgänge und wechseln zum zuletzt angeforderten Elternordner.

Über das Zahnrad neben der Tab-Leiste lässt sich der aktive Ordner-Tab benennen, duplizieren, umordnen oder ins andere Dateifenster kopieren und verschieben. Ein fest gesperrter Tab öffnet Ordnerwechsel in einem neuen Tab. Die Sperre mit erlaubtem Ordnerwechsel kehrt beim Aktivieren oder über die Schaltfläche für den Stammordner zur gespeicherten Basis zurück. `Strg+Umschalt+A` öffnet die Tab-Liste. **Ansicht → Tab-Gruppen verwalten** speichert benannte Gruppen für beide Dateifenster und lädt sie durch Anhängen oder Ersetzen. Bei FTP- und Archiv-Tabs speichert das Profil den lokalen Ersatzordner.

In **Ansicht → Tab-Gruppen verwalten → Total-Commander-Datei importieren** kannst du eine `.tab`-Datei als Gruppe speichern. Arco ordnet `[activetabs]` der beim Start des Imports aktiven Seite zu und `[inactivetabs]` der anderen Seite. Die Vorschau zeigt lokale Pfade, Titel, den aktiven Tab und unterstützte Sperren. Weitere Optionen und übersprungene virtuelle Pfade werden gezählt. Beim Ersetzen bleiben Seiten ohne importierte Tabs erhalten. [Total-Commander-Hilfe und FAQ](https://www.ghisler.com/dfaq.htm)

**Ansicht → Ordner-Tabs als .tab exportieren** speichert die offenen Tabs beider Seiten oder einer einzelnen Seite. Gespeicherte Gruppen haben denselben Export in ihrem Gruppenmenü. Arco schreibt UTF-16LE und erwartet einen neuen Dateinamen. Die Befehlszeile unterstützt `SAVETABS`, `SAVETABSL`, `SAVETABSR`, `SAVETABS2`, `SAVETABS2L`, `SAVETABS2R`, `OPENTABS` und `APPENDTABS` mit einer `.tab`-Datei als Argument.

Im Menü **Ansicht** lassen sich markierte lokale Ordner als Hintergrund-Tabs auf beiden Seiten öffnen. **Kürzlich geschlossene Tabs** zeigt die letzten 20 einzeln geschlossenen Tabs der laufenden Sitzung und öffnet sie auf ihrer ursprünglichen Seite.

**Konfigurieren → Einstellungen** enthält Seiten für Layout, Bedienung und Tastatur. Im Layout lassen sich die obere und mittlere Symbolleiste, Laufwerksbuttons und Laufwerksliste, Pfad, Verlauf und Spaltenüberschriften separat ein- und ausblenden. `Alt+D` öffnet die Pfadeingabe auch bei ausgeblendetem Pfad. Unter Bedienung lässt sich einstellen, ob die Ziffernblock-Auswahl Ordner einbezieht.

Unter **Tastatur** wählst du einen Befehl, klickst auf **Tastenkürzel aufzeichnen** und drückst die Kombination. **Zuweisen** ergänzt eine Belegung. Bei einem Konflikt zeigt Arco den bisherigen Befehl an und bietet **Belegung ersetzen** an. Einzelne Belegungen lassen sich entfernen. **Standardbelegung laden** setzt alle Kürzel im Entwurf zurück. **Anwenden** oder **OK** speichert die Änderungen im Profil. Systemtasten wie `Alt+F4` sowie `Esc` und das alleinige `F10` behalten ihre Bedienfunktion.

Die Standardbelegung folgt bei `Alt+←/→` dem Ordnerverlauf. `Num +` und `Num −` erweitern beziehungsweise verkleinern die Auswahl nach Maske. `Num *` kehrt die Auswahl um und `Num /` stellt die vorherige Auswahl wieder her. `Alt+Num +/−` arbeitet mit der Erweiterung der Datei unter dem Cursor.

`Strg+Umschalt+B` zeigt markierte lokale Dateien und alle Dateien in den markierten Ordnern in einer Liste. Erneutes Drücken kehrt zur Ordneransicht zurück. [Total-Commander-Funktionsbeschreibung](https://www.ghisler.ch/board/viewtopic.php?t=26819)

`Strg+L` zeigt die logische Größe der lokalen Auswahl und den von ihren Dateidaten belegten Speicher. Windows liefert die Belegung komprimierter und dünn belegter Dateien. Hardlinks zählen für den belegten Speicher einmal. Bei Abfragefehlern kennzeichnet der Dialog die Teilsumme. `Alt+Umschalt+Enter` trägt die logischen Größen der sichtbaren Ordner in die Größenspalte ein. `Esc` bricht die Berechnung bei fokussierter Dateiliste ab. [Total-Commander-Befehlsreferenz](https://www.ghisler.ch/wiki/index.php?title=List_of_internal_commands)

Im Menü **Dateien** lassen sich Namen, Pfade und Details der Dateiauswahl als Text kopieren. Die Detailansicht gibt Name, Erweiterung, Größe, Datum, Attribute und aktive WDX-Spalten als tabulatorgetrennte Werte aus.

Über **Werkzeuge** stehen die Zweigansicht, Text-, Hex- und Bildvorschau, rekursive Datei- und Textsuche, ein Textvergleich, Mehrfachumbenennung, Verzeichnissynchronisierung mit Vorschau, Prüfsummen, Duplikatsuche, Teilen und Zusammenfügen sowie ZIP-Funktionen inklusive AES zur Verfügung. Der Import aus `wincmd.ini` ergänzt Tabs, Verzeichnisfavoriten und unterstützte Kürzel aus `[Shortcuts]`. Die Vorschau zeigt betroffene Standardbelegungen. Eigene Arco-Belegungen haben Vorrang. [TC-Profilformat](https://www.ghisler.ch/wiki/index.php/Wincmd.ini)

- **Archive.** Arco liest ZIP-Dateien mit Deflate64, BZIP2, LZMA, PPMd, XZ und Zstandard. Der optionale 7-Zip-Adapter listet, prüft und entpackt weitere Formate. Er erstellt 7z-, TAR- und GZIP-Archive. GZIP akzeptiert eine einzelne Datei. Der Adapter benötigt 7-Zip 26.05 oder neuer. Arco sucht zuerst am absoluten Pfad in `ARCO_7ZIP_PATH`, dann unter `7zip\7z.exe` neben Arco und schließlich unter `%ProgramFiles%\7-Zip\7z.exe`. [7-Zip-Download](https://www.7-zip.org/download.html)
- **FTP und FTPS.** Verbindungen öffnen sich im aktiven Dateifenster und nutzen für gewöhnliche Transfers das mit Windows gelieferte `curl.exe`. Downloads können ein geprüftes Teilstück fortsetzen, sofern der Server die benötigten Angaben und Bytebereiche liefert. Direkter FXP-Transfer ist zwischen geeigneten Klartext-FTP-Servern über IPv4 verfügbar. Der Zielserver kann nach einem Abbruch eine temporär benannte Teildatei behalten.
- **Total-Commander-Plugins.** Unter **Konfigurieren → Plugin-Verwaltung** kannst du WCX-, WFX-, WDX- und WLX-DLLs registrieren, ZIP-Pakete mit `pluginst.inf` installieren oder Registrierungen aus `wincmd.ini` importieren. Dort kannst du Plugins aktivieren, deaktivieren, umordnen, umbenennen, per Dateimaske eingrenzen, prüfen und entfernen. Neue Registrierungen sind zunächst deaktiviert. Die Architektur liest Arco aus der DLL. Beim Prüfen und Ausführen lädt der passende Host die DLL mit deinen Windows-Rechten. [Ghisler-Plugin-SDKs](https://www.ghisler.com/plugins.htm), [Format für Plugin-Installationspakete](https://www.ghisler.ch/wiki/index.php/Plugins_Automated_Installation)

Die Plugin-Werkzeuge verwenden die aktivierten passenden Registrierungen. `F3` verwendet das erste aktive WLX-Plugin mit passender Dateimaske in einem eigenen Fenster. `Strg+Q` bettet den passenden WLX-Host in die Schnellansicht ein. Wenn der Host nicht startet oder beendet wird, übernimmt die interne Vorschau. **Dateien → Intern anzeigen** öffnet den eingebauten Betrachter. Einfache TC-Regeln für Dateierweiterungen werden beim Import zu Masken. Weitere TC-Erkennungsregeln sind als Hinweis sichtbar.

Wähle in der **Plugin-Verwaltung** ein aktives WCX- oder WFX-Plugin und öffne **Dateioperationen**. Bei WCX kannst du Archive prüfen, einzelne Dateien entpacken, packen und löschen, sofern das Plugin die jeweilige Funktion bereitstellt. Änderungen werden an einer Archivkopie geprüft und anschließend veröffentlicht. WFX unterstützt Upload, Download, Umbenennen, Verschieben, Anlegen von Ordnern sowie das Löschen von Dateien und leeren Ordnern. Lokale Downloads erwarten einen neuen Zieldateinamen. Bei einem abgebrochenen WFX-Schreibauftrag lässt sich der tatsächliche Zielzustand im Plugin-Dateisystem prüfen. [Ghisler-Plugin-SDKs](https://www.ghisler.com/plugins.htm)

**Netz → WFX-Dateisystem öffnen** lädt ein aktiviertes WFX-Plugin in das aktuelle Dateifenster. Die Pfadleiste akzeptiert Plugin-Pfade, und Ordner lassen sich per Doppelklick oder `Enter` öffnen. `F5` kopiert Dateien und vollständige Ordner. `F6` verschiebt sie zwischen einem lokalen Ordner und einem WFX-Fenster in beide Richtungen. In zwei Fenstern desselben Plugins übernimmt das Plugin das Verschieben. `F3` zeigt eine lokale Vorschaukopie an. `F7` erstellt einen Ordner, `Umschalt+F6` benennt einen Eintrag um. `F8` löscht nach Bestätigung Dateien und Ordner samt Inhalt dauerhaft. **Netz → WFX verlassen** kehrt zum lokalen Ordner zurück.

Rekursive Transfers erhalten leere Unterordner und erwarten einen neuen Zielnamen. Beim Verschieben zwischen lokalem Fenster und Plugin vergleicht Arco den übertragenen Inhalt bytegenau, bevor es die Quelle entfernt. Dazu liest es die Dateien über das Plugin erneut. Ein Fehler beim Entfernen wird als Teilzustand gemeldet. Rekursive Aufträge folgen keinen Verknüpfungen im Quellbaum. Ordner direkt im virtuellen Stamm sind beim rekursiven Löschen und beim Verschieben zwischen WFX und lokalem Fenster geschützt, da sie Verbindungen darstellen können.

Wenn das Plugin nach Zugangsdaten oder einer Bestätigung fragt, öffnet Arco einen Dialog. Passwortfelder sind maskiert. Arco übergibt die Antworten direkt über die Prozess-Pipe und speichert sie weder im Profil noch in temporären Dateien. Ein neuer Host kann erneut nach einer Anmeldung fragen. Ein WFX-Mehrfachauftrag verarbeitet Gruppen von bis zu 48 Einträgen im selben Host. Der Bericht ordnet Konflikte und Fehler den Einträgen zu.

Plugins mit `FsSetDefaultParams` erhalten ein eigenes Einstellungsverzeichnis `arco-wfx/<Pluginname>` neben der DLL. Ghislers SFTP-Plugin legt dort seine `sftpplug.ini` mit Verbindungsprofilen und bekannten Hostschlüsseln ab. Eine neue Verbindung lässt sich im Plugin-Stamm über `F7` einrichten. [Ghisler SFTP-Plugin](https://www.ghisler.com/plugins.htm)

WCX kann markierte lokale Dateien gemeinsam zum Packen vormerken. Zum Entpacken oder Löschen mehrerer Archiveinträge gib die Pfade mit `|` als Trennzeichen ein. Jede Änderung am Archiv wird separat geprüft und veröffentlicht. Der Bericht führt bereits abgeschlossene Einträge auf.

**WDX-Spalte hinzufügen** in der Plugin-Verwaltung zeigt die vom ausgewählten WDX-Plugin angebotenen Felder. Bis zu drei Spalten lassen sich in lokalen Detailansichten anzeigen. Arco lädt sichtbare Werte im Hintergrund in der ersten vom Plugin angebotenen Einheit. Ein Klick auf den Spaltenkopf sortiert den geladenen Dateibestand nach Feldtyp. Ein zweiter Klick kehrt die Richtung um. Fehlende Werte stehen innerhalb ihrer Datei- oder Ordnergruppe am Ende. Ein neuer Verzeichnisinhalt beginnt wieder mit der Standardspalte des Tabs. Unter **Konfigurieren → WDX-Spalten verwalten** lassen sich Spalten entfernen.

Unter **Befehle → Nach Plugin-Feld suchen** kannst du bis zu 16 Bedingungen aus aktiven WDX-Plugins hinzufügen. Die Suche kombiniert eine Dateimaske mit Teiltreffern, Vergleichen wie `>= 1024` oder einem regulären Ausdruck mit `~` am Anfang. In der Übersicht wählst du, ob alle oder mindestens eine Bedingung erfüllt sein muss. **Als Suchprofil speichern** merkt sich Maske, Bedingungen und Modus. **Befehle → WDX-Suchprofile** öffnet die Abfrage für den gerade aktiven lokalen Ordner. Die Suche schließt seine Unterordner ein und zeigt die Treffer im Dateifenster.

In der Mehrfachumbenennung fügt `[=Plugin.Feld]` denselben Feldwert in die Namensmaske ein. Ein fehlender Wert oder ungültiger Zielname stoppt die Vorschau. Der Platzhalter verwendet die erste Einheit des Feldes. [Total-Commander-Referenz zur Mehrfachumbenennung](https://ghisler.ch/wiki/index.php?title=Multi-Rename_Tool)

ZIP-Pakete erhalten einen eigenen Ordner neben dem Profil. Beim Entfernen der letzten zugehörigen Registrierung entfernt Arco das verwaltete Paketverzeichnis. Extern registrierte DLLs bleiben am ursprünglichen Ort.

### Archive in den Dateifenstern

- Öffne ein unterstütztes Archiv mit `Enter` oder mit der eingestellten Mausaktion. `[..]` führt aus dem Archivstamm zum Ordner mit der Archivdatei zurück. Die Pfadeingabe akzeptiert auch `C:\Ordner\Beispiel.zip::Unterordner`.
- `F3` zeigt die fokussierte Datei an. `Strg+Q` öffnet die Schnellansicht für unverschlüsselte Einträge. `Alt+F7` sucht im aktuellen Archivordner und seinen Unterordnern nach Dateinamen. ZIP und JAR unterstützen auch Textsuche. Vorschau und Textsuche verarbeiten bis zu 64 MiB pro Datei.
- `F5` entpackt markierte Einträge in den lokalen Ordner gegenüber. Ist dort ein ZIP geöffnet, kopiert `F5` stattdessen aus dem aktiven Ordner oder Archiv in den angezeigten ZIP-Ordner. Namenskonflikte werden vor dem Kopieren geprüft. Vorhandene Dateien und ZIP-Einträge werden nicht überschrieben.
- `Umschalt+F6` benennt Dateien oder Ordner in unverschlüsselten ZIPs um. `F8` entfernt die markierte Auswahl nach Bestätigung dauerhaft aus dem ZIP. Arco schreibt beim Bearbeiten eine temporäre Datei und ersetzt das bisherige ZIP nach Abschluss.
- Vorhandene Änderungszeitstempel erscheinen in den Archivfenstern und werden bei der Extraktion von Dateien übernommen. ZIP speichert seine klassischen Zeitstempel in lokaler Zeit mit Zwei-Sekunden-Auflösung.

### Dateifenster vergleichen und synchronisieren

`Umschalt+F2` markiert neuere und einseitig vorhandene Dateien. Im Menü **Markieren** kannst du zusätzlich einseitig vorhandene Ordner einbeziehen oder gleich große Dateien mit gleichem Zeitstempel ausblenden. Ein erneuter Aufruf des Ausblendbefehls zeigt die Dateien wieder an. In der Befehlszeile unterstützen `cm_CompareDirs` (`533`) und `cm_CompareDirsWithSubdirs` (`536`) auch die Parameter `1` für einseitige Einträge und `2` für einseitige Einträge und Größenunterschiede. `cm_DirMatch` (`534`) schaltet die Ansicht mit ausgeblendeten gleichen Dateien um. [Total-Commander-Befehlsreferenz](https://www.ghisler.ch/wiki/index.php?title=List_of_internal_commands)

Unter **Verzeichnisse synchronisieren** stehen beidseitiger Abgleich, Kopieren in eine Richtung und Spiegeln von links nach rechts zur Auswahl. Die Optionen umfassen Unterordner, leere Ordner, markierte Einträge, Inhaltsvergleich, Datum ignorieren und Dateimasken wie `*.txt;*.md | geheim*`. Benannte Profile speichern diese Optionen für die jeweils geöffneten Ordner. Die Vorschau zeigt Größe und Datum je Seite. Kopierrichtungen lassen sich je Eintrag auswählen. Bei **Spiegeln** erfordern die angezeigten Löschungen rechts eine ausdrückliche Bestätigung. Arco führt sie nach den Kopien aus und überspringt sie bei einem Kopierfehler.

Mit einem Archiv und einem lokalen Ordner kann Arco den Inhalt vergleichen und freigegebene Dateien aus dem Archiv in den Ordner kopieren. Der Vergleich benötigt freien Speicher für die temporär entpackten Daten. Für passwortgeschützte ZIP-Dateien lässt sich das Passwort für den Vergleich eingeben. [Total-Commander-Hilfe zur Synchronisierung](https://www.ghisler.com/advanced.htm)

### FTP in den Dateifenstern

Wähle links oder rechts das gewünschte Dateifenster und öffne **Netz → FTP-Verbindungen** oder drücke `Strg+F`. Lege unter **Neu** ein benanntes Profil an und speichere es. Die Adresse enthält bei Bedarf Port und Startordner. Ein lokaler Startordner wird nach erfolgreicher Anmeldung im anderen Dateifenster geöffnet. `Strg+N` öffnet die Schnellverbindung.

Ein Profil speichert das Protokoll, den Benutzernamen, einen optionalen HTTP- oder SOCKS-Proxy, den aktiven oder passiven Modus und das Verzeichnisformat. Im automatischen Modus versucht Arco MLSD, danach Unix- oder IIS-LIST und schließlich eine reine Namensliste. Über **Netz → FTP-Unix-Rechte ändern** lassen sich die Rechte der Auswahl oktal setzen, sofern der Server MLSD und `SITE CHMOD` unterstützt.

Arco fragt das Passwort beim Verbinden ab. Mit aktivierter Speicheroption liegt es in der Windows-Anmeldeinformationsverwaltung des aktuellen Kontos auf diesem Rechner. Das JSON-Profil enthält dafür eine Referenz. **Importieren** zeigt unterstützte Verbindungen aus `wcx_ftp.ini` vor der Übernahme an. Vorhandene Profilnamen bleiben erhalten. Für übernommene Verbindungen fragt Arco die Passwörter beim Verbinden erneut ab. Die Vorschau kennzeichnet übersprungene Spezialkonfigurationen. [Ghisler-Hilfe zur FTP-Konfiguration](https://www.ghisler.com/efaqftp.htm), [Windows Credential Manager API](https://learn.microsoft.com/en-us/windows/win32/api/wincred/nf-wincred-credwritew)

FTP verwendet dieselbe Pfadleiste, Tabs, Sortierung, Auswahl und Navigation wie lokale Ordner. Doppelklick oder `Enter` öffnen Ordner und zeigen Dateien in der Vorschau an. `F3` öffnet ebenfalls die Vorschau. `F7` erstellt einen Ordner, `Umschalt+F6` benennt einen Eintrag um und `F8` löscht die Auswahl nach Bestätigung dauerhaft.

`F5` öffnet die Kopiervorschau für Dateien und Ordner zwischen lokal und FTP sowie zwischen zwei FTP-Ordnern. Vorhandene Zieldateien sind abgewählt und lassen sich einzeln zum Ersetzen auswählen. Beim Kopieren zwischen FTP-Ordnern verwendet Arco je Datei eine temporäre lokale Kopie. **Netz → FTP-Verzeichnis anzeigen** öffnet die separate FTP-Ansicht mit dem Einzeldownload. **Netz → FTP trennen** schließt die betreffende Sitzung in allen Tabs und entfernt sie aus dem Verlauf. Das gespeicherte Profil verwendet für solche Tabs den zuletzt lokalen Ordner.

### FTP-Dateien bearbeiten

Drücke im FTP-Dateifenster `F4`, um eine Datei mit MLSD-Metadaten bis 64 MiB im eingestellten Editor zu öffnen. Speichere dort und wähle im Arco-Dialog **Änderungen hochladen**. Die Arbeitskopie lässt sich mehrfach speichern und übertragen. **Netz → Offene FTP-Bearbeitungen** öffnet den Dialog erneut.

Arco lädt die neue Fassung unter einem temporären Namen hoch und prüft die bisherige Serverdatei anhand ihrer Metadaten und ihres Inhalts vor dem Ersetzen. Ein Konflikt erhält die Arbeitskopie und die fremde Serverfassung. FTP stellt zwischen der Prüfung und dem Umbenennen keine gemeinsame Sperre bereit. Die neuen Dateirechte richten sich nach dem Server und der bereitgestellten Datei.

Die Arbeitskopien liegen unter `%LOCALAPPDATA%\Arco Commander\ftp-edits` und bleiben beim Trennen oder Beenden erhalten. Das Löschen einer Arbeitskopie verlangt eine Bestätigung.

### FTP mit einem lokalen Ordner abgleichen

Öffne einen FTP-Ordner und einen lokalen Ordner und wähle **Netz → FTP mit lokalem Ordner abgleichen**. In der Vorschau steht der lokale Ordner links und FTP rechts. Du kannst beide Richtungen, Unterordner, leere Ordner, markierte Einträge und Dateimasken wählen. Beim Spiegeln von lokal nach FTP werden zusätzliche Servereinträge nach separater Bestätigung und nur nach erfolgreichen Kopien gelöscht. Arco prüft vor dem Löschen erneut, ob die lokale Seite den Eintrag inzwischen enthält.

Für Abgleich und Schreiboperationen im Dateifenster benötigt Arco MLSD mit Typ, Dateigröße und UTC-Änderungsdatum für Dateien. Beim Inhaltsvergleich lädt Arco passende Dateien vorübergehend herunter. Downloads beim Abgleich übernehmen die Serverzeit. Uploads setzen den Zeitstempel, sofern der Server MFMT unterstützt. Arco überträgt Uploads unter einem temporären Namen und prüft den Zielstand vor der Veröffentlichung erneut. Wenn ein Server das direkte Ersetzen ablehnt, sichert Arco die alte Version vorübergehend per Umbenennung. Nach einem Verbindungsabbruch kann eine temporäre Datei oder Sicherung auf dem Server verbleiben. FTP bietet dabei keine gemeinsame Sperre gegenüber anderen Clients. [FTP-Synchronisierung in Total Commander](https://www.ghisler.com/screenshots/en/08.html), [curl-FTP-Kommandos](https://curl.se/docs/manpage.html#-Q)

## Bauen und starten

Voraussetzungen sind Rust 1.92 oder neuer und die MSVC-C++-Buildtools mit Windows SDK. Starte die Befehle in einer Developer PowerShell mit `cargo` im `PATH`.

```powershell
cargo build --locked --workspace
cargo run --locked -- --left 'C:\Quelle' --right 'C:\Ziel'
cargo test --locked --workspace --all-targets -- --test-threads=1
```

Für 32-Bit-Plugins muss neben `arco-commander.exe` zusätzlich `arco-plugin-host-x86.exe` liegen. In einer x86 Developer PowerShell lässt sich der Host so bauen und neben den x64-Build kopieren.

```powershell
rustup target add i686-pc-windows-msvc
cargo build --locked -p arco-plugin-host --target i686-pc-windows-msvc
Copy-Item target\i686-pc-windows-msvc\debug\arco-plugin-host.exe target\debug\arco-plugin-host-x86.exe
```

`--portable` legt das Sitzungsprofil neben der ausführbaren Datei ab. Mit `--profile <Datei>` lässt sich ein eigener Speicherort wählen.
Beim direkten Start von `arco-commander.exe` erscheint das Windows-Anwendungsfenster.

## Funktionsparität

Die Referenz bleibt Total Commander 11.58.1 für Windows. Die Funktionsparität ist noch offen. Dazu zählen unter anderem vollständige Plugin-Operationen und eingebettete Plugin-Vorschau, weitere Archivformate und Archivbearbeitung, vollständige TC-Befehls- und Profilkompatibilität, physische USB- und LPT-Transfers sowie Teile der Windows-Shell-Integration. Die Matrix führt die einzelnen Varianten und die bisher geprüften Abläufe auf.

- [Entwicklungsplan](PLAN.md)
- [Funktionsmatrix und Prüffälle](FUNKTIONSMATRIX.md)
- [Herstellerhilfe und Referenzindex für Dialoge, Einstellungen und Tastatur](TOTAL-COMMANDER-REFERENZ.md)
