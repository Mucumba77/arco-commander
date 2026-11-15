# Änderungen

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
