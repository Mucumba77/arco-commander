# Änderungen

## 1.3.0 · 16. November 2026

- Erweiterte Suchkriterien mit relativen Altersgrenzen, Uhrzeiten, Größeneinheiten, ganzen Wörtern, inverser Inhaltssuche, gemischten Hex-/Textbytes, DOS-Kodierungen und Office-/EPUB-Text.
- Pfad- und Traverse-Filter, `@Dateilisten`, Partnerdateien anderer Erweiterung, Suchverlauf und F2-Nachsuche im Hauptdialog sowie im separaten Suchprozess.
- Übergreifende Archivsuche mit getrennter Herkunft, Passwortunterstützung und sicherer Trefferübernahme für einzelne beziehungsweise gemischte Ursprünge.
- Bzip2-/XZ-Streams ohne gespeicherten Mitgliedsnamen lassen sich auflisten und durchsuchen. Fehlende Größen werden begrenzt und abbrechbar ermittelt.
- Eigener Duplikatauswahldialog mit Ordner-, Namens-, Suchprofil- und WDX-Filtern sowie priorisiertem Erhalt einer Kopie. Zusätzliche interne TC-Dateifelder einschließlich Windows- und Medienmetadaten.
- Native schreibgeschützte RichEdit-RTF-Darstellung und zusätzliche Windows-Medienformate über optionales Media Foundation; sichtbare Fehler und erreichbare Mediensteuerung.
- Vollständige dokumentierte WLX-Erkennungsgrammatik, geordnete Vorschau-Fallbacks und begrenzte Miniatur-Batches. Plugin-Protokoll **13** erfordert den Neubau beider Hosts.
- Korrigierter Plugin-Broker für Prozesse ohne Konsoleneingabe: abgeschlossene Anfragen schließen ihre Eingabepipe, damit der Host zuverlässig beendet wird. Interaktive Anmeldungen bleiben während des Austauschs möglich.
- Getrennte Zeitbudgets für das Laden einer Plugin-DLL und kurze SDK-Aufrufe. Eine Startmeldung beginnt das 10-Sekunden-Limit für Miniaturen und WFX-Standardansichten erst nach dem Laden; Abbruch bleibt auch während des Ladens möglich.
- Erweiterte Umbenennmasken, globale Zähler, NFC, englische Titelschreibung, bedingte und formatierte WDX-Felder, relative Zielunterordner, automatische Kollisionsnummern sowie Umordnen/Entfernen in der Vorschau mit neu berechneten Zählern.
- FTP-Inhaltsvergleich verarbeitet HASH-Antworten mit und ohne Bytebereich sowie mehrzeilige Antworten. Teilbereiche, unpassende Dateinamen oder Prüfsummen werden nicht als vollständiger Inhaltsnachweis verwendet.
- Zusätzliche unabhängige Referenzdaten für Archive, Synchronisierung, Metadaten, Prüfsummen und Teil-/Kodierformate; reproduzierbare SDK-Testquellen und ergänzte englische/französische Übersetzungen. Fenstertests bleiben beim Projektinhaber.

### Veröffentlichung und Prüfung

- Bereich 3 ist im definierten Matrixumfang TOOL-01–TOOL-13 implementiert: 13/13 IDs. Fenster-, Codec-, Drucker- und direkte TC-Gegenprüfungen werden separat geführt.
- 310 automatisierte Tests in acht Suiten bestanden; 38 explizite Umgebungsfälle im Standardlauf ausgelassen. Zusätzlich 16 erfolgreiche Referenz-Testläufe für SDK-Plugins, Archive, Medienmetadaten und lokalen FTP-Abgleich.
- Portables Windows-x64-Paket mit beiden Plugin-Hosts (Protokoll 13), ZIP-SFX, neuer Werkzeuganleitung, Abschlussnachweis und Drittanbieterhinweisen.
- Paketierung mit `--no-app-run`: kein Start von Arco Commander, auch nicht für `--version` oder `--help`. Fenster- und Windows-10-Abnahme bleiben beim Projektinhaber.

## 1.2.0 · 15. November 2026

### Dialoge und Bedienung

- Einheitliche Datei- und Binärvergleiche mit Dateiinformationen, Unterschiedsnavigation, Fortschrittsphasen und Schutz ungespeicherter Bearbeitungen.
- Einfg markiert den aktuellen Eintrag und springt zum nächsten; Leertaste markiert ohne Weitergehen. Eigene Tastenkürzel bleiben erhalten.
- Gegliederte Dateieigenschaften, Attribute, Zeitstempel und FTP-Berechtigungen mit kopierbaren Angaben, Validierung und Fortschritt.
- Gemeinsame Formulare und Ergebnisse für Prüfsummen, Teilen/Zusammenfügen und Kodieren/Dekodieren. Abbrüche und Veröffentlichungskollisionen schützen vorhandene Dateien.
- Filterbare Verwaltung und strukturierte Editoren für Tabs, Favoriten, Buttonbars, Starter, Spaltensätze, Ansichtsregeln und Dateizuordnungen. Entwürfe bleiben bei korrigierbaren Fehlern erhalten; geänderte Eingaben werden vor dem Verwerfen geschützt.
- Gemeinsame Hinweise, Fehler und Auftragsberichte mit Zählern und vollständigen kopierbaren Details. Das letzte Ergebnis ist über Statusleiste und Hilfemenü erneut erreichbar; Hintergrundmeldungen ersetzen keinen geöffneten Eingabedialog.
- Überarbeitete Laufwerksauswahl, Kommentare, Ignorierliste, Packerprofile und Umbenennvorlagen. Feste Aktionsleisten, scrollbare Inhalte und ergänzte Fokusführung in Such-/Listerfenstern.
- Ergänzte englische und französische Übersetzungen, Tastaturhinweise und Symbole.

### Veröffentlichung und Prüfung

- Portables Paket für Windows 10 22H2 und Windows 11 (x64), einschließlich x64-/x86-Plugin-Hosts, ZIP-SFX, Anleitung und Drittanbieterhinweisen.
- 271 automatisierte Tests bestanden; 33 externe Referenzfälle übersprungen. Formatierung und statische UI-Kompilierung geprüft.
- Paketierung mit `--no-app-run`: Arco Commander wird auch nicht für Versions-/Hilfsaufrufe gestartet. Eingebettete Version, PE-Architektur, Subsystem, ZIP-Integrität, beide Plugin-Hosts und ZIP-SFX werden separat geprüft.
- Fenster-, DPI-, reine Maus-/Tastatur- und Windows-10-Prüfungen übernimmt der Projektinhaber anhand der erweiterten [UI-Prüfliste](docs/UI-PRUEFUNG.md). Der volle Funktionsumfang von Total Commander bleibt ein Entwicklungsziel; die offenen Matrixfälle sind unverändert ausgewiesen.

## 1.1.0 · 12. November 2026

### Oberfläche und Bedienung

- Einheitliche Dialoge, Schaltflächen und sichtbarer Tastaturfokus. Dateien und Ordner öffnen per Doppelklick oder Enter; ein Einzelklick wählt aus.
- Gemeinsame Fortschrittsfenster für Kopieren, Verschieben, Löschen, Packen, Entpacken, Suche, Umbenennen, Synchronisieren, FTP und Plugin-Aufträge. Messbare Transfers zeigen Durchsatz, Datenmenge und geschätzte Restzeit; unbekannte Gesamtmengen bleiben als Aktivität erkennbar.
- Gegliederte Suchkriterien, laufende Suchzähler und Ergebnislisten mit Maus- und Tastaturnavigation. Das separate Suchfenster empfängt gebündelte Fortschrittsmeldungen seines Suchprozesses.
- Umbenennvorschau mit getrennten Spalten und Rückkehr zu den bisherigen Eingaben. Abbrechen berücksichtigt die Vorbereitung, Namensübernahme und Wiederherstellung bereits geänderter Namen.
- Synchronisationsvorschau mit klaren Kopierrichtungen, gesonderter Löschfreigabe und Zählern für bearbeitete und ausstehende Aktionen.
- Gegliederte FTP-Profile mit **Speichern & verbinden**, Fortschritt beim Verbindungsaufbau und beim Download sowie direkten Aktionen für lokale Arbeitskopien. Verbindungen überschreiben keine inzwischen gewechselte Ansicht.
- Filterbare Plugin-Verwaltung mit separatem Detailbereich, Paketfortschritt und strukturierten Importvorschauen.
- Gegliederte Einstellungen mit Änderungsstatus, gezielter Fehlernavigation, Übernahme normalisierter Werte und Kategorienwechsel über `Strg+Bild auf/ab`.

### Prüfung und Fortsetzung

- Lokal: 233 automatisierte Tests bestanden, 33 externe Referenzfälle übersprungen. Zusätzlich wurden Downloadfortschritt, Abbruch und FTP-Rückübertragungen gegen einen lokalen Testserver geprüft.
- Die Fensterbedienung, Skalierung und der manuelle Windows-10-Test sind gesondert zu prüfen. Die Anwendung wurde für diese Arbeiten nicht visuell gestartet.
- [UI-Prüfliste](docs/UI-PRUEFUNG.md) und [nächster Arbeitsblock](docs/WEITERARBEIT.md) liegen im Repository und im Downloadpaket. Die offenen Paritätsfälle der Funktionsmatrix bleiben ausgewiesen.

## 1.0.0 · 10. November 2026

Erste reguläre Veröffentlichung als portables Windows-x64-Paket. Die vollständige Total-Commander-Parität bleibt ein Entwicklungsziel. Die [Funktionsmatrix](FUNKTIONSMATRIX.md) dokumentiert den Stand und die offenen Varianten.

### Oberfläche und Bedienung

- Gruppierte Menüs mit Symbolen und den aktuell belegten Tastenkürzeln. Die Befehlspalette filtert nach Befehlsnamen, Tastenkürzeln und unterstützten `cm_`-Aliasen.
- Zentrale Einstellungen für Sprache, Farbschema, Layout, Tastatur, Schrift, Dateiübertragung und externe Programme.
- Kompakte Eingabe- und Bestätigungsdialoge mit passenden Aktionsnamen. FTP-Profile und Mehrfach-Umbenennen haben feste Feldbeschriftungen.
- Einheitliche Text- und Hex-Werkzeuge für interne Vorschau und Lister. Das separate Suchfenster zeigt laufende Suchvorgänge an und übernimmt das gewählte Farbschema.

### Dateioperationen und Distribution

- Fortschrittsanzeige für Kopieren, Verschieben und Archivaufträge. Der Transfermanager zeigt laufende und wartende Aufträge getrennt an.
- Kopier-, Verschiebe- und Packdialoge zeigen Quelle und Ziel vor dem Start. Bei regulärem ZIP-Pack- und Entpackvorgang zeigt die Fortschrittsanzeige den Stand anhand der unkomprimierten Daten.
- Die Versionsnummer ist im Hilfemenü und über `--version` abrufbar. Das Downloadpaket enthält die x64- und x86-Plugin-Hosts, das ZIP-SFX-Modul, eine Kurzanleitung und Drittanbieterhinweise.
