# Suche und Werkzeuge

Diese Anleitung beschreibt Version 1.3.0. Die Funktionstasten können in den Einstellungen umbelegt werden.

## Dateien und Archive durchsuchen

Öffne die erweiterte Dateisuche über das Menü oder die Befehlspalette. Die Suche ist auch in einem separaten Fenster verfügbar. Namen, Inhalt, Größe, Datum, Alter, Attribute, Tiefe und WDX-Suchprofile lassen sich kombinieren. Gespeicherte Suchprofile enthalten die Kriterien; Archivpasswörter werden nicht im Profil gespeichert.

- **Namen:** Mehrere Masken beispielsweise `*.txt;*.md | *.bak`. `*.*` findet auch Dateien ohne Erweiterung. `~` leitet einen regulären Ausdruck ein. Pfadmasken können am Suchort beginnen, etwa `\docs\*.txt`. `*.ini | windows\` lässt den ausgeschlossenen Verzeichniszweig aus. Für diese Muster gelten Windows-Pfadtrennzeichen.
- **Inhalt:** Text, regulärer Ausdruck oder Hex-Bytes. Eine Hex-Suche kann Textbytes enthalten, zum Beispiel `00 "Arco" 0D 0A`. Ganze Wörter und „Text nicht enthalten“ sind eigene Optionen. Die DOS-Kodierungen 850 und 437 stehen neben UTF und Windows-1252 zur Verfügung. „Alle Kodierungen“ prüft die angebotenen Textkodierungen.
- **Office und EPUB:** Die zusätzliche Option durchsucht XML-/HTML-Inhalt in den ZIP-Containern, ohne Dokumente in einem Office-Programm zu öffnen.
- **Partnerdatei:** „Partnerdatei mit anderer Erweiterung“ findet etwa `*.cr2` mit gleichnamiger JPG-Datei im selben Ordner. Mehrere Erweiterungen wie `jpg;png` sind möglich. „Partnerdatei fehlt“ kehrt die Bedingung um. In Archiven wird der Katalog geprüft, ohne die Partnerdatei zu entpacken. Der Filter ist in Suchprofilen gespeichert und folgt der vom [TC-Autor beschriebenen Partnersuche](https://ghisler.ch/board/viewtopic.php?p=409879).
- **Grenzen:** Größen dürfen Einheiten enthalten, beispielsweise `0,1 GiB`. Datumsgrenzen unterstützen Uhrzeiten. Relative Altersangaben sind etwa `7 d`, `12 h` oder `2 w`. Attribute verwenden Regeln wie `+r -hs`; zusätzliche Kennzeichen stehen für Verzeichnisse, Komprimierung, Verschlüsselung, Offline und temporäre Dateien.
- **Suchorte:** Mehrere Pfade mit Semikolon oder Zeilenumbruch trennen. Anführungszeichen erhalten Semikolons innerhalb eines Pfades. `@C:\Listen\Suche.txt` liest eine UTF-8-, UTF-16- oder Windows-1252-Dateiliste. Relative Einträge beziehen sich auf deren Ordner. Auch Dateien, Verzeichnisse, Wildcards und mit `<` eingeleitete reguläre Ausdrücke sind in Dateilisten möglich. Tiefe `0` prüft dort nur ausdrücklich genannte Einträge.
- **Archive:** „Auch Dateien in Archiven durchsuchen“ erweitert lokale Suchläufe. Die Ergebnisliste zeigt Container und Mitglied getrennt als `Archiv::Eintrag`. Unterstützt werden die vorhandenen lesbaren Archivadapter; einige Formate benötigen den eingerichteten externen Packer oder ein Passwort. Fehler eines einzelnen Archivs werden als Lesefehler angezeigt. Die übrigen Treffer bleiben verfügbar.

**Treffer übernehmen:** Stammt das gesamte Ergebnis aus einem Archiv, öffnet Arco die passenden Einträge in einem neuen Archiv-Tab. Bei mehreren Ursprüngen werden lokale Dateien und passende Archivdateien übernommen. Damit arbeiten Öffnen, Kopieren, Verschieben und Löschen mit dem richtigen Anbieter. Dieses Verhalten folgt der Herstellerbeschreibung zur Trefferübernahme aus Archiven.

**Suchverlauf:** Vorherige und nächste Ergebnisse bleiben im Arbeitsspeicher verfügbar. „Andere Suchergebnisse verwerfen“ behält das aktuell angezeigte Ergebnis. Es werden höchstens acht Ergebnisse beziehungsweise insgesamt 250.000 ältere Einträge vorgehalten; ein einzelnes größeres aktuelles Ergebnis bleibt verfügbar. Der Verlauf wird beim Beenden verworfen.

**F2 – Treffer durchsuchen:** Beginnt eine Nachsuche in den vorherigen Treffern. Bei Tiefe `0` werden nur diese Einträge geprüft; die Namensmaske wird dabei ignoriert. Bei höherer Tiefe werden auch die gefundenen Verzeichnisse durchsucht. Archivmitglieder behalten ihre Containerzuordnung. „Kriterien bearbeiten“ öffnet die Kriterien der gewählten Suche erneut.

Everything bleibt optional: `ev:` fragt den gesamten Index ab, `ed:` die angegebenen Suchorte. Der Index sucht keine Inhalte innerhalb von Archivcontainern. Die zusätzliche Archivsuche ermittelt Container deshalb in den lokalen Suchorten. Einfache Suchausdrücke können bei fehlendem Index auf die interne Suche zurückfallen.

## Duplikate auswählen

Die Duplikatsuche kann Name, Größe, Inhalt und interne beziehungsweise WDX-Dateifelder vergleichen. Nach dem Gruppieren öffnet sich der Auswahldialog; über Ziffernblock-Plus lässt er sich für übernommene Gruppen erneut öffnen.

Filter für Namen, Erweiterungen, reguläre Ausdrücke, gespeicherte Suchprofile, Ordner und Unterordner lassen sich mit Größen- und Zeitregeln kombinieren. „Eine Kopie behalten“ wertet bis zu drei Prioritäten aus. Die Vorschau zeigt die Markierung, bevor sie in das Dateifenster übernommen wird. Noch nicht aktualisierte Filter werden vor der Übernahme neu geprüft.

Die internen `tc`-Dateifelder umfassen unter anderem Namen und Pfade, Größe und Zeiten, Attribute, Kommentar, Windows-Zoneninformationen, DOS-Namen, Versionsressourcen und die Medienfelder Tag Present, Title, Artist, Album, Release Year, Description, Track Nr und Genre. `tc.name` ist der Name ohne Erweiterung; `tc.fullname` enthält die Erweiterung. Medienmetadaten werden als Daten gelesen, ohne Wiedergabe oder Programmstart. Die zusätzliche TC-Partnersuche hat einen eigenen Suchfilter, da sie einen Suchwert benötigt und kein gewöhnliches Metadatenfeld ist.

## Mehrfach umbenennen

Masken für Namen und Erweiterung, Suchen/Ersetzen, abschließende Schreibweise und globale Zähler stehen im selben Dialog. Die Vorschau ist vor dem Ausführen maßgeblich.

| Beispiel | Bedeutung |
| --- | --- |
| `[B0]`, `[B1]` | Eltern- beziehungsweise Großelternverzeichnis |
| `[B+0]`, `[B+1]` | Laufwerkspräfix beziehungsweise erstes Verzeichnis von links |
| `[B+1_2-5]` | Zeichenbereich eines Verzeichnisteils |
| `[N2-5]` | Zeichenbereich des bisherigen Namens |
| `[C]` | Laufender Zähler; Start, Schritt, Breite und Wiederholung stehen in eigenen Feldern |
| `[c]`, `[c2]`, `[c3]` | Anzahl der Einträge, Anzahl mit Zählerbreite beziehungsweise letzter Zählerwert; Kleinschreibung beachten |
| `[=tc.size._d3,]` | Internes Größenfeld mit ausdrücklich gesetzter Zifferngruppierung |
| `[=tc.path length]` | Länge des vollständigen Pfades in UTF-16-Zeicheneinheiten |
| `[=tc.decode html characters (utf-8)]` | Prozentkodierte Zeichen im bisherigen Basisnamen dekodieren; außerdem sind Text/ANSI und UTF-16 verfügbar |
| `[=tc.random number]` | Zufallszahl von 0 bis 99999, ohne Eindeutigkeitsgarantie |
| `[=Plugin.Feld._X2]` | Numerisches Plugin-Feld in Hex-Gruppen |

WDX-Platzhalter unterstützen bedingte Präfixe/Suffixe, Zeichenbereiche und typabhängige Zahlen-/Datumsformate. Dezimale Zifferngruppen verwenden ohne ausdrücklich angegebenes Trennzeichen die Windows-Regionseinstellung. NFC und englische Titelschreibung sind verfügbar. Relative Zielunterordner werden angezeigt und erst bei der Ausführung angelegt.

Der interne Katalog enthält auch `case sensitive name` und `case sensitive in folder`. Windows wird dafür nur lesend abgefragt. Der zweite Wert ist bei Dateien falsch; der erste richtet sich nach dem übergeordneten Verzeichnis. Dateinamen-Dekodierung arbeitet ohne Dateiänderung; ungültige UTF-Sequenzen werden gemeldet. Vor einer Umbenennung gelten weiterhin die Zielpfad- und Namensprüfungen.

**Reihenfolge:** In der Vorschau einen Eintrag auswählen und „Nach oben“/„Nach unten“ oder `Strg+Pfeil auf/ab` verwenden. „Aus Vorschau entfernen“ beziehungsweise `Entf` nimmt ihn nur aus dem Auftrag. Zähler und Zielnamen werden neu berechnet; Dateien werden dadurch nicht verändert.

**Namenskonflikte:** Die optionale automatische Nummerierung ergänzt freie Namen wie `Bericht (2).txt`. Ohne diese Option bleiben Kollisionen Fehler. Auch bei einer später entstandenen Zielkollision wird keine fremde Datei überschrieben. Eine fehlgeschlagene oder abgebrochene Ausführung versucht, bereits ausgeführte Schritte zurückzunehmen. Die erfolgreiche Rücknahme eines abgeschlossenen Auftrags kann neu angelegte leere Zielunterordner hinterlassen.

## Lister, Schnellansicht und Plugins

Text, Hex, Bilder, bereinigtes HTML, RTF und Multimedia verwenden dieselben Bedienwege für Lister und Schnellansicht. Windows-RichEdit stellt RTF-Tabellen und automatische Listen schreibgeschützt dar. OLE-Objekte, Datei- und Feldanweisungen werden entfernt; der sichtbare Feldtext bleibt erhalten. HTML und RTF sind auf 2 MiB begrenzt.

Zusätzliche Windows-Medienformate verwenden optional geladenes Media Foundation. Abspielen/Pause, Stopp, Position, Lautstärke und Stummschaltung sind per Maus und Tastatur erreichbar. Es gibt keinen automatischen Wiedergabestart. Fehlende Medienkomponenten beziehungsweise Codecs führen zu einer sichtbaren Fehlermeldung. Bereits unterstützte HTML5-Formate verwenden weiterhin WebView2.

WLX-Erkennungsregeln bestimmen die Reihenfolge passender Plugins. Lehnt ein Plugin ab, folgt der nächste Kandidat beziehungsweise die interne Vorschau. Miniaturen werden in begrenzten Hintergrund-Batches geladen. **Beide Plugin-Hosts müssen Protokoll 13 verwenden**; bei eigenen Builds den x86-Host ebenfalls neu bauen.

## Vergleich, Synchronisierung und Dateiwerkzeuge

Datei-/Binärvergleich, Dateifenstervergleich und Synchronisierung behalten die gemeinsamen Vorschau-, Fortschritts- und Ergebnisdialoge. Synchronisierung unterstützt lokale Ordner, lokal↔ZIP/7z, schreibgeschützten Archivvergleich und lokal↔FTP. Beim FTP-Inhaltsvergleich werden angebotene Server-Prüfsummen verwendet; bei Ablehnung kann der Inhalt gelesen werden. HASH-Antworten mit Bytebereich müssen die gesamte erwartete Datei abdecken; Dateiname, Algorithmus und Prüfsumme werden geprüft. Erst freigegebene Schritte werden ausgeführt.

Prüfsummen umfassen SFV/CRC32, MD5, SHA-1, SHA-2, SHA-3 und BLAKE3. Teilen/Zusammenfügen unterstützt die TC-CRC-Begleitdatei. UUE, XXE und MIME lassen sich kodieren/dekodieren; BinHex wird dekodiert. Beschädigte oder unvollständige Teilsätze werden als Fehler gemeldet.

## Noch zu prüfen

Automatisierte Prüfungen ersetzen keine Darstellungstests. Die verbleibende Maus-/Tastatur-, DPI-, RichEdit-, Codec-, Plugin-Fenster- und Druckerabnahme führt der Projektinhaber anhand von [UI-PRUEFUNG.md](UI-PRUEFUNG.md) durch. Ein direkter TC-Gegenlauf ist kein bereits bestandener Nachweis.

Herstellerreferenz: [Deutsche Total-Commander-Hilfe](https://www.totalcommander.ch/help/tcmdd1158_chm.zip), insbesondere `dlg_search.htm` und `dlg_multi_rename_tool.htm`. Windows-Zifferngruppierung: [GetLocaleInfoEx](https://learn.microsoft.com/en-us/windows/win32/api/winnls/nf-winnls-getlocaleinfoex).
