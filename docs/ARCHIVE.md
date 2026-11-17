# Archive bedienen

Version 1.5.0 erweitert den Archivbereich gegenüber 1.4.0. Der Quellstand ist über Git-Tag `v1.5.0` nachvollziehbar. Maßgeblich sind ARC-01 bis ARC-07 der [Funktionsmatrix](../FUNKTIONSMATRIX.md). Die [Abschlussprüfung](AREA4-ABSCHLUSS.md) trennt Implementierung, automatisierte Nachweise und manuelle Abnahme.

## Öffnen, kopieren und bearbeiten

Einzelklick markiert, Doppelklick oder Eingabetaste öffnet ein Archiv als Dateifenster. `Strg+Bild ab` betritt unterstützte Archive einschließlich erkannter SFX-Dateien. SFX-Dateien werden zum Lesen nicht ausgeführt. `F3` zeigt eine Datei, `F5` kopiert die Auswahl in das andere Fenster. Zwei Archivfenster können so Dateien über einen geprüften temporären Zwischenstand austauschen. Der separate Entpackdialog bietet Zielordner und Zielmasken; vorhandene Dateien werden über den gemeinsamen Konfliktdialog behandelt.

Beschreibbare Archive unterstützen Ergänzen, Umbenennen und Löschen. Vorhandene ZIP-/7z-Rohteiler, ACE-Sätze und UC2-SAS-Sätze werden vorübergehend neu aufgebaut und geprüft. Anschließend werden ihre Teile mit Wiederherstellung bei fehlgeschlagener Veröffentlichung ersetzt; überzählige alte Teile werden entfernt. Änderungen der Quelle, fehlende Teile, Schreibschutz, Abbruch und Zielkollisionen verhindern das Ersetzen des bisherigen Satzes. Das ist keine gemeinsame Transaktion mit anderen Programmen und keine Stromausfallsicherung.

TAR/TGZ, UC2 und geschützte beziehungsweise Solid-ACE-Dateien können eine vollständige Dekompression und Neukompression benötigen. Diese braucht zusätzlichen freien Speicher und erhält nicht jedes herstellerspezifische Zusatzfeld oder Recovery-Record. Gesperrte ACE-Archive werden nicht verändert. CAB und klassische datenträgerbasierte ZIP-Sätze (`.z01` … `.zip`) bleiben schreibgeschützt. Das betrifft nicht die beschreibbaren Rohteiler `.zip.001` und `.7z.001`.

## Packauswahl und Hintergrundaufträge

Im Packdialog stehen Format, Archivname und optionales Passwort sowie gemeinsame Auswahloptionen bereit:

- **Pfadnamen mit speichern** erhält die Ordnerstruktur. Ohne diese Option werden Dateinamen flach abgelegt; Kollisionen führen zu einer Meldung.
- **Unterordner rekursiv mitpacken** schließt den gesamten ausgewählten Baum ein. Ohne Rekursion werden keine tieferen Dateien mitgenommen.
- **Basisordner weglassen** legt den Inhalt eines ausgewählten Ordners unmittelbar im Archiv ab.
- **Ein Archiv pro ausgewählter Datei oder Ordner** erzeugt getrennte, vorab auf Kollisionen geprüfte Ziele.
- **Nach erfolgreicher Prüfung ins Archiv verschieben** entfernt die Quellen erst, wenn alle Archive erstellt und erneut geprüft wurden und die Quellen unverändert sind. Diese Option erfordert Rekursion.

Die Optionen gelten auch beim Packen mit externen Profilen. Mehrteiler werden als ein Archivauftrag gezählt. Ein fehlgeschlagener späterer Auftrag lässt bereits fertiggestellte Archive bestehen, entfernt aber keine Quellen. Fortschrittsfenster, Abbruch und Hintergrundbetrieb verwenden die gemeinsamen Dialoge. Bei unbekannter Bytegesamtmenge wird keine erfundene Prozentzahl oder Restzeit angezeigt.

## Archiveinstellungen

**Konfigurieren → Archiveinstellungen** öffnet den Archivbereich des allgemeinen Einstellungsdialogs. Er ist auch über die Befehlspalette erreichbar. Die Werte werden mit dem Profil gespeichert; ältere Profile erhalten kompatible Vorgaben.

| Gruppe | Einstellbare Werte |
| --- | --- |
| ZIP | Store, Deflate, BZIP2, LZMA, PPMd, XZ, Zstandard; Rate 0–9; AES-128/192/256 oder ZipCrypto bei gesetztem Passwort |
| 7z | LZMA2, LZMA, PPMd, BZIP2; Rate 0–9; Solid; Dateinamen verschlüsseln; Threads, Wörterbuch, Wortgröße und Solid-Blockgröße |
| TAR/GZIP | GNU oder POSIX/PAX; GZIP-Rate; gemeinsames TAR+GZIP-Format `.tgz` |
| Prüfung/Datum | ZIP nach Erstellung prüfen; Archivdatum auf die neueste Quelldatei setzen |
| Herkunft | `Zone.Identifier` übernehmen oder weglassen; gilt auch für WCX-Entpacken |

Eigene 7z-Speicher-/Threadwerte verwenden den mitgelieferten 7-Zip-Konsolenadapter. Null wählt dessen Vorgabe. Grenzen werden vor dem Speichern geprüft: bis zu 64 Threads, maximal 128 MiB Wörterbuch, PPMd maximal 64 MiB; methodenabhängige Wort-/Blockwerte. Bei BZIP2 sind 100–900 KiB Wörterbuch und keine eigene Wortgröße vorgesehen. Die Grenze verhindert Archive, deren Wörterbuch der interne Leser nicht innerhalb seines Speicherbudgets lesen könnte. Zusätzliche Werkzeugoptionen lassen sich in externen Packerprofilen angeben.

Ohne Passwort wird keine Verschlüsselung aktiviert. Die gesonderte ZipCrypto-Formatauswahl verlangt ein Passwort; ZipCrypto ist ein Legacy-Verfahren. ACE-Passwörter sind auf 1–50 UTF-8-Bytes begrenzt. Passwörter gehen über begrenzte Standardeingabe-Pipes an Adapter, nicht als ersetzte Profilargumente oder Passwortdateien. Ein externes Profil muss den Passwortmodus seines Werkzeugs unterstützen; ein unverschlüsseltes Ergebnis trotz gesetztem Passwort wird verworfen.

## Formate und Backends

| Format | Lesen/Prüfen/Entpacken | Erstellen/Bearbeiten | Besondere Varianten |
| --- | --- | --- | --- |
| ZIP/ZIP64/JAR/XPI | Rust | Rust; LZMA-Erstellung über 7-Zip | Unicode, ZipCrypto, AES, Rohteiler, ZIP-SFX; klassische `.zNN`-Sätze lesen |
| 7z | Rust | Rust; eigene Speicher-/Threadwerte über 7-Zip | Solid, AES und Header-Schutz, Rohteiler; 7z-SFX mit mitgeliefertem Modul |
| TAR/TGZ/GZIP | 7-Zip; TGZ als innerer TAR-Baum | TAR/TGZ/GZIP erstellen; TAR/TGZ vollständig neu aufbauen | GNU/POSIX, große GZIP-Längen werden durch begrenztes Lesen verifiziert |
| BZIP2/XZ/Zstandard/Brotli | 7-Zip beziehungsweise Rust-Brotli | Entsprechend dem angebotenen Format/externen Profil | Namenslose Streams, leere Streams; Brotli auch als ZIP-Methode 129 mit ZipCrypto/AES |
| RAR | 7-Zip | Passendes externes RAR-Profil | RAR4/RAR5, Solid, verschlüsselte Daten/Header, alte und neue Teilenamen, SFX lesen |
| ARJ | 7-Zip | Passendes externes ARJ-Profil | Fortsetzungen `.a01` … und erkannte SFX-Präfixe |
| LHA/LZH | 7-Zip; ergänzender Rust-Leser für Header 3 | Passendes externes LHA-Profil | Header 0–3 und gängige LH-Methoden; LHA32-Verzeichnisse werden als Wildcard übergeben |
| CAB | 7-Zip; ergänzender Rust-Leser für LZX ohne Intel-E8-Transformation | Lesen/Entpacken | Store, MSZIP, LZX und zusammengehörige CAB-Sätze; E8 bleibt beim Konsolenleser |
| ACE | Python/acefile-Adapter | Rust Store/LZ77/Blocked; Verschlüsselungsprimitiv über acefile | Passwort, `.ace/.c00`-Sätze erstellen/ändern, SFX lesen; weitere Encoderoptionen über externes Profil |
| UC2 | Python-Adapter | Rust/Python; SAS-Sätze erstellen/ändern | DOS-UC2SEA-Erstellung/-Neubau mit Originalwerkzeugen und DOSBox-X; keine Ausführung des Ergebnisses erforderlich |
| WCX | Getrennter x86-/x64-Plugin-Host | Nach Fähigkeiten des installierten Plugins | Gemeinsame Mehrfachaufträge und Zonenrichtlinie; keine Garantie für jedes fremde Plugin |

Das Windows-Paket enthält 7-Zip 26.06 einschließlich Lizenz und zugehörigem Quellcode. ACE/UC2 benötigen weiterhin die separat eingerichtete optionale Python-Umgebung; die getesteten Paketversionen und Einrichtungsschritte stehen im [README](../README.md). Beim Entwicklungsbuild können `ARCO_7ZIP_PATH` und `ARCO_ARCHIVE_PYTHON` auf die Werkzeuge zeigen. RAR-/ARJ-/LHA-Packer sind gesondert einzurichten; ihre Lizenzen und Formatfähigkeiten bleiben maßgeblich. Beispielsweise kann ein alter LHA-Encoder leere Ordner auslassen. Arco erkennt abweichende Archiveinträge statt stillschweigend Quellen zu verlieren. Ein leeres DOS-UC2SEA kann der Originalencoder nicht erzeugen; das Entfernen seines letzten Eintrags wird daher vor Änderung des Satzes abgewiesen.

## Externe Profile und TC-Import

Ein Profil enthält eine absolute ausführbare Datei, eine Zielendung und getrennte Argumente. `{archive}` sowie entweder `{sources}` oder `@{list}` müssen jeweils ein eigenes Argument sein. Es wird kein Shell-Kommando aus den Eingaben gebaut. Alle erzeugten Archivteile werden vor Veröffentlichung auf Inhalt, Größe und Lesbarkeit geprüft. Zielkollisionen dürfen keine bereits vorhandenen Teile überschreiben.

Der bestehende Import aus `wincmd.ini` zeigt Packerpfade und erkannte Einstellungen vor Übernahme an. Unterstützt werden ARJ/LHA/RAR/UC2/ACE/ZIP/7z-Pfade, Rate/Methode, Solid, AES-Stärke, Header-Schutz, Prüfung, Datum und TAR-Dialekt. Bei 7z werden Threadzahl und das aktive Methoden-/Ratenprofil für Wörterbuch, Wort- und Solid-Blockgröße übernommen. Positive TC-Wörterbuchwerte bedeuten MiB, negative KiB. Die übrigen TC-Ratenprofile werden nicht als umschaltbare Arco-Vorlagensammlung gespeichert. Vorhandene gleichnamige Arco-Packerprofile werden nicht ungefragt ersetzt.

## Grenzen und Abnahme

Links, NTFS-Zielstreams und unsichere oder kollidierende Archivpfade werden vor Veröffentlichung abgewiesen. Die allgemeine Extraktionsgrenze beträgt 256 GiB je Auswahl, die Vorschau höchstens 64 MiB. Archivweitergaben an externe Werkzeuge laufen mit Quellkontrollen, Größenlimits und Abbruch; das ersetzt keine universelle Isolation beliebiger Fremdprogramme.

Die Implementierungsquote der Matrix ist keine Aussage über jede historische Codecvariante oder jede Version eines externen Packers. Fensterbedienung, direkte TC-Gegenläufe und nicht verfügbare Originalencoder bleiben als manuelle beziehungsweise umgebungsabhängige Abnahme benannt. Siehe [Fensterprüfliste](UI-PRUEFUNG.md).
