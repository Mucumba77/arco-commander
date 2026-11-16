# Bereich 3 – Implementierung und Nachweise

Stand: 16. November 2026, Version 1.3.0. Der Quellstand ist über den Git-Tag `v1.3.0` nachvollziehbar.

Der Abschluss bezieht sich auf die definierten **TOOL-01 bis TOOL-13** der [Funktionsmatrix](../FUNKTIONSMATRIX.md). Implementierung und manuelle Abnahme werden getrennt bewertet. Die [Bedienungsanleitung](SUCHE-UND-WERKZEUGE.md) beschreibt die neuen Optionen; die [Fensterprüfliste](UI-PRUEFUNG.md) enthält die erwartete Abnahme durch den Projektinhaber.

## Automatisierter Abschlusslauf

**Build und statische Prüfung erfolgreich. 310 Tests bestanden, 38 explizite Umgebungsfälle ausgelassen, 8 Testsuiten. Zusätzlich 16 erfolgreiche Referenz-Testläufe.** Die ausgelassenen Fälle sind keine stillschweigend bestandenen Tests. Die Zusatzläufe enthalten unterschiedliche SDK-Architekturen und sind deshalb als Testläufe gezählt. Englisch und Französisch enthalten je 1212 eindeutige Schlüssel ohne fehlende Slint-Übersetzungen; alle 231 Slint-Bildverweise sind vorhanden. Beide aktuellen Plugin-Hosts wurden gebaut und über native ANSI-/Unicode-Referenzen geprüft. Eigene Tests sichern die abgeschlossene Eingabepipe ohne Konsoleneingabe sowie getrennte Lade- und Aufrufbudgets ab: Verzögertes Laden gelingt; eine blockierte Vorschaufunktion erreicht weiterhin das 10-Sekunden-Limit. Die FTP-Referenzen umfassen HASH mit vollständigem Bytebereich und einen ausdrücklich erzwungenen Download-Fallback.

Die normalen Prüfungen entsprechen der Windows-CI:

```powershell
cargo fmt --all -- --check
cargo build --workspace --locked
cargo test --workspace --all-targets --locked -- --test-threads=1
```

Die lokalen Läufe verwenden zusätzlich `--offline`. Slint-Warnungen werden beim Build als Fehler behandelt. Headless-Suchprozess-Tests führen ausschließlich `--search-worker` aus, ohne Hauptfenster oder GUI-Ereignisschleife. Es wurde keine normale Arco-Instanz und kein TC-Fenster gestartet.

## Nachweise nach Funktionsgruppe

| Gruppe | Automatisierte Abdeckung |
| --- | --- |
| Suche | Mehrere Suchorte, UTF-Dateilisten, Dateilisten-RegEx, Masken/Traverse-Filter, Größen/Alter/Uhrzeit, Text/Hex/Encoding, Office-/EPUB-Text, Partnersuche und Profilkompatibilität. Bei Suchorten mit überlappenden Teilbäumen wird jedes Mitglied nur einmal aufgenommen. Masken wie `\*.txt` bleiben an der jeweiligen Suchwurzel. |
| Suchprozess und Archive | Kriterien und Nur-Treffer-Modus über den separaten Prozess; Herkunft von Mitgliedern in unterschiedlichen Archiven, verschlüsselte ZIP-/7z- und Mehrteilerfälle, Nachsuche mit Tiefe 0, Abbruch und sichere Übernahme als Archivmitglieder beziehungsweise lokale Container. Die Implementierung verwendet echte Dateipfade und Archivpfade mit eigener Containeridentität. |
| Unabhängige Archive | Suchreferenzen werden mit 7-Zip für ZIP, 7z, TAR, Gzip, Bzip2 und XZ erstellt. Die Prüfsuche verwendet dieselben Namen-/Inhaltskriterien. Zusätzliche leere und nichtleere Bzip2-/XZ-Ströme prüfen fehlende Headernamen und unbekannte Größen. Der Adapter leitet den Namen nur für erkannte Stream-Signaturen ab und zählt bei Bedarf die entpackten Bytes mit Limit und Abbruch, ohne einen Zielinhalt zu veröffentlichen. Vorhandene Archivadapter benötigen je nach Format den eingerichteten externen Packer. |
| Everything | Der vorhandene Live-Nachweis für Everything/ES und EFU bleibt als Bestandsnachweis bestehen. Die aktuelle normale Suite prüft Parser, Suchbereiche und Rückfallverhalten. Es wird daraus kein neuer Live-Lauf mit jedem verfügbaren Everything-Build abgeleitet. |
| Duplikate | Gruppierung, Namens-/Ordner-/Profilfilter, Zeit-/Größenregeln, Gleichstände, mindestens eine unmarkierte Kopie und WDX-Bedingungen. Eigene ANSI-/Unicode-DLLs prüfen Feldnamen, typisierte Werte und benannte Einheiten über beide Hosts. |
| Interne Dateifelder | Abgleich aller 45 gewöhnlichen Feldnamen aus dem Referenzpaket, einschließlich Pfadlänge, Verzeichnis-Groß-/Kleinschreibung, Zufallszahlen und drei Dateinamen-Dekodiermodi. Pfad-/Namenssemantik, Zeiten, Attribute, Zonenstreams, DOS-Namen und native PE-Versionsressourcen. Die Version wird als Ressource gelesen, ohne das untersuchte Programm auszuführen. |
| Medienmetadaten | FFmpeg erzeugt unabhängige MP3-, FLAC-, Ogg-, Opus-, M4A-, WAV- und WavPack-Proben. Acht Felder je Container werden geprüft, ohne Wiedergabe. Der Parser begrenzt Speicher und gelesene Daten und unterstützt Abbruch. |
| Lister/Quick View | Klassifizierung zusätzlicher Formate, sichere HTML-/RTF-Aufbereitung, RTF-Tabellen-/Listen-Daten und kontrollierte Medien-Dokumente. Der native Renderer selbst wird in diesen Tests nicht geöffnet. Die JavaScript-Prüfung setzt den JSON-Vorlagenplatzhalter ein und prüft anschließend die Syntax. |
| WLX | Dokumentierte Erkennungsgrammatik mit Prioritäten, Grenzen und Ablehnung ungültiger Regeln. Native Offscreen-Bitmap-Prüfung für ANSI/x86 und Unicode/x64 einschließlich Headergrenze, Pixelreihenfolge und abgelehnter Übergröße. Ein eigener Unterprozess ohne Konsoleneingabe prüft den GUI-nahen Startfall. Der Broker schließt abgeschlossene Eingabepipes; Anmeldedialoge behalten sie bis zum Ende des Austauschs. Miniaturen werden in Batches von höchstens acht Dateien abgefragt. |
| Umbenennen | Masken, Zähler, NFC/Unicode, Schreibweise, bedingte/formatierte WDX-Werte, Vorlagenkompatibilität, relative Ziele, Nummernkollisionen, Zyklen, späte Kollision und Rollback. Umordnen/Entfernen berechnet Zähler neu, ohne die Quellen zu verändern. |
| Vergleich/Synchronisierung | Text-/Binärnavigation und Grenzen, UTF-/Zeilenenden, Schreibschutz bei geänderten Zielen; gleicher Abgleichbaum auf Datenträger, ZIP und 7z. FTP-Referenzen prüfen beide Richtungen, Spiegelung/Löschschutz und Schutz ignorierter Dateien. Ein eigener Testserver bietet SHA-256 an und sperrt normale Downloads: Die Inhaltsprüfung muss HASH nutzen; für eine ausdrücklich abgelehnte Prüfsumme wird der Download-Fallback geprüft. |
| Prüfsummen/Transportformate | Unabhängige hashlib/OpenSSL-/zlib-Vektoren und offizielle BLAKE3-Referenz. TC-CRC-Begleitformat, bekannte UUE-/XXE-Vektoren, MIME und unabhängig erstellte BinHex-Daten-/Ressourcenzweige. Fehlende, beschädigte und doppelte Teile bleiben Fehler. |

Die SDK-Testquellen liegen unter `crates/arco-core/tests/fixtures/wlx-thumbnails` und `wdx-search`. Ihre READMEs beschreiben Build und Umgebungsvariablen. Beide Plugin-Hosts verwenden **Protokoll 13**. Der x86-Host gehört bei der späteren Paketierung ebenfalls in das Windows-Paket.

Der Host meldet den erfolgreichen DLL-Ladevorgang gesondert. Erst dann beginnt das kurze 10-Sekunden-Budget für Miniatur- und WFX-Standardansichtsaufrufe; für diese Aufträge gilt während des Starts das begrenzte 90-Sekunden-Budget. Abbruch wird in beiden Phasen geprüft. Der zusätzliche SDK-Test verzögert das Laden in beiden Architekturen um je 11 Sekunden und verlangt einen erfolgreichen Abschluss. Ein separat um 11 Sekunden verzögerter Vorschauaufruf muss weiterhin das 10-Sekunden-Limit erreichen. Der beobachtete erste Ladevorgang einer frisch gebauten x64-Test-DLL dauerte etwa 10,6 Sekunden; die Diagnose zeigte die Verzögerung innerhalb von `LoadLibrary`, bevor die Plugin-Funktion aufgerufen wurde. Eine konkrete Ursache außerhalb der Anwendung wird daraus nicht abgeleitet.

Die fünf FTP-Referenzen lassen sich in einer Windows-Entwicklershell mit `py -3 scripts/test-ftp-sync-reference.py` wiederholen. Das Skript benötigt `pyftpdlib` im verwendeten Python oder unter `ARCO_LOCAL_FTP_PACKAGES`. Es erstellt temporäre Daten und Server ausschließlich auf `127.0.0.1`, nutzt feste Test-Zugangsdaten und führt nur die benannten Synchronisierungstests aus. Bestehende FTP-Profile und Server werden nicht verwendet. Für Archivtests `ARCO_7ZIP_PATH` auf 7-Zip **26.05 oder neuer** setzen; eine ältere systemweite Installation genügt dem Adapter nicht.

Die Medienmetadaten-Referenz benötigt den vollständigen Pfad zu FFmpeg in `ARCO_FFMPEG_PATH`. FFmpeg wird nur für die Erzeugung unabhängiger Testdaten verwendet; die Anwendung liest deren Tags mit dem Rust-Parser. Es findet keine Wiedergabe statt.

Der HASH-Parser unterstützt die ältere Antwort ohne Bytebereich und die spätere Form mit Bytebereich aus den [FTP-HASH-Entwürfen](https://datatracker.ietf.org/doc/html/draft-bryan-ftpext-hash). Ein Bereich muss bei Byte 0 beginnen und am letzten Byte der erwarteten Größe enden. Tests weisen Teilbereiche, unbekannte Größen, fehlerhafte Zahlen, unpassende Algorithmen und Namen sowie doppelte Prüfsummen zurück. Der lokale Server liefert eine Antwort mit vollständigem Bereich; zusätzlich wird der Abschluss einer mehrzeiligen Antwort geprüft. Die Entwürfe werden damit nicht als verabschiedeter RFC dargestellt.

## Herstellerabgleich und Grenzen

Die deutsche CHM-Hilfe und die Daten eines signierten TC-11.58.1-Pakets wurden gelesen; der Installer und die TC-EXE wurden nicht ausgeführt. Der Datenabgleich dient unter anderem dem internen Feldkatalog. Die Partnersuche wird als eigener Filter angeboten, da sie einen Suchparameter benötigt. Das entspricht der [Beschreibung des TC-Autors](https://ghisler.ch/board/viewtopic.php?p=409879).

Die folgenden Punkte bleiben **Abnahme beziehungsweise Umgebungsnachweis**, nicht als bestanden markiert:

- Bedienung ausschließlich mit Maus beziehungsweise Tastatur, Fokus, DPI, helle/dunkle Darstellung und kleine Fenster.
- Tatsächliche RichEdit-/WebView2-Darstellung, Tabellen/Listen, Mediensteuerung, installierte Codecs und fehlende Windows-Medienkomponenten.
- Native WLX-/andere Plugin-Fenster und Rückkehr zur Anwendung.
- Lister-/Dateilistendruck mit dem ausgewählten Drucker.
- Direkte TC-Gegenläufe mit denselben Profilen, Umbenenn- und UNC-Beispielen sowie Teil-/Kodierdateien.
- Zusätzliche Windows-, Netzwerk-, Datei- und Plugin-Umgebungen außerhalb der beschriebenen Referenzen.

Die Inhaltssuche ist auf 64 MiB pro Datei begrenzt; formatiertes HTML/RTF auf 2 MiB. Laufzeit- und Formatgrenzen sind dokumentierte Schutzgrenzen. Der Bereich-3-Abschluss ist keine Zusicherung, jede TC-Version und jedes Fremdplugin ohne Unterschiede nachzubilden.

Quellen: [Offizielle deutsche Hilfe](https://www.totalcommander.ch/help/tcmdd1158_chm.zip), [WLX-SDK](https://www.ghisler.com/plugins.htm), [Windows-Regionseinstellungen](https://learn.microsoft.com/en-us/windows/win32/api/winnls/nf-winnls-getlocaleinfoex), [Lofty-Metadatenparser](https://docs.rs/lofty/0.25.5/lofty/), [BLAKE3-Testvektoren](https://github.com/BLAKE3-team/BLAKE3/blob/master/test_vectors/test_vectors.json).
