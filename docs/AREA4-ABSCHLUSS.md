# Bereich 4 – Implementierung und Nachweise

Stand: Version 1.5.0 vom 17. November 2026, aufbauend auf Version 1.4.0. Die Archivimplementierung wurde am 16. November abgeschlossen; der Release-Quellstand ist über Tag `v1.5.0` nachvollziehbar.

**ARC-01 bis ARC-07 sind im definierten Matrixumfang implementiert: 7/7 IDs, 100 %.** Das ist eine Implementierungsquote mit den [dokumentierten Format-/Backendgrenzen](ARCHIVE.md). Sie behauptet keine uneingeschränkte Gleichheit mit allen TC-Versionen, proprietären Zusatzdaten, historischen Codecvarianten oder fremden Plugins. Fenster-, direkte TC- und umgebungsabhängige Originalencoderabnahme bleiben separat offen.

## Automatisierte Prüfung

**351 Tests bestanden, 49 ausdrücklich ignorierte Umgebungsfälle im Standardlauf, sechs Testsuiten. Zusätzlich 16 Archiv-Referenztests bestanden.** Diese 16 gehören zu den im Standardlauf ignorierten Fällen; ihre zusätzlichen Parameter-/Korpusvarianten werden nicht als weitere Unit-Tests mitgezählt. Die übrigen 33 Umgebungsfälle gelten nicht als bestanden.

**Formatierung, statischer Check aller Workspace-Ziele und normaler Windows-Build erfolgreich.** Slint-Warnungen wurden dabei als Fehler behandelt.

Es wurde keine Arco-Instanz gestartet, auch nicht mit `--help`, `--version` oder `--search-worker`. Ebenso wurden weder erzeugte SFX-Dateien noch DOSBox/TC ausgeführt. Die vorhandenen Integrationstests `search_worker` und `zip_sfx` sind deshalb in diesem Arbeitsblock nicht ausgeführt worden. Test-Harnesses, Konsolenpacker/-leser und der fensterlose Plugin-Host sind davon getrennt.

Der Standardlauf umfasst Bibliotheken, Programmmodule und die Windows-Dateisystemreferenzen:

```powershell
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked --offline
cargo test --workspace --lib --bins --test windows_reference --locked --offline -- --test-threads=1
cargo build --workspace --locked --offline
```

Die lokalen Läufe setzen `CARGO_INCREMENTAL=0`, `CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0`, `CARGO_BUILD_JOBS=2` und `SLINT_COMPILER_DENY_WARNINGS=1`. Rust 1.99.1/MSVC wurde verwendet; das Projektminimum bleibt Rust 1.93. Die erzeugten x64-Dateien wurden statisch geprüft: Arco und ZIP-SFX verwenden GUI-Subsystem 2, der Host Konsolensubsystem 3. Keine EXE wurde dafür ausgeführt. Englisch und Französisch enthalten jeweils 1279 eindeutige UI-Schlüssel; alle hinzugefügten Slint-Texte sind übersetzt. Python-/PowerShell-Syntax, Dokumentationslinks und Diff-Leerraumprüfung sind ebenfalls geprüft.

## Referenzumfang

| Gruppe | Nachweis |
| --- | --- |
| ZIP | Native Methoden und AES-Stärken/ZipCrypto, LZMA über die begrenzte Passwortpipe; Brotli-Methode 129 einschließlich AES-Authentifizierung, falschem Passwort und beschädigten Nutzdaten. SFX-Payload, Archivkommentar und Fortschritt werden ohne Ausführung des Ergebnisses geprüft. |
| 7z | LZMA2/LZMA/PPMd/BZIP2, Store, Solid/Einzelblöcke, sichtbare/verschlüsselte Header und Roundtrip. Eigene Thread-/Wörterbuch-/Wort-/Solid-Blockwerte über den Konsolenadapter. |
| Neuaufbau | TAR/TGZ, UC2 und geschütztes ACE ergänzen/umbenennen/löschen; leere Container, unveränderte Quellen bei Konflikten und Erhalt des alten Archivs vor Veröffentlichung. |
| Mehrteiler | ACE-Aufteilung mitten in Dateien, Passwort, fehlende/geänderte Teile, Bearbeitung und überzählige Teile; UC2-SAS-Einstieg/-Neubau; vollständige externe ZIP-Teilesätze mit geschützten Zielkollisionen. Vorhandene ZIP-/7z-Referenzläufe prüfen zusätzlich Rohteiler und SFX-Daten. |
| Packer | LHA32 aus der vom TC-Hersteller verlinkten Distribution, getrennte Quellen-/Listenargumente und externe ZIP-/7z-Profile. Ausgabeinhalt und Verschlüsselung müssen zur Auswahl passen. Die zwei zusätzlichen ACE-Encoderläufe prüfen Blocked-Delta/LZ77 und Store-Fallback. |
| Herkunft/WCX | Bytegenaue NTFS-ADS-Übernahme, strengste Zone aus Folgeteilen und explizites Weglassen. Die eigene Unicode-WCX-DLL prüft den tatsächlichen x64-Host-Aufruf einschließlich beider Zonenregeln. Quelle: `crates/arco-core/tests/fixtures/archive_zone_wcx.c`; keine Plugin-Fenster. |
| Einstellungen/Import | Alte Profile ohne Archivabschnitt, persistente Optionen, UI-Konvertierung und atomare Validierung. TC-Pfade, Methoden, Raten und Einheiten aktiver 7z-Vorgaben; neue Oberflächentexte in Englisch/Französisch. |

Der unabhängige Lesekorpus enthält **32 Quelldateien und 23 Prüffälle**: 16 erfolgreiche Formatfälle mit 57 Dateinutzlasten sowie sieben explizite Ablehnungen von Verknüpfungs-/unsicheren Pfadfällen. Hinzu kommen die zwei realen ACE-Archive innerhalb der 32 Quellen; diese werden vollständig über ihre Formatprüfsummen geprüft und entpackt. Alle Downloads haben Herkunft und SHA-256 im erzeugten Manifest. Die erwarteten Nutzlasthashes stammen von 7-Zip beziehungsweise Windows-bsdtar, nicht von Arcos eigener Ausgabe.

Fest referenzierte Quellen:

- [libarchive-Testdaten](https://github.com/libarchive/libarchive/tree/a94150bafc7b5e91f6394d160486f4988a99e79b/libarchive/test), Commit `a94150bafc7b5e91f6394d160486f4988a99e79b`.
- [acefile-Testdaten](https://github.com/droe/acefile-testdata/tree/eb3ee7b1541a5d641fdfc115df06c3ba478e09e0), Commit `eb3ee7b1541a5d641fdfc115df06c3ba478e09e0`.
- [Ghisler-Packerübersicht](https://www.ghisler.com/tools.htm), [LHA32-Referenzpaket](https://plugins.ghisler.com/plugins/lha32.zip), SHA-256 `3427f78e8a640eb98b193922f44075c02f085a5193ef200783b1a5adec228b6c`.
- Deutsche TC-11.58.1-Hilfe, insbesondere `dlg_pack`, `dlg_configpack` und `inisettings3`; [Hersteller-FAQ](https://www.ghisler.com/efaqpack.htm).

Der Korpus hat zwei tatsächliche Lücken des Konsolenlesers sichtbar gemacht: LHA-Header 3 und CAB-LZX mit 16-Bit-Sonderfall. Die ergänzenden Rust-Leser schließen diese Fälle. Intel-E8-CAB bleibt beim bewährten Konsolenpfad, da die verwendete Rust-LZX-Bibliothek bei einer E8-Nulladresse abweichende Bytes liefert. Der Regressionstest prüft beide Wege anhand unabhängiger Nutzlasthashes.

Neue Reader-Abhängigkeiten: `cab 0.6.0` (MIT), `delharc 0.7.0` (MIT oder Apache-2.0) und transitiv `lzxd 0.2.7` (MIT). Der Release-Paketierer erfasst ihre Lizenzen wie die übrigen Cargo-Abhängigkeiten. Der reine Referenzpacker LHA32 wird durch diesen Arbeitsblock nicht zum Programmpaket hinzugefügt.

## Referenzlauf wiederholen

In einer x64 Developer PowerShell mit Rust und den vorbereiteten Konsolen-/Python-Adaptern:

```powershell
./scripts/test-archive-references.ps1 -PrepareCorpus -Offline
```

`-PrepareCorpus` lädt ausschließlich die fest referenzierten Testdaten nach `target/area4/corpus`. Ohne den Schalter bleiben die vorhandenen Daten erhalten. Toolpfade lassen sich mit `-SevenZip`, `-ArchivePython` und `-Lha` übergeben. `-Offline` betrifft Cargo; die erstmalige Korpusvorbereitung benötigt Netzwerkzugriff.

Für den zusätzlichen WCX-Grenztest zunächst die mitgelieferte Test-DLL bauen und dann den Runner ergänzen:

```powershell
cl /nologo /LD /W4 /WX /O2 /Fetarget\area4\tools\zone.wcx64 /Fotarget\area4\tools\zone.obj crates\arco-core\tests\fixtures\archive_zone_wcx.c /link /IMPLIB:target\area4\tools\zone.lib
./scripts/test-archive-references.ps1 -Offline -WcxPlugin ./target/area4/tools/zone.wcx64
```

Die Ausgabeordner müssen vorher existieren. Der Test benötigt NTFS und den passenden Plugin-Host neben den Testprogrammen oder im normalen Buildverzeichnis. Ohne `-WcxPlugin` laufen 15 Referenztests, mit dem Zusatz 16. Es werden keine Programme aus den Testarchiven gestartet.

## Verbleibende Abnahme

- Der Projektinhaber prüft Fokus, Darstellung, DPI und reine Maus-/Tastaturbedienung anhand der [Fensterprüfliste](UI-PRUEFUNG.md).
- Direkte Gegenläufe in Total Commander sowie aktuelle Original-RAR-/ARJ-/ACE-Schreibencoder und weitere WCX-Plugins bleiben umgebungsabhängige Referenzen. Fehlende Werkzeuge sind keine stillschweigend bestandenen Tests.
- DOS-UC2SEA-Erstellung/-Neubau behält die erforderlichen Originalwerkzeuge und DOSBox-X. Sie wurden hier nicht gestartet. Das Löschen des letzten Eintrags wird wegen der Encodergrenze ohne Änderung des Originals abgewiesen.
- Stromausfall, konkurrierende Fremdprogramme, Netzlaufwerke und sämtliche proprietären Zusatzfelder werden durch die lokalen Transaktionsprüfungen nicht vollständig nachgewiesen.

Version 1.5.0 veröffentlicht diesen Stand; weitere Funktionsbereiche benötigen einen neuen Auftrag. Rückmeldungen aus den Fenstertests haben Vorrang.
