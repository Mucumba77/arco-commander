# Fortsetzung nach Arco Commander 1.1.0

Stand: 12. November 2026. Dieses Dokument hält den nächsten Arbeitsblock über Sitzungen hinweg fest.

## Nächster Schritt

1. **Rückmeldungen aus den Fenstertests haben Vorrang.** Der Projektinhaber prüft Version 1.1.0 anhand von [UI-PRUEFUNG.md](UI-PRUEFUNG.md). Gemeldete Bedienungs-, Fokus-, Darstellungs- und Fortschrittsfehler zuerst beheben.
2. **Die verbleibenden Werkzeugdialoge vereinheitlichen.** Als nächster Bereich sind Datei- und Binärvergleich, Dateieigenschaften beziehungsweise Attribute und Berechtigungen sowie Prüfsummen, Teilen/Zusammenfügen und Kodieren/Dekodieren vorgesehen. Vor Änderungen die vorhandenen Dialoge und Arbeitsabläufe im Quelltext inventarisieren.
3. **Danach die übrigen Konfigurationsabläufe prüfen.** Dazu gehören Tabs, Favoriten, Buttonbars, Starter, Ansichtsregeln, Spaltensätze und Dateizuordnungen. Kleine Parameter gehören nach Möglichkeit in passende Einstellungsseiten; Arbeitsabläufe erhalten klare eigene Dialoge.
4. **Erst anschließend die Funktionsmatrix weiter ausbauen.** Die bisherigen offenen Paritäts- und Referenzfälle werden durch die Überarbeitung der Oberfläche nicht automatisch abgeschlossen.

## Fortgeltende Vorgaben

- **Die Anwendung nicht selbst starten.** Keine Fenster öffnen und keine visuellen Fenstertests durchführen. Diese übernimmt weiterhin der Projektinhaber. Build, statische UI-Prüfung und automatisierte Tests ohne Oberfläche sind zulässig.
- Alle Aktionen müssen mit Maus und Tastatur erreichbar sein. Einzelklick wählt aus; Doppelklick oder Enter öffnet Dateien und Verzeichnisse.
- Gemeinsame Dialogbausteine aus `ui/dialog-theme.slint` und `ui/operation-dialogs.slint` verwenden: Überschrift, gegliederte Felder, feste Aktionsleiste, sichtbarer Fokus und erreichbare Abbruchaktion.
- Lange Aufträge laufen außerhalb des UI-Threads und verwenden das gemeinsame Fortschrittsfenster. Gesamtfortschritt, Schritte, Pfad, Durchsatz und Restzeit nur anzeigen, wenn die zugrunde liegenden Werte vorliegen. Unbekannte Gesamtmengen erhalten eine Aktivitätsanzeige.
- Löschfreigaben, Konfliktentscheidungen, Abbruch und gegebenenfalls Wiederherstellung müssen nachvollziehbar bleiben. Gespeicherte Eingaben bei einer Rückkehr zur Vorschau erhalten.
- Bestehende Änderungen bewahren. Die allgemeine Oberfläche wird vor weiterem Funktionsausbau abgeschlossen.

## Bereits überarbeitet

- Dateioperationen: Kopieren, Verschieben, Löschen, Packen und Entpacken mit gemeinsamen Dialogen und Fortschritt.
- Suche, separates Suchfenster, Mehrfachumbenennen und Synchronisieren mit gegliederten Formularen, strukturierten Vorschauen und Fortschrittsphasen.
- FTP-Profile, erste Verzeichnisliste beim Verbindungsaufbau, Download, Arbeitskopien und Rückübertragungen.
- Plugin-Verwaltung mit Filter, Detailbereich, Paketvorbereitung und Importvorschauen.
- Zentrale Einstellungen mit Kategorien, Änderungsstatus und gezielter Fehlernavigation.

## Technischer Prüfstand vor der Veröffentlichung

- Workspace-Build erfolgreich; Formatierung und statische UI-Prüfung ohne Beanstandung.
- 233 automatisierte Tests bestanden, 33 externe Referenzfälle übersprungen.
- Zwei zusätzliche lokale FTP-Referenztests bestanden: Downloadfortschritt mit Abbruch ohne veröffentlichte Zieldatei sowie wiederholte Rückübertragung mit Erkennung serverseitiger Inhaltskonflikte.
- Manuelle Fenster-, DPI-, Narrator- und Windows-10-Prüfungen sind damit nicht nachgewiesen. Die Screenshots im README stammen aus Version 1.0.0 und wurden nicht durch neue Fensteraufrufe aktualisiert.
