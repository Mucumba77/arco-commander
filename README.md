# Arco Commander

Arco Commander ist ein Zwei-Fenster-Dateimanager für Windows, geschrieben in Rust und inspiriert von Norton Commander und Total Commander. Die Desktop-Oberfläche verwendet Slint. Die erste Zielplattform ist Windows 11 x64.

## Entwicklungsstand

Die Oberfläche hat zwei kompakte, virtualisierte Dateilisten mit getrennten Spalten für Name, Erweiterung, Größe, Datum und Attribute. Die sortierte Spalte zeigt ihre Richtung an. Laufwerk und Speicherplatz stehen oberhalb der anklickbaren Pfadsegmente. `[..]` führt in den Elternordner und gehört nie zu einer Dateiauswahl. Das Verhältnis der beiden Fenster lässt sich an der Trennlinie ziehen und wird im Profil gespeichert.

Die Dateiliste verwendet standardmäßig Segoe UI Regular mit 14 logischen Pixeln. Unter **Einstellungen → Schrift** lassen sich Segoe UI, Arial und Verdana sowie 12, 14, 16 oder 18 Pixel auswählen. Zeilenhöhe und Detailspalten passen sich der Schriftgröße an.

`Strg+F1` schaltet auf Kurzansicht, `Strg+F2` auf Detailansicht und `Strg+Umschalt+F1` auf Miniaturansicht. Die Miniaturansicht lädt in lokalen Ordnern sichtbare BMP-, GIF-, JPEG-, PNG- und WebP-Bilder im Hintergrund. Bei anderen Dateien und Ordnern erscheint ein Symbol. Beim Aktualisieren des Ordners baut Arco die Vorschauen neu auf. Die Ansicht wird pro Tab gespeichert. [Total-Commander-Befehlsreferenz](https://www.ghisler.ch/wiki/index.php?title=List_of_internal_commands)

`Strg+F8` schaltet den Ordnerbaum im aktiven lokalen Dateifenster ein oder aus. Er beginnt im aktuellen Ordner und lädt einen Ast beim Aufklappen im Hintergrund. Mit den Pfeiltasten wechselst du den Eintrag oder klappst einen Ast auf und zu. Doppelklick oder Enter öffnet den Ordner in der darunterliegenden Dateiliste. Ein Klick auf die Dateiliste gibt ihr wieder den Tastaturfokus. [Total-Commander-Befehlsreferenz](https://www.ghisler.ch/wiki/index.php?title=List_of_internal_commands)

`Strg+Umschalt+F8` wechselt zwischen ausgeblendeten Bäumen, einem gemeinsamen Baum für das gerade aktive Dateifenster und je einem Baum pro Fenster. Diese Bäume beginnen am Windows-Desktop. Doppelklick oder Enter öffnet auch virtuelle Shell-Ordner im Arco-Dateifenster. **Ansicht → Windows-Desktop im Dateifenster** und **Ansicht → Dieser PC im Dateifenster** öffnen sie direkt. Shell-Ordner unterstützen den Tab-Verlauf, Pfadsegmente und das Windows-Kontextmenü für den angeklickten Eintrag. Physische Ordner wechseln in die normale Dateiansicht. Arco speichert die Shell-Adressen im eigenen Profil. [Windows-Shell-Namensraum](https://learn.microsoft.com/en-us/windows/win32/shell/navigate)

Das Hauptmenü und die Symbolleiste gruppieren die häufigen Aktionen. `Alt+D` öffnet die Pfadeingabe, `Strg+D` die Ordnerfavoriten und `Strg+S` den Schnellfilter. `Strg+Q` zeigt Text, Hexdaten oder ein Bild zur fokussierten Datei im gegenüberliegenden Fenster. `Strg+Umschalt+L` setzt den Fokus in die Befehlszeile. Dort funktionieren `cd <Pfad>`, Arco-Befehle aus `Strg+K` und die unterstützten `cm_`-Aliase. Die klassische Funktionsleiste reicht von `F2` bis `F8`. Kopieraufträge laufen im Hintergrund und melden Konflikte.

`Strg+Umschalt+S` springt zum nächsten sichtbaren Treffer und behält Filter und Markierungen bei. Die Schnellsuche bietet Namensanfang, Teiltreffer und regulären Ausdruck. Im Schnellfilter aktiviert ein führendes `~` reguläre Ausdrücke. Unter **Markieren** lässt sich die Auswahl in einer neuen Textdatei speichern und aus einer Dateiliste oder der Zwischenablage laden. Jede Zeile enthält einen Namen oder Pfad.

Unter **Einstellungen → Bedienung** legst du fest, ob die Schnellsuche zusätzlich beim Tippen oder mit `Alt` beziehungsweise `Strg+Alt` und einem Buchstaben oder einer Ziffer öffnet.

**Ansicht → Spaltensätze** oder `Umschalt+F1` legt einen Satz pro Tab fest. Neben Name, Erweiterung, Größe, Datum, Attributen und Kommentar lassen sich zuvor registrierte WDX-Felder mit eigener Überschrift und Breite einfügen. **Ansichtsmodi** speichern Ansicht, Spaltensatz und Sortierung. Unter **Konfigurieren → Automatische Ansichtsregeln** gilt die erste passende Regel. Zum Beispiel wählt `Fotos | lokal | C:/Medien/** | *.jpg;*.png | *.raw` den Modus **Fotos** für passende Ordner. Beim Verlassen des Bereichs wird die gespeicherte manuelle Ansicht des Tabs wieder aktiv. **Konfigurieren → Datei- und Auswahlfarben** bietet eigene Hell- und Dunkelwerte nach Maske sowie für Auswahl und Cursor. [Total-Commander-Spaltensätze](https://ghisler.ch/wiki/index.php?title=Custom_columns)

In **Mehrfachsortierung** kombinierst du Standardfelder und WDX-Spalten, etwa `erweiterung, -wdx1, name`. `wdx1` bezeichnet die erste WDX-Spalte der Ansicht. Ansichtsregeln akzeptieren zwei zusätzliche Felder. `Fotos | laufwerk | C:/Medien/** | *.jpg;*.png | | und | 50` verlangt einen passenden Pfad und mindestens 50 Prozent Bilder unter den Dateien im Ordner.

**Ansicht → Nur markierte Einträge umschalten** schränkt das aktive Dateifenster auf die beim Einschalten markierten Einträge ein. Der Elternordner bleibt erreichbar. Die Ansicht endet beim erneuten Aufruf, beim Ordnerwechsel oder wenn eine Suche oder Zweigansicht die Liste ersetzt. In der Befehlszeile funktionieren auch `cm_ShowOnlySelected` und `2023`. [Total-Commander-Befehlsreferenz](https://www.ghisler.ch/wiki/index.php?title=List_of_internal_commands)

`Alt+F1` öffnet die linke und `Alt+F2` die rechte Laufwerkswahl. Wähle mit Pfeiltasten und Enter oder tippe den Laufwerksbuchstaben. In der Dateiliste wählt ein Linksklick den Eintrag aus. Doppelklick und `Enter` öffnen Dateien und Ordner. `Strg`-Klick markiert einzeln und `Umschalt`-Klick markiert einen Bereich. Rechtsklick und `Umschalt+F10` öffnen bei lokalen Dateien das klassische Windows-Kontextmenü. Beim Rechtsklick auf eine markierte Datei gilt das Menü für die markierte Auswahl im selben Ordner.

**Ansicht → Windows-Ordner öffnen** zeigt die von Windows verwalteten Pfade für Desktop, Dokumente, Downloads und weitere Benutzerordner. **Netz** bietet die Windows-Dialoge zum Verbinden und Trennen von Netzlaufwerken. **Hilfe** öffnet bei Bedarf die Windows-Systeminformationen oder den Versionsdialog. [Windows-Ordner](https://learn.microsoft.com/windows/win32/shell/known-folders), [Netzlaufwerkdialoge](https://learn.microsoft.com/en-us/windows/win32/wnet/using-the-connections-dialog-box)

**Ordnerfavoriten** kann Gruppen mit Untergruppen sowie einen optionalen Ordner für das zweite Dateifenster speichern. Der Import aus `wincmd.ini` übernimmt passende `[DirMenu]`-Einträge mit ihren Titeln, lokalen Pfaden und Gruppen. Er löst auf beiden Seiten unter anderem `%COMMANDER_PATH%`, `%COMMANDER_DRIVE%`, `%USERPROFILE%` und `%$PERSONAL%` auf. Für `%COMMANDER_PATH%` sucht Arco nach der ausführbaren Datei neben der INI und verwendet danach die Umgebung oder die registrierte Installation. **Netz → Lokale Windows-Freigaben** zeigt lokale SMB-Freigaben. Neue Freigaben geben dem aktuellen Windows-Konto Lese- oder Vollzugriff auf Freigabeebene. Die Dateisystemrechte gelten zusätzlich. **Dateien → Datenträgerbezeichnung ändern** zeigt den ausgewählten lokalen Datenträger vor der Änderung an. [Total-Commander-Verzeichnisliste](https://ghisler.ch/wikide/index.php?title=Verzeichnisliste), [TC-Pfadvariablen](https://www.ghisler.ch/wikide/index.php/Navigation), [Windows-Freigaben](https://learn.microsoft.com/en-us/windows/win32/netshare/network-share-functions)

`↑` und `↓` bewegen den Cursor in der Dateiliste. Mit `Rücktaste` oder `Strg+Bild↑` wechselst du zum Elternordner. Wiederholte Rücktasten überspringen noch laufende Ladevorgänge und wechseln zum zuletzt angeforderten Elternordner.

Über das Zahnrad neben der Tab-Leiste lässt sich der aktive Ordner-Tab benennen, duplizieren, umordnen oder ins andere Dateifenster kopieren und verschieben. Ein fest gesperrter Tab öffnet Ordnerwechsel in einem neuen Tab. Die Sperre mit erlaubtem Ordnerwechsel kehrt beim Aktivieren oder über die Schaltfläche für den Stammordner zur gespeicherten Basis zurück. `Strg+Umschalt+A` öffnet die Tab-Liste. **Ansicht → Tab-Gruppen verwalten** speichert benannte Gruppen für beide Dateifenster und lädt sie durch Anhängen oder Ersetzen. Bei FTP- und Archiv-Tabs speichert das Profil den lokalen Ersatzordner.

In **Ansicht → Tab-Gruppen verwalten → Total-Commander-Datei importieren** kannst du eine `.tab`-Datei als Gruppe speichern. Arco ordnet `[activetabs]` der beim Start des Imports aktiven Seite zu und `[inactivetabs]` der anderen Seite. Die Vorschau zeigt lokale Pfade, Titel, den aktiven Tab und unterstützte Sperren. Weitere Optionen und übersprungene virtuelle Pfade werden gezählt. Beim Ersetzen bleiben Seiten ohne importierte Tabs erhalten. [Total-Commander-Hilfe und FAQ](https://www.ghisler.com/dfaq.htm)

**Ansicht → Ordner-Tabs als .tab exportieren** speichert die offenen Tabs beider Seiten oder einer einzelnen Seite. Gespeicherte Gruppen haben denselben Export in ihrem Gruppenmenü. Arco schreibt UTF-16LE und erwartet einen neuen Dateinamen. Die Befehlszeile unterstützt `SAVETABS`, `SAVETABSL`, `SAVETABSR`, `SAVETABS2`, `SAVETABS2L`, `SAVETABS2R`, `OPENTABS` und `APPENDTABS` mit einer `.tab`-Datei als Argument.

Im Menü **Ansicht** lassen sich markierte lokale Ordner als Hintergrund-Tabs auf beiden Seiten öffnen. **Kürzlich geschlossene Tabs** zeigt die letzten 20 einzeln geschlossenen Tabs der laufenden Sitzung und öffnet sie auf ihrer ursprünglichen Seite.

**Konfigurieren → Einstellungen** enthält Seiten für Layout, Bedienung und Tastatur. Im Layout lassen sich die obere und mittlere Symbolleiste, Laufwerksbuttons und Laufwerksliste, Pfad, Verlauf und Spaltenüberschriften separat ein- und ausblenden. `Alt+D` öffnet die Pfadeingabe auch bei ausgeblendetem Pfad. Unter Bedienung lässt sich einstellen, ob die Ziffernblock-Auswahl Ordner einbezieht.

Unter **Konfigurieren → Eigene Buttonbars** bearbeitest du die obere oder mittlere Leiste. Eine Schaltfläche enthält Beschriftung, Symbol und einen Arco-Befehl, einen unterstützten `cm_`-Aufruf oder einen absoluten EXE-Pfad. Beim Symbol kannst du auch den absoluten Pfad einer Bilddatei bis 1 MiB eintragen. Für eine EXE steht jedes Argument auf einer eigenen Zeile. Die Leisten übernehmen die gespeicherte Reihenfolge. Eine leere Liste zeigt die Standardbelegung. [Total-Commander-Buttonbar](https://ghisler.ch/wiki/index.php?title=Button_bar)

Im Menü der jeweiligen Leiste importierst du unterstützte Befehle und absolute EXE-Pfade mit festen Argumenten aus `.bar`-Dateien. Die Vorschau zeigt, welche Schaltflächen übernommen werden. Der Export schreibt eine neue `.bar`-Datei in UTF-16LE. [Total-Commander-Buttonbar](https://ghisler.ch/wiki/index.php?title=Button_bar)

**Starter** verwendet denselben Editor für benannte Programme und Arco-Befehle. **Konfigurieren → Buttonbar- und Startervorlagen** speichert beide Leisten und das Startermenü unter einem Namen. Unter **Einstellungen** kannst du das helle, dunkle oder das Windows-Farbschema wählen.

Unter **Tastatur** wählst du einen Befehl, klickst auf **Tastenkürzel aufzeichnen** und drückst die Kombination. **Zuweisen** ergänzt eine Belegung. Bei einem Konflikt zeigt Arco den bisherigen Befehl an und bietet **Belegung ersetzen** an. Einzelne Belegungen lassen sich entfernen. **Standardbelegung laden** setzt alle Kürzel im Entwurf zurück. **Anwenden** oder **OK** speichert die Änderungen im Profil. Systemtasten wie `Alt+F4` sowie `Esc` und das alleinige `F10` behalten ihre Bedienfunktion.

Die Standardbelegung folgt bei `Alt+←/→` dem Ordnerverlauf. `Num +` und `Num −` erweitern beziehungsweise verkleinern die Auswahl nach Maske. `Num *` kehrt die Auswahl um und `Num /` stellt die vorherige Auswahl wieder her. `Alt+Num +/−` arbeitet mit der Erweiterung der Datei unter dem Cursor.

`Strg+Umschalt+B` zeigt markierte lokale Dateien und alle Dateien in den markierten Ordnern in einer Liste. Erneutes Drücken kehrt zur Ordneransicht zurück. [Total-Commander-Funktionsbeschreibung](https://www.ghisler.ch/board/viewtopic.php?t=26819)

`Strg+L` zeigt die logische Größe der lokalen Auswahl und den von ihren Dateidaten belegten Speicher. Windows liefert die Belegung komprimierter und dünn belegter Dateien. Hardlinks zählen für den belegten Speicher einmal. Bei Abfragefehlern kennzeichnet der Dialog die Teilsumme. `Alt+Umschalt+Enter` trägt die logischen Größen der sichtbaren Ordner in die Größenspalte ein. `Esc` bricht die Berechnung bei fokussierter Dateiliste ab. [Total-Commander-Befehlsreferenz](https://www.ghisler.ch/wiki/index.php?title=List_of_internal_commands)

`Strg+Umschalt+F2` zeigt die Kommentare eines Ordners aus `descript.ion`. `Strg+Z` bearbeitet den Kommentar des fokussierten Eintrags. **Dateien → Attribute und Zeitstempel ändern** setzt oder entfernt R, H, S und A und kann die Auswahl rekursiv bearbeiten. Dateilisten und Textdateien lassen sich über **Dateien** mit Vorschau drucken. Der Lister bietet ebenfalls Textdruck an. [Ghisler zu descript.ion](https://www.ghisler.ch/board/viewtopic.php?p=355053), [Windows-Druckdialog](https://learn.microsoft.com/en-us/windows/win32/api/commdlg/ns-commdlg-printdlgw)

Im Menü **Dateien** lassen sich Namen, Pfade und Details der Dateiauswahl als Text kopieren. Die Detailansicht gibt Name, Erweiterung, Größe, Datum, Attribute und aktive WDX-Spalten als tabulatorgetrennte Werte aus.

Markiere die gewünschten Zielordner in einem lokalen Dateifenster. Wähle im anderen Fenster die Quelldateien und Ordner aus und öffne **Dateien → In markierte Zielordner kopieren**. Die Vorschau listet alle Ziele auf. Arco löst markierte Verzeichnislinks auf, verarbeitet jeden Zielordner einmal und prüft Überschneidungen vor der ersten Kopie. Ignorierliste, Konfliktregel und Abbruch gelten für den gesamten Auftrag.

Beim lokalen Kopieren mit `F5` und Verschieben mit `F6` kannst du den vorhandenen Zielordner ändern und eine Zielmaske eintragen. Relative Pfade gehen vom vorgeschlagenen Ordner aus. `*.bak` ändert die Erweiterung und `*-Kopie.*` hängt Text an den Namen an. `?` übernimmt das Zeichen an seiner Position und `*` die verbleibenden Zeichen. Arco prüft Namen und Kollisionen, bevor es mit dem Auftrag beginnt. [Total-Commander-Referenz zu Zielmasken](https://ghisler.ch/board/viewtopic.php?p=204785)

F5 und F6 bieten gespeicherte lokale Suchfilter sowie Regeln für gleichnamige Zieldateien. Dazu gehören Überspringen, Überschreiben, Überschreiben nach Datum oder Größe und ein freier Zielname. F5 kann außerdem an den bisherigen Zielinhalt anhängen. **Dateien → Transfermanager** zeigt lokale Aufträge, FTP- und FXP-Transfers, Kopien über Archivfenster und Plugin-Mehrfachaufträge in einer Warteschlange. Reihenfolge, Abbruch und optionales Protokoll gelten für diese Transfers. Pause, Geschwindigkeitslimit und erneuter Kopierversuch stehen für geeignete lokale Aufträge zur Verfügung. Mit **Dateikommentare mitführen** steuert der Transfermanager die Übernahme von `descript.ion`-Kommentaren bei lokalen Transfers.

Standardmäßig öffnet ein Dateikonflikt einen Dialog mit Pfad, Größe, Zeitstempel, Schreibschutz und Text- oder Hexvorschau für beide Seiten. Die Entscheidung gilt für diese Datei oder optional für alle folgenden Konflikte. Wenn sich die Zieldatei inzwischen geändert hat, meldet Arco beim Überschreiben einen Fehler.

Bei lokalen Zielen kannst du einen eigenen Dateinamen eingeben. Arco prüft den Namen und veröffentlicht die Datei nur, wenn der Name im Zielordner frei ist. Beim Entpacken aus einem Archivfenster und bei WFX-Downloads steht derselbe Dialog zur Verfügung. FTP-Kopien fragen bei vorhandenen Zieldateien nach der FTP-Übersicht. Bei FTP-Kopien in beide Richtungen und zwischen Servern sowie bei beschreibbaren ZIP- und 7z-Zielen lässt sich der Quellinhalt an die Zieldatei anhängen. Der Inhalt wird vor der Veröffentlichung temporär zusammengesetzt.

Bei WFX-Zielen kannst du überspringen, unter einem freien Namen speichern oder eine einzelne Datei überschreiben. Für das Überschreiben benötigt das Plugin die jeweiligen Kopier-, Umbenenn- und Löschoperationen. Arco bereitet die neue Datei unter einem Zwischennamen vor und prüft Größe, Änderungszeit und Attribute der bisherigen Zieldatei vor der Veröffentlichung. Falls die Veröffentlichung scheitert, versucht Arco die bisherige Zieldatei wiederherzustellen und nennt im Fehlerfall die relevanten Sicherungs- und Zwischenpfade. Für kleine FTP-, WFX- und Archiveinträge bis 64 KiB kann der Dialog eine Text- oder Hexvorschau laden. [WFX-SDK](https://www.ghisler.com/plugins.htm)

**Konfigurieren → NTFS-Kopieroptionen** speichert vier getrennte Optionen für lokale F5- und F6-Aufträge. Sie erhalten benannte Datenströme, die Quell-DACL als geschützte DACL, Hardlink-Gruppen innerhalb eines Zielauftrags und die freien Bereiche von Sparse-Dateien. Verschieben per nativer Umbenennung behält die vorhandenen Dateimetadaten. [Windows-Dateiströme](https://learn.microsoft.com/en-us/windows/win32/fileio/file-streams), [Sparse-Dateien](https://learn.microsoft.com/en-us/windows/win32/fileio/sparse-files)

Unter **Konfigurieren → Interne Dateizuordnungen** hinterlegst du Masken, Aktionen und Programme. Die erste passende Standardaktion gilt beim Öffnen in Arco. **Dateien → Interne Dateiaktionen** bietet alle passenden eigenen Verben an. **Mit Windows öffnen** und **Öffnen mit** rufen die Windows-Zuordnung auf. Im Dateimenü stehen außerdem die Windows-Aktionen für Administrator, anderen Benutzer und `.lnk`-Verknüpfungen bereit. [Windows-Shell-Verben](https://learn.microsoft.com/en-us/windows/win32/shell/launch)

`Alt+F7` öffnet die erweiterte lokale Suche. Sie unterstützt mehrere Suchorte, eine markierte Auswahl, Masken und Ausschlüsse, reguläre Ausdrücke, Text mit Kodierungswahl, Hexdaten, Größe, Datum, Attribute und Ordnertiefe. Benannte Profile stehen auch als lokale Transferfilter zur Verfügung. Die Suchergebnisse lassen sich als Dateiliste übernehmen. Unter **Konfigurieren → Everything-Suche** kann der absolute Pfad zur optionalen `es.exe` hinterlegt werden. `ev:` fragt den globalen Index ab und `ed:` beschränkt die Abfrage auf die Suchorte. Einfache Suchmuster können bei fehlendem Index mit der internen Suche laufen. [Total-Commander-Suche](https://ghisler.ch/wiki/index.php?title=Search), [Everything CLI](https://www.voidtools.com/support/everything/command_line_interface/)

Über **Dateien → Dateien kodieren** und **Dateien → Dateien dekodieren** verarbeitest du einzelne UUE- und XXE-Datenblöcke und MIME-Anhänge. Arco schreibt die Ergebnisse in den lokalen Ordner gegenüber. Die MIME-Kodierung erzeugt eine Base64-Datei mit der Endung `.b64`. Beim Dekodieren verarbeitet Arco einzelne Base64-Anhänge und bis zu 64 MiB große mehrteilige MIME-Dateien. BinHex 4.0 lässt sich dekodieren. Ein vorhandener Mac-Ressourcenzweig wird als `.rsrc`-Begleitdatei gespeichert. Arco prüft Dateinamen und schreibt ausschließlich neue Zieldateien. [MIME-Base64](https://www.rfc-editor.org/rfc/rfc2045.html), [BinHex 4.0](https://www.rfc-editor.org/rfc/rfc1741.html)

Für `message/partial` markierst du alle Fragmente und wählst MIME beim Dekodieren. Arco prüft die Teilnummern, fügt passende Nachrichten bis 64 MiB zusammen und schreibt anschließend die Anhänge. [MIME-Nachrichtenteile](https://www.rfc-editor.org/rfc/rfc2046#section-5.2.2)

Mit **Ansicht → Nur Programme anzeigen** (`Strg+F11`) bleiben Ordner und Programme mit den Erweiterungen EXE, COM, BAT, PIF und CMD sichtbar. `Strg+F10` stellt die normale Ansicht wieder her.

Unter **Konfigurieren → Ignorierliste bearbeiten** speicherst du Namen, Pfade und Masken. Eine vorhandene `tcignore.txt` lässt sich importieren und vor dem Speichern bearbeiten. `cm_SwitchIgnoreList` und **Ansicht → Ignorierliste umschalten** schalten die Regeln um. Sie gelten in lokalen, FTP- und WFX-Dateifenstern sowie für lokale Suche, Größenberechnung, Kopieren, Verschieben und Synchronisieren. Beim lokalen Verschieben bleiben ignorierte Unterordner an der Quelle erhalten, auch auf demselben Volume. [Offizielle CHM-Hilfe](https://www.totalcommander.ch/help/tcmdd1158_chm.zip)

FTP-Kopien und der FTP-Abgleich berücksichtigen die Regeln für die beteiligten Hosts und Profile. Beim FTP-Spiegeln bleiben Elternordner mit ignorierten Einträgen erhalten.

Über **Werkzeuge** stehen die Zweigansicht, Text-, Hex- und Bildvorschau, rekursive Datei- und Textsuche, ein Textvergleich, Mehrfachumbenennung, Verzeichnissynchronisierung mit Vorschau, Prüfsummen, Duplikatsuche, Teilen und Zusammenfügen sowie ZIP-Funktionen inklusive AES zur Verfügung. Der Import aus `wincmd.ini` ergänzt Tabs, Verzeichnisfavoriten und unterstützte Kürzel aus `[Shortcuts]`. Die Vorschau zeigt betroffene Standardbelegungen. Eigene Arco-Belegungen haben Vorrang. [TC-Profilformat](https://www.ghisler.ch/wiki/index.php/Wincmd.ini)

Der Lister kann Text und Hex durchsuchen und zu einem dezimalen oder hexadezimalen Byte-Offset springen. Für Text stehen automatische Erkennung, UTF-8, UTF-16LE, UTF-16BE und Windows-1252 zur Wahl. **Dateien → Lister in eigenem Fenster** öffnet lokale, FTP-, WFX- und Archivdateien in unabhängigen Fenstern. Mit **Eigenes Fenster** lässt sich die aktuelle interne Vorschau aus dem Hauptfenster lösen. Im Textvergleich lassen sich Unterschiede einzeln bearbeiten oder auf die andere Seite übernehmen. Vor dem Speichern prüft Arco den bisherigen Dateiinhalt und erhält die Kodierung und den vorherrschenden Zeilenumbruch der Zieldatei. [Windows-Dateiersetzung](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-replacefilew)

HTML und RTF bis 2 MiB öffnen sich mit **F3** im separaten internen Lister. HTML zeigt bereinigte Überschriften, Tabellen, Listen und eingebettete Rasterbilder. RTF zeigt Text, Zeilenumbrüche und grundlegende Formatierungen wie Fett, Kursiv, Unterstrichen und Schriftgröße. Der Lister bietet außerdem Wiedergabesteuerung für gängige Audio- und Videoformate wie MP3, WAV, MP4 und WebM. Die formatierte Anzeige und Wiedergabe verwenden die installierte Microsoft Edge WebView2 Runtime. Die Wiedergabe hängt von den verfügbaren Codecs ab. Über **Quelldaten anzeigen** wechselst du zu Text oder Hex. [WebView2 Runtime](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/distribution)

Im Binärvergleich blätterst du durch die Byteabweichungen. Beide Seiten zeigen denselben hexadezimalen Offset. `--` kennzeichnet ein fehlendes Byte am Ende der kürzeren Datei.

**Befehle → Umbenennvorlagen** speichert Masken und optionale RegEx-Ersetzungen. Eine Dateinamensliste kann direkt oder im eingestellten externen Editor bearbeitet werden. Der Dialog prüft die neuen Namen vor der Übernahme. `[C10+5:3]` beginnt bei 010, `[Y]-[M]-[D]` verwendet das Änderungsdatum und `[X+]` fügt bei fehlendem Zwischenablagetext eine leere Zeichenfolge ein. [Total-Commander-Mehrfachumbenennung](https://ghisler.ch/wiki/index.php?title=Multi-Rename_Tool)

`[Caa]` startet einen zweistelligen Buchstabenzähler, `[C+1/100:a]` erhöht den Zähler nach jeweils 100 Einträgen und bestimmt die Zahlenbreite automatisch. `[N2-5]` übernimmt einen Zeichenbereich aus dem Namen und `[N-3-]` dessen letzte drei Zeichen. [Total-Commander-Zähler und Zeichenbereiche](https://www.ghisler.ch/wiki/index.php?title=Multi-rename_tool#Multi-rename_tool_placeholders)

**Datei teilen** bietet einen Total-Commander-Modus mit nummerierten Teilen und `.crc`-Begleitdatei. **Dateien verbinden** erkennt den Satz anhand der Begleitdatei oder eines nummerierten Teils im selben Ordner. Arco schlägt den Originalnamen vor und prüft Größe und CRC32 vor dem Speichern. [Total-Commander-Änderungsübersicht](https://www.ghisler.ch/wiki/index.php?title=List_of_changes_%28version_7.04a_-_7.50%29)

- **Archive.** ZIP und 7z lassen sich direkt in Arco öffnen, prüfen, erstellen und bearbeiten. Der optionale Adapter erstellt TAR, GZIP und selbstentpackende 7z-Archive und liest weitere Formate. GZIP akzeptiert eine einzelne Datei. Der Adapter benötigt 7-Zip 26.05 oder neuer. Arco sucht zuerst am absoluten Pfad in `ARCO_7ZIP_PATH`, dann unter `7zip\7z.exe` neben Arco und schließlich unter `%ProgramFiles%\7-Zip\7z.exe`. [7-Zip-Download](https://www.7-zip.org/download.html)
- **FTP und FTPS.** Verbindungen öffnen sich im aktiven Dateifenster und nutzen für gewöhnliche Transfers das mit Windows gelieferte `curl.exe`. Downloads können ein geprüftes Teilstück fortsetzen, sofern der Server die benötigten Angaben und Bytebereiche liefert. Direkter FXP-Transfer ist zwischen geeigneten Klartext-FTP-Servern über IPv4 verfügbar. Der Zielserver kann nach einem Abbruch eine temporär benannte Teildatei behalten.
- **Total-Commander-Plugins.** Unter **Konfigurieren → Plugin-Verwaltung** kannst du WCX-, WFX-, WDX- und WLX-DLLs registrieren, ZIP-Pakete mit `pluginst.inf` installieren oder Registrierungen aus `wincmd.ini` importieren. Dort kannst du Plugins aktivieren, deaktivieren, umordnen, umbenennen, per Dateimaske eingrenzen, prüfen und entfernen. Neue Registrierungen sind zunächst deaktiviert. Die Architektur liest Arco aus der DLL. Beim Prüfen und Ausführen lädt der passende Host die DLL mit deinen Windows-Rechten. [Ghisler-Plugin-SDKs](https://www.ghisler.com/plugins.htm), [Format für Plugin-Installationspakete](https://www.ghisler.ch/wiki/index.php/Plugins_Automated_Installation)

Die Plugin-Werkzeuge verwenden die aktivierten passenden Registrierungen. `F3` verwendet das erste aktive WLX-Plugin mit passender Dateimaske in einem eigenen Fenster. `Strg+Q` bettet den passenden WLX-Host in die Schnellansicht ein. Wenn der Host nicht startet oder beendet wird, übernimmt die interne Vorschau. **Dateien → Intern anzeigen** öffnet den eingebauten Betrachter. Einfache TC-Regeln für Dateierweiterungen werden beim Import zu Masken. Weitere TC-Erkennungsregeln sind als Hinweis sichtbar.

Wähle in der **Plugin-Verwaltung** ein aktives WCX- oder WFX-Plugin und öffne **Dateioperationen**. Bei WCX kannst du Archive prüfen, einzelne Dateien entpacken, packen und löschen, sofern das Plugin die jeweilige Funktion bereitstellt. Änderungen werden an einer Archivkopie geprüft und anschließend veröffentlicht. WFX unterstützt Upload, Download, Umbenennen, Verschieben, Anlegen von Ordnern sowie das Löschen von Dateien und leeren Ordnern. Lokale Downloads erwarten einen neuen Zieldateinamen. Bei einem abgebrochenen WFX-Schreibauftrag lässt sich der tatsächliche Zielzustand im Plugin-Dateisystem prüfen. [Ghisler-Plugin-SDKs](https://www.ghisler.com/plugins.htm)

**Netz → WFX-Dateisystem öffnen** lädt ein aktiviertes WFX-Plugin in das aktuelle Dateifenster. Die Pfadleiste akzeptiert Plugin-Pfade, und Ordner lassen sich per Doppelklick oder `Enter` öffnen. `F5` kopiert Dateien und vollständige Ordner. `F6` verschiebt sie zwischen einem lokalen Ordner und einem WFX-Fenster in beide Richtungen. In zwei Fenstern desselben Plugins übernimmt das Plugin das Verschieben. `F3` zeigt eine lokale Vorschaukopie an. `F7` erstellt einen Ordner, `Umschalt+F6` benennt einen Eintrag um. `F8` löscht nach Bestätigung Dateien und Ordner samt Inhalt dauerhaft. **Netz → WFX verlassen** kehrt zum lokalen Ordner zurück.

Rekursive Transfers erhalten leere Unterordner und erwarten einen neuen Zielnamen. Beim Verschieben zwischen lokalem Fenster und Plugin vergleicht Arco den übertragenen Inhalt bytegenau, bevor es die Quelle entfernt. Dazu liest es die Dateien über das Plugin erneut. Ein Fehler beim Entfernen wird als Teilzustand gemeldet. Rekursive Aufträge folgen keinen Verknüpfungen im Quellbaum. Ordner direkt im virtuellen Stamm sind beim rekursiven Löschen und beim Verschieben zwischen WFX und lokalem Fenster geschützt, da sie Verbindungen darstellen können.

Wenn das Plugin nach Zugangsdaten oder einer Bestätigung fragt, öffnet Arco einen Dialog. Passwortfelder sind maskiert. Arco übergibt die Antworten direkt über die Prozess-Pipe und speichert sie weder im Profil noch in temporären Dateien. Ein neuer Host kann erneut nach einer Anmeldung fragen. Ein WFX-Mehrfachauftrag verarbeitet Gruppen von bis zu 48 Einträgen im selben Host. Der Bericht ordnet Konflikte und Fehler den Einträgen zu.

Plugins mit `FsSetDefaultParams` erhalten ein eigenes Einstellungsverzeichnis `arco-wfx/<Pluginname>` neben der DLL. Ghislers SFTP-Plugin legt dort seine `sftpplug.ini` mit Verbindungsprofilen und bekannten Hostschlüsseln ab. Eine neue Verbindung lässt sich im Plugin-Stamm über `F7` einrichten. [Ghisler SFTP-Plugin](https://www.ghisler.com/plugins.htm)

Wenn du im WFX-Plugin die verschlüsselte Speicherung wählst, legt Arco das Kennwort in der Windows-Anmeldeinformationsverwaltung des aktuellen Kontos ab. Die Anmeldedaten gehören zum Plugin-Verzeichnis und zur Verbindung. Passende 32- und 64-Bit-Varianten im selben Verzeichnis teilen sich den Speicher. Arco unterstützt dabei auch die SDK-Aufrufe zum Kopieren, Verschieben und Löschen eines Kennworts. Das Windows-Konto entsperrt den Speicher. [WFX-SDK](https://ghisler.github.io/WFX-SDK/cryptproc.htm), [Windows Credential Manager API](https://learn.microsoft.com/en-us/windows/win32/api/wincred/nf-wincred-credwritew)

WCX kann markierte lokale Dateien gemeinsam zum Packen vormerken. Zum Entpacken oder Löschen mehrerer Archiveinträge gib die Pfade mit `|` als Trennzeichen ein. Jede Änderung am Archiv wird separat geprüft und veröffentlicht. Der Bericht führt bereits abgeschlossene Einträge auf.

Unter **WDX-Spalte hinzufügen** in der Plugin-Verwaltung wählst du ein Feld und gegebenenfalls eine vom Plugin angebotene Einheit. Die lokale Standard-Detailansicht zeigt bis zu drei WDX-Spalten. Eigene Spaltensätze können bis zu 29 Zusatzspalten aus Standard- und WDX-Feldern enthalten. Arco lädt sichtbare Werte im Hintergrund. Ein Klick auf den Spaltenkopf sortiert den geladenen Dateibestand nach Feldtyp. Ein zweiter Klick kehrt die Richtung um. Fehlende Werte stehen innerhalb ihrer Datei- oder Ordnergruppe am Ende. Ein neuer Verzeichnisinhalt beginnt wieder mit der Standardspalte oder Sortierfolge des Tabs. Unter **Konfigurieren → WDX-Spalten verwalten** lassen sich WDX-Felder aus der Auswahlliste entfernen. Bereits gespeicherte Spaltensätze behalten ihre Felddefinition.

Unter **Befehle → Nach Plugin-Feld suchen** kannst du bis zu 16 Bedingungen aus aktiven WDX-Plugins hinzufügen. Die Suche kombiniert eine Dateimaske mit Teiltreffern, Vergleichen wie `>= 1024` in der gewählten Einheit oder einem regulären Ausdruck mit `~` am Anfang. In der Übersicht wählst du, ob alle oder mindestens eine Bedingung erfüllt sein muss. **Als Suchprofil speichern** merkt sich Maske, Bedingungen samt Einheiten und Modus. **Befehle → WDX-Suchprofile** öffnet die Abfrage für den gerade aktiven lokalen Ordner. Die Suche schließt seine Unterordner ein und zeigt die Treffer im Dateifenster.

Unter **Befehle → WDX-Feld ändern** kannst du beschreibbare Plugin-Felder für bis zu 128 lokale Dateien ändern. Das Plugin schreibt nach der Bestätigung direkt in die Dateien. Der Ergebnisdialog zeigt den Status pro Datei. Änderungen können bei einem Fehler oder Abbruch teilweise gespeichert sein. [WDX-Schreibschnittstelle](https://ghisler.github.io/WDX-SDK/contentsetvalue.htm)

Wenn das Feld einen **Plugin-Editor** anbietet, kannst du ihn anstelle der direkten Eingabe öffnen. Das Plugin bekommt den verfügbaren Wert der ersten ausgewählten Datei als Ausgangswert. Mit **OK** im Plugin-Editor wird der neue Wert auf die bestätigte Auswahl angewendet. [WDX-Editor-Schnittstelle](https://ghisler.github.io/WDX-SDK/contenteditvalue.htm)

In der Mehrfachumbenennung fügt `[=Plugin.Feld]` den Feldwert in der ersten Einheit in die Namensmaske ein. Mit `[=Plugin.Feld.Einheit]` wählst du eine andere vom Plugin angebotene Einheit. Ein fehlender Wert oder ungültiger Zielname stoppt die Vorschau. [Total-Commander-Referenz zur Mehrfachumbenennung](https://ghisler.ch/wiki/index.php?title=Multi-Rename_Tool)

ZIP-Pakete erhalten einen eigenen Ordner neben dem Profil. Beim Entfernen der letzten zugehörigen Registrierung entfernt Arco das verwaltete Paketverzeichnis. Extern registrierte DLLs bleiben am ursprünglichen Ort.

### Archive in den Dateifenstern

- Öffne ein unterstütztes Archiv mit `Enter` oder mit der eingestellten Mausaktion. `[..]` führt aus dem Archivstamm zum Ordner mit der Archivdatei zurück. Die Pfadeingabe akzeptiert auch `C:\Ordner\Beispiel.zip::Unterordner`.
- `F3` zeigt die fokussierte Datei an. `Strg+Q` öffnet die Schnellansicht für unverschlüsselte Einträge. `Alt+F7` sucht im aktuellen Archivordner und seinen Unterordnern. ZIP, JAR, XPI, 7z und eigenständige Brotli-Dateien unterstützen auch Textsuche. Vorschau und Textsuche verarbeiten bis zu 64 MiB pro Datei.
- Eigenständige `.br`-Dateien und unverschlüsselte ZIP-Einträge mit Brotli-Methode 129 lassen sich direkt prüfen und entpacken. `.tbr` zeigt die enthaltene TAR-Datei an. [Total-Commander-Änderungsübersicht](https://www.ghisler.com/mailingd.htm)
- **Archive → Packen** erstellt ZIP- und 7z-Mehrteiler mit einstellbarer Teilgröße und optionalem AES-256-Passwort. Bei 7z schützt das Passwort auch die Dateinamen. Die native Archivansicht öffnet einen Satz über `.7z.001` oder `.zip.001` und liest direkt aus den Folgesegmenten im selben Ordner. Vorschau, Textsuche, Prüfen und Entpacken verwenden das Archivpasswort bei Bedarf. [7-Zip-Mehrteiler](https://www.7-zip.org/recover.html)
- `F5` entpackt markierte Einträge in den lokalen Ordner gegenüber oder kopiert sie in geöffnete ZIP-, 7z- und unterstützte ACE-Archive. Für ZIP und 7z umfasst das auch Mehrteiler. Der Konfliktdialog fragt beim Entpacken und bei beschreibbaren ZIP- und 7z-Zielen nach der gewünschten Behandlung.
- `Umschalt+F6` benennt Dateien oder Ordner in 7z, ZIP und unterstützten ACE-Einzelarchiven um. `F8` entfernt die markierte Auswahl nach Bestätigung. ZIP unterstützt dabei AES und ZipCrypto. Beim Neuschreiben entschlüsselt Arco erhaltene ZipCrypto-Einträge im Datenstrom und verschlüsselt sie wieder mit demselben Verfahren. **ZIP (ZipCrypto)** steht als eigene Packoption mit Passwort zur Verfügung. Mehrteiler werden vor dem Austausch vollständig geprüft und `.001` wird zuletzt veröffentlicht. Bei 7z bleiben der Schutz der Dateinamen und die Verschlüsselung vorhandener Einträge erhalten. [ZIP-Bibliothek](https://docs.rs/crate/zip/9.4.1), [7z-Bibliothek](https://github.com/hasenbanck/sevenz-rust2)
- Unter Windows übernehmen die eingebauten Archivpfade beim Entpacken `Zone.Identifier` auf Dateisysteme mit benannten Datenströmen. Enthält ein Archivsatz mehrere Zonenmarkierungen, übernimmt Arco die strengste. [Windows-Dateiströme](https://learn.microsoft.com/en-us/windows/win32/fileio/file-streams)
- **Archive → Öffnen** behandelt 7z- und ZIP-SFX-Dateien als Archiv. **Archive → Packen** erstellt selbstentpackende Dateien beider Formate mit optionalem AES-256-Passwort. 7z-SFX verwendet das Modul `7z.sfx` und schützt mit dem Passwort auch die Dateinamen. ZIP-SFX verwendet `arco-zip-sfx.exe` neben Arco und akzeptiert ein Passwort von bis zu 256 UTF-16-Codeeinheiten. Beim Start von ZIP-SFX öffnet sich die Windows-Ordnerauswahl und bei Bedarf ein Passwortdialog. Für Skripte akzeptiert ZIP-SFX `--extract-to <vorhandener Ordner>` und liest das Passwort bei verschlüsselten Dateien als UTF-8 von der Standardeingabe bis zum Dateiende. [7-Zip-Funktionsübersicht](https://www.7-zip.org/), [Windows-Passwortdialog](https://learn.microsoft.com/en-us/windows/win32/api/wincred/nf-wincred-creduipromptforcredentialsw)

ACE und UC2 verwenden optional eine Python-Umgebung. Lege sie unter `archive-python` neben der Arco-EXE an oder setze `ARCO_ARCHIVE_PYTHON` auf den absoluten Pfad zu `python.exe`. Die folgenden Befehle richten die getesteten Paketversionen in einer separaten Umgebung ein. [acefile](https://github.com/droe/acefile), [UC2](https://pypi.org/project/uc2/)

```powershell
py -3.10 -m venv archive-python
.\archive-python\Scripts\python.exe -m pip install acefile==0.6.14 uc2==3.0.0a10
```

ACE unterstützt das Anzeigen, Prüfen und Entpacken unverschlüsselter Einzelarchive über `acefile`. **ACE (LZ77)** und **ACE 2.0 (Blocked)** komprimieren direkt in Rust. Blocked wählt pro Datenblock LZ77 oder LZ77 mit Byte-Delta-Filter. Beide wählen pro Datei Store, wenn es kleiner ist. **ACE (ohne Kompression)** speichert alle Einträge ohne Kompression. Beim Bearbeiten übernimmt Arco vorhandene komprimierte Einträge unverändert und prüft das Ergebnis mit `acefile`. In Solid-Archiven blockiert Arco Löschvorgänge, falls spätere Einträge Daten der Auswahl zum Entpacken benötigen könnten. Dateinamen müssen in CP437 darstellbar sein. Schreibzugriffe auf gesperrte, verschlüsselte und mehrteilige Archive sowie Einträge mit Dateikommentaren oder NTFS-Sicherheitsdaten sind ausgenommen. [ACE-Formatspezifikation](https://www.hugi.scene.org/online/coding/hugi%2012%20-%20coace.htm), [ACE-2.0-Referenzdecoder](https://github.com/droe/acefile/blob/master/acefile.py)

UC2 unterstützt das Packen mit der klassischen Huffman-Methode und die Prüfung gespeicherter Versionen. **UC2-SAS-Mehrteiler** schreibt `.p01`, `.p02` und weitere Teile. Beim Öffnen eines beliebigen Teils müssen die anderen Teile daneben liegen. DOS-UC2SEA-Sätze lassen sich über **Archive → Öffnen** als Daten lesen. Die UC2-Dateiliste zeigt die neueste Version eines Pfads. Die verwendete UC2-Bibliothek befindet sich im Alpha-Status. [UC2-Mehrteiler](https://raw.githubusercontent.com/evvaletov/uc2/main/docs/volumes.md)

**Selbstentpackendes DOS-UC2SEA** erzeugt eine DOS-EXE und bei Bedarf `.001`, `.002` und weitere Teile. `ARCO_DOSBOX_X` zeigt auf die DOSBox-X-EXE, `ARCO_UC2SEA_DIR` auf den Ordner mit den Originalwerkzeugen `UC.EXE`, `UC2SEA.EXE` und `DOS.SEA`. Beide Pfade müssen absolut sein. Arco prüft Namen und Inhalte gegen die Quellen, bevor es den Satz veröffentlicht. Der EXE-Basisname ist auf acht ASCII-Zeichen begrenzt. Root-Dateien mit dem Namen `SEASTART` und den ausführbaren DOS-Erweiterungen werden abgewiesen. [Originalwerkzeuge und Hinweise](https://github.com/evvaletov/uc2/tree/main/original/UC2_source), [DOSBox-X](https://dosbox-x.com/)

### Externe Packerprofile

Unter **Konfigurieren → Externe Packerprofile** lassen sich Profile anlegen, bearbeiten und löschen. Das Formular erwartet Profilname, absoluten EXE-Pfad und Archivendung auf drei Zeilen. Jedes weitere Argument steht auf einer eigenen Zeile. `{archive}` wird durch den temporären Zielpfad ersetzt. `{sources}` übergibt jeden ausgewählten Pfad als separates Argument. Alternativ erzeugt `@{list}` eine UTF-8-Dateiliste. Setze bei 7-Zip dann zusätzlich `-scsUTF-8`.

```text
7-Zip ZIP
C:\Program Files\7-Zip\7z.exe
zip
a
-tzip
-spd
{archive}
{sources}
```

**Archive → Mit Profil packen** arbeitet mit Kopien der ausgewählten Dateien in einem temporären Ordner. Arco prüft die Eintragsliste, Dateigrößen und die Archivinhalte, bevor es die Datei im anderen Fenster veröffentlicht. Für Formate wie RAR oder ARJ ist ein passender externer Packer erforderlich. Die Prüfung nutzt den für das jeweilige Format eingerichteten Leseadapter.
- Vorhandene Änderungszeitstempel erscheinen in den Archivfenstern und werden bei der Extraktion von Dateien übernommen. ZIP speichert seine klassischen Zeitstempel in lokaler Zeit mit Zwei-Sekunden-Auflösung.

### Dateifenster vergleichen und synchronisieren

`Umschalt+F2` markiert neuere und einseitig vorhandene Dateien. Im Menü **Markieren** kannst du zusätzlich einseitig vorhandene Ordner einbeziehen oder gleich große Dateien mit gleichem Zeitstempel ausblenden. Ein erneuter Aufruf des Ausblendbefehls zeigt die Dateien wieder an. In der Befehlszeile unterstützen `cm_CompareDirs` (`533`) und `cm_CompareDirsWithSubdirs` (`536`) auch die Parameter `1` für einseitige Einträge und `2` für einseitige Einträge und Größenunterschiede. `cm_DirMatch` (`534`) schaltet die Ansicht mit ausgeblendeten gleichen Dateien um. [Total-Commander-Befehlsreferenz](https://www.ghisler.ch/wiki/index.php?title=List_of_internal_commands)

Unter **Verzeichnisse synchronisieren** stehen beidseitiger Abgleich, Kopieren in eine Richtung und Spiegeln von links nach rechts zur Auswahl. Die Optionen umfassen Unterordner, leere Ordner, markierte Einträge, Inhaltsvergleich, Datum ignorieren und Dateimasken wie `*.txt;*.md | geheim*`. Für den Abgleich zweier lokaler Ordner lassen sich diese Optionen als benanntes Profil mit oder ohne das aktuelle Verzeichnispaar speichern. Die Vorschau zeigt Größe und Datum je Seite. Kopierrichtungen lassen sich je Eintrag auswählen. Bei **Spiegeln** erfordern die angezeigten Löschungen rechts eine ausdrückliche Bestätigung. Arco führt sie nach den Kopien aus und überspringt sie bei einem Kopierfehler.

**Befehle → Synchronisierungsprofile** öffnet die gespeicherte Auswahl direkt. Profile mit einem hinterlegten Verzeichnispaar zeigen beide Pfade an.

Mit **Verzeichnisse synchronisieren** kannst du einen lokalen Ordner mit einem ZIP- oder 7z-Archiv einschließlich `.001`-Mehrteilern abgleichen. Wenn das Archiv beschreibbar ist, kannst du Änderungen in beide Richtungen freigeben. Zum Spiegeln ins Archiv öffnest du es rechts. Arco prüft das neu aufgebaute Archiv vor dem Ersetzen und erhält die Verschlüsselung. Öffnest du zwei Archive, erhältst du eine schreibgeschützte Vergleichsliste. Bei anderen unterstützten Archivformaten kannst du aus dem Archiv in einen lokalen Ordner kopieren. Der Vergleich benötigt Platz für die temporär entpackten Daten. [Total-Commander-Hilfe zur Synchronisierung](https://www.ghisler.com/advanced.htm)

### FTP in den Dateifenstern

Wähle links oder rechts das gewünschte Dateifenster und öffne **Netz → FTP-Verbindungen** oder drücke `Strg+F`. Lege unter **Neu** ein benanntes Profil an und speichere es. Die Adresse enthält bei Bedarf Port und Startordner. Ein lokaler Startordner wird nach erfolgreicher Anmeldung im anderen Dateifenster geöffnet. `Strg+N` öffnet die Schnellverbindung.

Ein Profil speichert das Protokoll, den Benutzernamen, einen optionalen HTTP- oder SOCKS-Proxy, den aktiven oder passiven Modus und das Verzeichnisformat. Im automatischen Modus versucht Arco MLSD, danach Unix- oder IIS-LIST und schließlich eine reine Namensliste. Über **Netz → FTP-Unix-Rechte ändern** lassen sich die Rechte der Auswahl oktal setzen, sofern der Server MLSD und `SITE CHMOD` unterstützt.

Arco fragt das Passwort beim Verbinden ab. Mit aktivierter Speicheroption liegt es in der Windows-Anmeldeinformationsverwaltung des aktuellen Kontos auf diesem Rechner. Das JSON-Profil enthält dafür eine Referenz. **Importieren** zeigt unterstützte Verbindungen aus `wcx_ftp.ini` vor der Übernahme an. Vorhandene Profilnamen bleiben erhalten. Für übernommene Verbindungen fragt Arco die Passwörter beim Verbinden erneut ab. Die Vorschau kennzeichnet übersprungene Spezialkonfigurationen. [Ghisler-Hilfe zur FTP-Konfiguration](https://www.ghisler.com/efaqftp.htm), [Windows Credential Manager API](https://learn.microsoft.com/en-us/windows/win32/api/wincred/nf-wincred-credwritew)

FTP verwendet dieselbe Pfadleiste, Tabs, Sortierung, Auswahl und Navigation wie lokale Ordner. Doppelklick oder `Enter` öffnen Ordner und zeigen Dateien in der Vorschau an. `F3` öffnet ebenfalls die Vorschau. `F7` erstellt einen Ordner, `Umschalt+F6` benennt einen Eintrag um und `F8` löscht die Auswahl nach Bestätigung dauerhaft.

`F5` bereitet die Kopiervorschau für Dateien und Ordner zwischen lokal und FTP sowie zwischen zwei FTP-Ordnern vor. Für vorhandene Zieldateien fragt Arco beim Ausführen nach der Konfliktentscheidung. Du kannst einen automatisch gewählten freien Zielnamen übernehmen oder einen eigenen Dateinamen eingeben. Für FTP-Dateien bis 64 KiB kann der Dialog eine Text- oder Hexvorschau laden. Beim Kopieren zwischen FTP-Ordnern verwendet Arco je Datei eine temporäre lokale Kopie. **Netz → FTP-Verzeichnis anzeigen** öffnet die separate FTP-Ansicht mit dem Einzeldownload. **Netz → FTP trennen** schließt die betreffende Sitzung in allen Tabs und entfernt sie aus dem Verlauf. Das gespeicherte Profil verwendet für solche Tabs den zuletzt lokalen Ordner.

### FTP-Dateien bearbeiten

Drücke im FTP-Dateifenster `F4`, um eine Datei mit MLSD-Metadaten bis 64 MiB im eingestellten Editor zu öffnen. Speichere dort und wähle im Arco-Dialog **Änderungen hochladen**. Die Arbeitskopie lässt sich mehrfach speichern und übertragen. **Netz → Offene FTP-Bearbeitungen** öffnet den Dialog erneut.

Arco lädt die neue Fassung unter einem temporären Namen hoch und prüft die bisherige Serverdatei anhand ihrer Metadaten und ihres Inhalts vor dem Ersetzen. Ein Konflikt erhält die Arbeitskopie und die fremde Serverfassung. FTP stellt zwischen der Prüfung und dem Umbenennen keine gemeinsame Sperre bereit. Die neuen Dateirechte richten sich nach dem Server und der bereitgestellten Datei.

Die Arbeitskopien liegen unter `%LOCALAPPDATA%\Arco Commander\ftp-edits` und bleiben beim Trennen oder Beenden erhalten. Das Löschen einer Arbeitskopie verlangt eine Bestätigung.

### FTP mit einem lokalen Ordner abgleichen

Öffne einen FTP-Ordner und einen lokalen Ordner und wähle **Netz → FTP mit lokalem Ordner abgleichen**. In der Vorschau steht der lokale Ordner links und FTP rechts. Du kannst beide Richtungen, Unterordner, leere Ordner, markierte Einträge und Dateimasken wählen. Beim Spiegeln von lokal nach FTP werden zusätzliche Servereinträge nach separater Bestätigung und nur nach erfolgreichen Kopien gelöscht. Arco prüft vor dem Löschen erneut, ob die lokale Seite den Eintrag inzwischen enthält.

Für Abgleich und Schreiboperationen im Dateifenster benötigt Arco MLSD mit Typ, Dateigröße und UTC-Änderungsdatum für Dateien. Beim Inhaltsvergleich fragt Arco die per `FEAT` angebotenen Server-Prüfsummen ab. Es unterstützt `HASH` mit SHA-512, SHA-256, SHA-1 oder MD5 sowie `XSHA1`, `XMD5`, `MD5` und `XCRC`. Fehlt eine verwertbare Prüfsumme, lädt Arco die Datei zum Vergleich vorübergehend herunter. Downloads beim Abgleich übernehmen die Serverzeit. Uploads setzen den Zeitstempel, sofern der Server MFMT unterstützt. Arco überträgt Uploads unter einem temporären Namen und prüft den Zielstand vor der Veröffentlichung erneut. Wenn ein Server das direkte Ersetzen ablehnt, sichert Arco die alte Version vorübergehend per Umbenennung. Nach einem Verbindungsabbruch kann eine temporäre Datei oder Sicherung auf dem Server verbleiben. FTP bietet dabei keine gemeinsame Sperre gegenüber anderen Clients. [FTP-HASH-Entwurf](https://www.ietf.org/archive/id/draft-bryan-ftp-hash-08.html), [FTP-Synchronisierung in Total Commander](https://www.ghisler.com/screenshots/en/08.html)

## Bauen und starten

Voraussetzungen sind Rust 1.93 oder neuer und die MSVC-C++-Buildtools mit Windows SDK. Starte die Befehle in einer Developer PowerShell mit `cargo` im `PATH`.

```powershell
cargo build --locked --workspace
cargo run --locked -- --left 'C:\Quelle' --right 'C:\Ziel'
cargo test --locked --workspace --all-targets -- --test-threads=1
```

Für 32-Bit-Plugins muss neben `arco-commander.exe` zusätzlich `arco-plugin-host-x86.exe` liegen. In einer x86 Developer PowerShell lässt sich der Host so bauen und neben den x64-Build kopieren.

```powershell
rustup target add i686-pc-windows-msvc
cargo build --locked -p arco-plugin-host --target i686-pc-windows-msvc
Copy-Item target\i686-pc-windows-msvc\debug\arco-plugin-host.exe target\debug\arco-plugin-host-x86.exe
```

`--portable` legt das Sitzungsprofil neben der ausführbaren Datei ab. Mit `--profile <Datei>` lässt sich ein eigener Speicherort wählen.
Beim direkten Start von `arco-commander.exe` erscheint das Windows-Anwendungsfenster.

## Funktionsparität

Die Referenz bleibt Total Commander 11.58.1 für Windows. Die Funktionsparität ist noch offen. Dazu zählen unter anderem vollständige Plugin-Operationen und eingebettete Plugin-Vorschau, weitere Archivformate und Archivbearbeitung, vollständige TC-Befehls- und Profilkompatibilität, physische USB- und LPT-Transfers sowie Teile der Windows-Shell-Integration. Die Matrix führt die einzelnen Varianten und die bisher geprüften Abläufe auf.

- [Entwicklungsplan](PLAN.md)
- [Funktionsmatrix und Prüffälle](FUNKTIONSMATRIX.md)
- [Herstellerhilfe und Referenzindex für Dialoge, Einstellungen und Tastatur](TOTAL-COMMANDER-REFERENZ.md)
