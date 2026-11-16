# Navigation und Dateioperationen

Bedienung der Ergänzungen in Version 1.4.0. Alle beschriebenen Aktionen sind über Menüs/Befehlspalette und die jeweiligen Dialoge erreichbar. Einzelklick markiert; Doppelklick oder Enter öffnet Datei bzw. Ordner. Einfg markiert und geht zur nächsten Zeile. Die Fensterabnahme übernimmt der Projektinhaber.

## Tabs und Spalten

**Ansicht → Spaltensätze** richtet eigene Ansichten ein. Standardfelder und registrierte WDX-Felder lassen sich mit TC-Ausdrücken kombinieren, beispielsweise `[=tc.fullname] · [=Plugin.Feld]`. Ein einzelnes Inhaltsfeld behält seinen numerischen, Zeit-, Booleschen oder Text-Sortierschlüssel. Zusammengesetzter Text wird nach dem dargestellten Text sortiert. Das Lesen erfolgt im Hintergrund; ein fehlendes Plugin beeinträchtigt die übrigen Spalten nicht.

In WFX-Fenstern unterscheidet Arco **Standardansicht**, **Plugin-Standardansicht** und eigene Sätze. Die Wahl gilt für den aktuellen Tab. Eigene Sätze enthalten Felder des Dateisystemplugins, Einheiten und zusammengesetzte Ausdrücke mit `[=<fs>.Feld]`. Der Spalteneditor bietet Referenzbreite, Mindestbreite der Namensspalte und horizontales Scrollen.

Tab-Gruppen können als `.tab` gespeichert und wieder geöffnet werden. Arco-Dateien enthalten zusätzliche eingebettete Definitionen, Optionen und die Tab-Sortierung. Eine fremde WDX-DLL wird dadurch nicht registriert oder aktiviert. Arco bindet ausschließlich bereits freigegebene Registrierungen; WFX-Definitionen können bis zur späteren Registrierung des Providers erhalten bleiben. Native TC-Spaltennummern benötigen die dazugehörigen importierten TC-Spaltensätze. Lokale und WFX-Nummern werden getrennt aufgelöst. Die zusätzlichen Arco-Definitionen werden von TC nicht als eigene Plugin-Installation übernommen.

## Automatische Ansichten

**Ansicht → Ansichtsmodi** bündelt Darstellung, Spalten und Sortierung. Automatische Regeln entscheiden geordnet anhand des Ortes und seiner Einträge. Modusbefehle verwenden die vorhandenen internen TC-Aliase. Ein Komma trennt Befehle; hinter `|` stehen Befehle beim Verlassen. `*` führt Eingangsaktionen auch bei Ordnerwechsel innerhalb desselben Modus aus, `-` unterdrückt sie beim Tabwechsel. Beispiel: `cm_100Percent|cm_50Percent`. Symbole für Tabs werden im Hintergrund geladen.

Allgemeine TC-Benutzerbefehle und weitere Kommandozeilenimporte gehören weiterhin zum Konfigurationsbereich der Gesamtmatrix. Die Ansichtsfunktion erweitert deren Dispatcher nicht automatisch.

## Windows-Orte

**Navigation → Weitere Windows-Orte** öffnet unter anderem Netzwerk, Bibliotheken, Anwendungen, Papierkorb, Systemsteuerung, Schriftarten und Drucker. Anbieter bestimmen, welche Dateioperationen verfügbar sind. Arco verwendet vorzugsweise auflösbare Shell-Adressen. Ohne eine solche Adresse wird eine begrenzte native Shell-Identität gespeichert; diese kann vom Rechner und vom installierten Anbieter abhängen. Ein Anzeigename wird weiterhin lesbar dargestellt.

## Buttonbars und Starter

Der Programmeditor kann Argumente zeilenweise oder als **Windows-Parameterzeile** bearbeiten. Importierte `.bar`-Zeilen behalten ihre ursprüngliche Quotierung beim Speichern und Exportieren. `%Q` steuert die automatische Quotierung nach TC-Regeln, `%B` unterstützt die dokumentierten Zweigansichtsvarianten. `%v` und `%V` beziehen sich auf WFX, FTP-Pfadparameter auf die FTP-Adresse. `?` öffnet aufgelöste Parameter zur Bearbeitung vor dem Start. Eine Listendatei bleibt bis zum Ende des gestarteten Programms verfügbar.

## Links und Metadaten

Im Kopierdialog erhält **Symbolische Links und Junctions als Links kopieren** Linkobjekte auch innerhalb ausgewählter Verzeichnisse. Verschieben bewahrt Links. Relative und nicht auflösbare symbolische Ziele bleiben Links; die Operation durchläuft das Ziel nicht. Symbolische Links benötigen das entsprechende Windows-Recht oder den Entwicklermodus. Andere Reparse-Typen und Volume-Mounts werden nicht als gewöhnliche Dateien übertragen.

Die NTFS-Optionen für Streams, Attribute, Zeiten und Sicherheitsdaten behandeln das Linkobjekt. Besitzer- und Überwachungsdaten hängen von Windows-Rechten ab. Bei Linkkonflikten einen freien Namen wählen oder überspringen. Eine Junction mit einem lokalen Laufwerksziel ist von einer symbolischen Verknüpfung zu einem UNC-Ziel zu unterscheiden.

Bei Drag-and-drop bestätigt Arco eine klassische Verschiebung erst nach den nötigen synchronen Windows-MOVE-Rückmeldungen und einer erneuten Quellenprüfung. Geänderte Quellen verhindern deren automatische Entfernung. Ausgewählte Hardlink-Namen werden einzeln behandelt. Die Schutzgrenzen stehen im Abschlussnachweis; sehr große Auswahlen können eine getrennte Quellenbereinigung erfordern.

## Ignorierliste und Sprachen

Ignorierregeln für Archivabgleiche verwenden den ursprünglichen Archivpfad. Eine temporäre Entpackposition verändert die Regelzuordnung nicht. Ignorierte Gegenstücke bleiben beim Spiegeln erhalten.

**Konfigurieren → Einstellungen → Allgemein** bietet Deutsch, Englisch und Französisch. Diese drei Sprachen bilden den unterstützten Sprachumfang. Dynamische Meldungen werden mit begrenzten Vorlagen übersetzt; Dateinamen, Pfade und vom Nutzer vergebene Namen bleiben Daten. Plugin-/Windows-eigene Dialoge und fremde Fehlermeldungen verwenden die Sprache ihres Anbieters. Technische TC-Ausdrücke, ältere Textformate und Parserkennungen werden nicht übersetzt, damit gespeicherte Definitionen gültig bleiben. Neue Einträge sollten über die beschrifteten Formularfelder angelegt werden.
