# Zu Arco Commander beitragen

Beiträge auf Deutsch und Englisch sind willkommen. Kleine Aufgaben findest du unter [good first issue](https://github.com/Mucumba77/arco-commander/labels/good%20first%20issue). Die [Funktionsmatrix](FUNKTIONSMATRIX.md) beschreibt den Umfang und die Prüffälle der Total-Commander-Kompatibilität.

## Fehler melden und Ideen vorschlagen

Verwende die [Issue-Vorlagen](https://github.com/Mucumba77/arco-commander/issues/new/choose). Für einen Fehler helfen die Arco-Version, der Windows-Build, die betroffene Aktion und kurze reproduzierbare Schritte. Verwende für Dateibeispiele Testdaten. Entferne Zugangsdaten, persönliche Pfade und private Dateinamen aus Anhängen. Sicherheitsprobleme meldest du über den [privaten Meldeweg](SECURITY.md).

## Unter Windows entwickeln

Installiere Rust ab Version 1.93, die MSVC-C++-Buildtools und das Windows SDK. Öffne eine Developer PowerShell. Der [README-Abschnitt zum Build](README.md#bauen-und-starten) erklärt auch den Host für 32-Bit-Plugins.

```powershell
cargo fmt --all --check
cargo build --locked --workspace
cargo test --locked --workspace --all-targets -- --test-threads=1
```

Die Windows-CI führt diese Prüfungen auf einem x64-Runner aus. Als ignoriert markierte Referenztests benötigen je nach Fall Zusatzprogramme, Test-Plugins, Server oder besondere Windows-Berechtigungen. Für Änderungen an der Bedienung beschreibe die geprüften Schritte mit Maus und Tastatur sowie die verwendete Windows-Version.

## Pull Request öffnen

1. Erstelle einen Branch in deinem Fork auf Basis des aktuellen `main`.
2. Beschränke den Beitrag auf eine Änderung und verknüpfe das zugehörige Issue, falls eines existiert.
3. Beschreibe, was sich für Nutzer ändert und wie du es geprüft hast. Aktualisiere bei Änderungen am Funktionsumfang die passende Zeile der Funktionsmatrix.
4. Öffne den Pull Request gegen `main` und prüfe das Ergebnis der Windows-CI.

Eigene Codebeiträge werden unter der [MIT-Lizenz](LICENSE) des Projekts bereitgestellt. Gib die Herkunft und Lizenz von übernommenem Code oder Grafiken an.
