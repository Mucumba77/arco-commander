# Änderungen

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
