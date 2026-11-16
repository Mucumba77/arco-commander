# Abschlussarbeit Bereiche 1 und 2

Abschlussstand Arco Commander 1.4.0, Quellstand über Tag `v1.4.0`. Dieser Katalog dokumentiert die Implementierung von UI-01–UI-10 und FILE-01–FILE-10 sowie deren automatisierte Nachweise.

Die Anwendung wird nicht gestartet. Fenster-, reine Maus-/Tastatur-, DPI-, Drucker- und externe Hardwareabnahmen bleiben beim Projektinhaber. Privilegierte Referenzfälle werden getrennt protokolliert; ein ausgelassener Fall ist kein bestandener Test.

## Abschlussstatus

**Implementierung: UI-01–UI-10 = 10/10 IDs, 100 %; FILE-01–FILE-10 = 10/10 IDs, 100 %.** Bewertet ist der definierte Matrixumfang. Fenster-/Hardware-/Rechteabnahme und direkte TC-Gegenläufe werden getrennt geführt. Dieser Quellstand ist Bestandteil von Version 1.4.0.

## Nachweise

Die folgenden Testergebnisse und Grenzen gehören zum Abschlussstand. Der Abschlusskatalog ersetzt die frühere grobe Einschätzung von etwa 90 % beziehungsweise 95 %.

Abschließender Standardlauf: **343 Tests bestanden, 41 ausdrücklich markierte Umgebungsfälle ausgelassen, 8 Suiten** (`cargo test --workspace --all-targets --locked --offline -- --test-threads=1`). Zusätzlich **vier SDK-Referenztests bestanden**: drei WFX-Fälle auf x86/x64 und ein zusammengesetzter WDX-/WFX-Spaltenleser. Die gesonderte Katalogprüfung bestand zwei Tests. Auslassungen sind keine positiven Referenznachweise.

`cargo build --workspace --locked --offline`, `cargo fmt --all -- --check` und Diff-Whitespaceprüfung erfolgreich; Slint-Warnungen verboten. Die normale Windows-EXE wurde gebaut und als PE-GUI-Anwendung geprüft. Kein Produktstart, auch kein `--help`/`--version`. Plugin-Protokoll unverändert 13.

Portable `.tab`-Dateien enthalten Spaltendefinitionen, keine ausführbare Plugin-Registrierung. Für WDX wird ausschließlich eine bereits aktivierte Registrierung gebunden (exakter Pfad, danach eindeutiger Dateiname beziehungsweise Stammname). WFX-Definitionen bleiben nach Plugin-Anzeigename erhalten, auch wenn die Registrierung erst später erfolgt. Fehlende oder deaktivierte Plugins werden gemeldet. Fremde DLL-Pfade werden durch den Tab-Import nicht freigegeben.

Der gemeinsame Spaltenleser verarbeitet maximal 96 sichtbare Einträge pro Batch. Wiederholte Feldabfragen werden geteilt; WFX-Inhaltsfelder werden in SDK-Batches gemeinsam abgefragt. Ein einzelnes unformatiertes Feld behält seinen numerischen, Zeit-, Booleschen oder Text-Sortierschlüssel; zusammengesetzte Texte sortieren nach dem dargestellten Text. Grenzen: 128 verschiedene Felder, 64 KiB pro Wert, 16 MiB pro Abfrage. Virtuelle Providerpfade werden nicht als lokale Dateipfade geöffnet. Providerfremde Metadaten bleiben leer; fehlende Plugins erzeugen eine sichtbare Meldung.

Die zusätzlichen Einstellfelder sind per Maus und Tastatur erreichbar: automatische Spaltenbreite, Mindestbreite der Namensspalte, horizontales Scrollen, Modusbefehle und Symbolpfad. Symbole werden im Hintergrund geladen. `*`, `-` und `|` steuern Wiederholung, Tabwechsel und Verlassen bei Modusbefehlen. Die vorhandenen internen TC-Aliase laufen über denselben Dispatcher wie manuelle Befehle. Die allgemeine Erweiterung beliebiger TC-Benutzerbefehle bleibt beim gesonderten Befehlszeilen-/Konfigurationsbereich der Gesamtmatrix.

Der Junction-Transfer kopiert Linkobjekte einschließlich gültiger/dangling Ziele und durchläuft deren Verzeichnisse nicht. Kopien berücksichtigen die Linkoption auch rekursiv; Verschieben erhält Junctions als Linkobjekte. Attribute, Zeiten, DACL/Besitzer und Datenströme werden am geöffneten Link übertragen. Datenstromnamen werden begrenzt und gegen Pfad-/Stream-Injektion geprüft. Überwachungs-ACLs benötigen weiterhin das Windows-Recht. Kopieren verlangt bei Junction-Konflikten einen freien Namen oder Überspringen.

Die OLE-Quellverwaltung nimmt vor der Übergabe einen geschützten Snapshot auf und entfernt nur nach übereinstimmender synchroner MOVE-Rückmeldung bekannte, erneut geprüfte Namen. Verzeichnisse werden ohne Reparse-Traversierung erfasst; geänderte Quellen verhindern die Bereinigung. Ausgewählte Hardlinks werden einzeln entfernt, andere Namen bleiben erhalten. Der Shell werden die geschützten physischen Elternpfade übergeben. Ein eigenes CF_HDROP-Datenobjekt erhält Pfade bei Ablehnung durch den Shell-Parser. Die Bereinigung ist auf 256 Auswahlwurzeln, 16.384 Knoten und 256 externe Elternverzeichnisse begrenzt; außerhalb dieser Grenzen bleibt eine nicht bestätigte Quellenbereinigung sichtbar. Der optionale Referenztest benötigt `ARCO_DRAG_REFERENCE_DIRECTORY` und öffnet keine Fenster.

Windows-Referenz: [Reparse-Operationen](https://learn.microsoft.com/en-us/windows/win32/fileio/reparse-point-operations), [Dateiströme](https://learn.microsoft.com/en-us/windows/win32/fileio/file-streams), [Dateiidentitäten](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-getfileinformationbyhandle), [Löschen einzelner Namen per Handle](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntddk/ns-ntddk-_file_disposition_information_ex).

## Ergänzter Variantenabgleich

- WFX-Tabs unterscheiden Standard-, Plugin- und benannte eigene Ansichten pro Tab. Native TC-WFX-Spaltennummern und Sortierpositionen werden getrennt von lokalen Spaltensätzen gespeichert, importiert und exportiert. Ansichtsmodi erhalten entsprechende Provider-Varianten und bewahren sie beim Bearbeiten. Manuell aktivierte Modi behalten Befehle und Symbole; eine Darstellungsänderung durch einen Modusbefehl beseitigt den Ausgangsbefehl nicht.
- Shell-Orte bevorzugen auflösbare Namen bzw. Dateisystemadressen. Der Fallback `shell:arco-pidl:` speichert eine validierte absolute Windows-ID mit Grenzen von 64 KiB und 512 Elementen. Die Oberfläche zeigt weiterhin den lesbaren Providernamen. Die Identität ist keine Zusage rechnerunabhängiger Portabilität jedes Shell-Anbieters.
- Die Linkübertragung umfasst nun auch symbolische Datei-/Verzeichnislinks, relative und nicht auflösbare Ziele. Ein expliziter Referenzlauf konnte seine Symlink-Testquellen wegen Windows-Fehler **1314** nicht erstellen. Dieser Lauf ist **nicht bestanden/nicht ausführbar**, kein positiver Übertragungsnachweis. Keine Rechteerhöhung und kein Entwicklermodus wurden ausgelöst. Strukturelle Symlink-Prüfungen und nicht privilegierte Junction-Übertragungen laufen getrennt.
- Englisch und Französisch enthalten statische Beschriftungen sowie begrenzte Vorlagen für dynamische Meldungen. Die Vorlagenprüfung verlangt identische Feldnamen/Formatspezifikationen und eindeutige Quellschlüssel. Nutzerpfade/-namen bleiben unverändert; nur benannte eingebettete Fehler werden rekursiv übersetzt. Sprachwahl auf Worker-Threads verändert keine Slint-Oberfläche. Technische Parserkennungen und fremde Windows-/Plugin-Texte bleiben in ihrem jeweiligen Format bzw. ihrer Anbietersprache.

## Abgleich aller IDs

| ID | Zugeordnete Umsetzung / Nachweise | Separat abnehmen |
| --- | --- | --- |
| UI-01 | Panel-/Tabmodell, Splitter, Seitenwechsel/-tausch, horizontal/vertikal und volle Breite; Modell-/Navigationsfälle | Maus, Tastatur, DPI |
| UI-02 | `profile.rs`, `profile/tab_views.rs`, `app/tabs.rs`; lokale/virtuelle Tabs, Gruppen, Sperren, portable Definitionen und native WFX-IDs | Direkter TC-Gegenlauf und Fenster |
| UI-03 | Laufwerke, Kapazität, Breadcrumbs, Verlauf und verschachtelte Favoriten einschließlich INI-/Variablenimport | Laufwerks-/UNC-Umgebungen |
| UI-04 | Listen-/Baum-/Kommentar-/Miniatur-/Schnellansicht und native Shell-Anbieter/Identitäten | Shell-/WLX-Fenster und Renderer |
| UI-05 | `view_settings`, `wdx/columns.rs`, `view_setup`; Ausdrücke, typgerechte Werte, Provider-Varianten, Optionen, Regeln, Farben und Modus-Lebenszyklus | Medien, Geräte, Plugin-Fenster |
| UI-06 | Zweig-/Auswahl-/Programmfilter und Ignorierliste für lokal, FTP, WFX sowie projizierten Archivabgleich | Fenster und weitere Server |
| UI-07 | Masken, Auswahl merken/laden/speichern, Zwischenablage und Einfg-Cursorpfad | Reine Maus-/Tastaturabläufe |
| UI-08 | Schnellsuche/-filter, RegEx, Startmodifikatoren und Ziffernblock | Native Alt-Tasten/Fokus |
| UI-09 | Buttonbars, Starter, Vorlagen, `.bar`, Symbole, TC-Argumente, unveränderte Parameterzeilen und Layout/Farbschema | Tatsächliche externe Aufrufe und Fenster |
| UI-10 | Drei unterstützte Sprachen, Hilfen, Shortcutanzeige und Fenster-/Instanzprofile; Katalog-/Vorlagenprüfung | Monitor, DPI, Sprache/Fokus |
| FILE-01 | Lokale und virtuelle Dateiaufträge, Kopieren/Verschieben/Umbenennen/Löschen; Transfer-/Schutzfälle | Papierkorb/Fenster |
| FILE-02 | Zielmasken, gespeicherte Filter, mehrere Ziele und Alias-/Quellüberschneidungsprüfung | Fenster |
| FILE-03 | Konfliktregeln, Datei-/Zielversion, Vorschau, Anhängen und alternative Namen für geeignete lokale/virtuelle Anbieter | Zusätzliche Plugins/Fenster |
| FILE-04 | Warteschlange, Fähigkeiten, Pause/Abbruch/Limit/Retry, Fortschritt und Log | Nebenläufige Fensterabläufe |
| FILE-05 | Attribute/Zeiten, Größen, Eigenschaften und `descript.ion`; NTFS- und erzwungener Copy/Remove-Fall | Echte Volumegrenze, exFAT |
| FILE-06 | Lange/Unicode-/UNC-Pfade, Refresh, Links, ADS, Sicherheitsdaten, Hardlink-Gruppen, Sparse und Erstellungszeit | Besitzer/SACL, symbolische Links mit Rechten, weitere Dateisysteme |
| FILE-07 | System-/interne Zuordnungen, Explorer-Menü, Öffnen mit, Verknüpfungen, UAC und anderer Benutzer | Tatsächliche Rechteabfrage/native Menüs |
| FILE-08 | Datei-/Textzwischenablage, Drag-and-drop, klassische/optimierte Rückmeldungen und geschützte Snapshots | Explorer, andere Volumes/SMB |
| FILE-09 | Shell-Orte, Netzlaufwerke, Freigaben/Administration, Datenträger- und Systeminformationen | Privilegierter SMB-Loopback, mehrere Konten |
| FILE-10 | Listen-/Textdruck, Tiefe, Vorschau und Windows-Druckeinstellungen | Tatsächlicher Druck und Druckerfenster |

Die Quellen und Prüfliste sind in [NAVIGATION-UND-DATEIEN.md](NAVIGATION-UND-DATEIEN.md) und [UI-PRUEFUNG.md](UI-PRUEFUNG.md) verknüpft. Der Gesamtstatus ist nach finalem Workspace-Test und normalem Windows-Build geschlossen. Die aufgeführten Fenster-/Umgebungsreferenzen bleiben offen.
