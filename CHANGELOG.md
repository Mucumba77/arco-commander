# Änderungen

## 1.8.0 · 21. November 2026

- Bei F5 zwischen geeigneten FTP-Servern lässt sich der direkte FXP-Weg wählen. Die Kopiervorschau berücksichtigt Ordner, Masken und Zielkonflikte. Der Auftrag erscheint separat im Transfermanager.
- FTP-Direkttransfers (FXP) funktionieren auch über IPv6. Arco prüft Zielnamen am tatsächlich verbundenen Zielserver und meldet Ablehnungen und verkürzte Transfers im Auftrag.
- `arco-net` verarbeitet mit `rm` und `rmdir` mehrere Serverpfade. `rm` unterstützt Dateimasken, rekursive Ordner und den Server-Papierkorb.
- Fortsetzbare SFTP-Downloads behalten die bereits geladenen Teilbereiche auch nach einem Abbruch oder Neustart. Arco prüft die Teilstände und lädt die fehlenden Daten über die verfügbaren Verbindungen.
- Downloadlisten und F5-Netzwerkkopien können unabhängige Dateien parallel innerhalb desselben Auftrags übertragen. Verbindungen und Durchsatzlimit werden gemeinsam verwaltet.

- Große SFTP-Dateien lassen sich über mehrere Verbindungen parallel herunterladen. Arco prüft die Teilbereiche vor dem Freigeben der Zieldatei und teilt das Durchsatzlimit auf die Verbindungen auf.
- Das Durchsatzlimit für laufende Netzwerkaufträge ist im Transfermanager einstellbar. SFTP, SCP, der interne FTP-Transport und unterstützte Downloads übernehmen Änderungen im Datenstrom.
- Der Transfermanager führt unabhängige Netzwerkaufträge parallel aus. Er zeigt Fortschritt und Abbruch pro Auftrag an und lässt Aufträge mit überlappenden Schreibpfaden warten. Die Anzahl gleichzeitiger Aufträge ist einstellbar.
- Fortsetzbare FTP- und FTPS-Transfers verbinden sich nach einer Unterbrechung bis zu zweimal neu. Arco prüft Teildateien vor dem Fortsetzen und kontrolliert bei einer verlorenen Upload-Abschlussantwort den vollständigen Dateiinhalt.
- Netzwerkaufträge können eine eigene Transfervorgabe und ein Durchsatzlimit verwenden. F5 berücksichtigt die gewählte Vorgabe bereits in der Kopiervorschau. Gespeicherte Profile behalten ihre Werte.
- Rückübertragungen für dieselbe Serverdatei werden über Arco-Instanzen einer Windows-Anmeldesitzung hinweg koordiniert. Der lokale FTPS-Referenzlauf prüft konkurrierende Rückübertragungen in beiden TLS-Modi.
- SFTP- und SCP-Dateifenster zeigen symbolische Links mit eigenem Symbol, öffnen deren Ziele und löschen oder recyceln die Links als einzelne Servereinträge. Der Abgleich schützt gleichnamige Einträge auf beiden Seiten.
- Symbolische Serverlinks lassen sich über SFTP, SCP und FTP/FTPS mit `SITE SYMLINK` anlegen. SFTP und SCP unterstützen außerdem harte Links bei Serverunterstützung und das Ändern symbolischer Linkziele nach erneuter Prüfung. Dieselben Aktionen stehen in `arco-net` bereit.
- Server-Arbeitskopien können im integrierten Texteditor geöffnet und pro Datei nach dem Speichern automatisch zurückübertragen werden. Konflikte oder Abbrüche halten die Automatik an und erhalten die Arbeitskopie.
- WebDAV folgt Weiterleitungen für Ordner und Downloads. Profiloptionen erlauben separat andere Hosts oder Ports sowie den Wechsel von HTTPS auf HTTP. Arco übernimmt beide Einstellungen aus WinSCP und bindet Zugangsdaten an den ursprünglichen Server.
- WebDAV-Profile unterstützen die Windows-Anmeldung über Negotiate und eine Option für nicht kodierte Sonderzeichen in Ressourcenadressen. Der WinSCP-Import übernimmt die Kompatibilitätsoption. WebDAV-Downloads verwenden den IIS-Header `Translate: f`.
- Arco zeigt den S3-Schutzstatus MFA Delete an und unterstützt geschützte Versionslöschungen und Änderungen der Bucket-Versionierung im Dialog und im Skript. Einmalcodes werden nur über HTTPS gesendet.
- S3-Profile nehmen temporäre Schlüssel mit Sitzungstoken und eine IAM-Rollen-ARN direkt an. Token können im Windows-Anmeldeinformationsspeicher hinterlegt oder in Skripten über eine Umgebungsvariable übergeben werden. Der WinSCP-Import übernimmt die Rollen-ARN.
- S3-Bucket-Versionierung lässt sich anzeigen, aktivieren und aussetzen. Dateiversionen und Löschmarkierungen mit unveränderlicher ID lassen sich gezielt löschen. `arco-net` kann die Historie anzeigen, Versionen herunterladen und wiederherstellen sowie Bucket-Versionierung abfragen und ändern.
- S3-Profile unterstützen Virtual-Host-Adressierung und eine ausdrückliche Kostenübernahme für Requester-Pays-Buckets. Der WinSCP-Import übernimmt beide Einstellungen.
- S3-Multipart-Uploads lassen sich mit bestätigten Teilen über Sitzungs- und Programmneustarts fortsetzen. Die Oberfläche und `arco-net` bieten eine Funktion zum Verwerfen gespeicherter Uploads.
- Ältere S3-Dateiversionen lassen sich auf dem Server als neuer aktueller Stand wiederherstellen. Bei Versionen über 5 GiB verwendet Arco eine Multipart-Kopie mit Fortschrittsanzeige, übernimmt Tags und Metadaten und prüft den Zielstand vor dem Abschluss.
- SFTP und SCP unterstützen mehrere verifizierte SHA256-Hostschlüssel für einen Schlüsselwechsel. Die Verbindungsdetails zeigen den tatsächlich verwendeten Schlüssel. WinSCP- und OpenSSH-Importe übernehmen passende Alternativen. Skripte können die Schlüssel mit `open -hostkey=…` vorgeben oder eingrenzen.
- Netzwerkprofile können den HTTP-Proxy für die eingegebene Serveradresse aus den Windows-Einstellungen ermitteln. Arco wertet Proxy-Skripte und Ausnahmen für das Ziel aus und schlägt die Adresse zur Übernahme vor.
- SFTP und SCP können lokale Proxy-Programme über Standardeingabe und Standardausgabe verwenden. Netzwerkprofile können SSH-Zielnamen lokal auflösen. Der WinSCP-Import übernimmt beide Einstellungen.
- SFTP und SCP handeln bei HTTP-CONNECT Digest oder Basic aus. SOCKS5 bietet mit hinterlegten Zugangsdaten CHAP und die Benutzername-Passwort-Anmeldung an.
- Klartext-FTP lässt sich über einen HTTP(S)-Forward-Proxy öffnen. Arco zeigt HTML-Verzeichnislisten in beiden Dateifenstern und lädt Dateien und Ordner per HTTP GET herunter.
- SFTP und SCP unterstützen Telnet-/TCP-Proxys mit WinSCP-kompatiblen Befehlsvorlagen. Der WinSCP-Profilimport übernimmt Adresse, Vorlage und Benutzerkennung.
- FTP-Profile unterstützen Legacy-S/KEY-Anmeldungen nach RFC 1760 mit Sechswort-Antworten. RFC-2289-Anmeldungen behalten ihr Hexadezimalformat.
- FTP-Netzwerkprofile unterstützen die acht von WinSCP dokumentierten Gateway-Anmeldeverfahren und übernehmen die Verfahrenswahl aus WinSCP-Profilen. `SITE` und `OPEN` können sich vor der Zielauswahl am Gateway anmelden.
- FTP und der eingebaute SSH-Transport unterstützen SOCKS4 und SOCKS4a mit Benutzerkennung. Der WinSCP-Profilimport übernimmt die Kennung und die DNS-Auswahl.

- Benannte Arbeitsbereiche speichern beide Tab-Leisten, aktive Seite, Fensteraufteilung und gekoppelte Navigation. Sie lassen sich über das Menü, eine Windows-Verknüpfung oder `--workspace` öffnen.
- Arbeitsbereiche lassen sich automatisch speichern und wieder öffnen. Arco importiert lokale und entfernte WinSCP-Tabs aus einer INI-Datei oder der Windows-Registrierung und zeigt zuletzt genutzte Arbeitsbereiche optional in der Windows-Sprungliste an.
- Server-Prüfsummen für FTP, FTPS und SFTP sind im Netzwerkmenü und mit optionalem Algorithmus im Skript verfügbar.
- Netzwerkprofile lassen sich in verschachtelten Verbindungsordnern organisieren und filtern. Ordneränderungen erhalten bestehende Profilnamen und Verknüpfungen.
- Serverrechte sowie numerische Besitzer- und Gruppen-IDs lassen sich für SFTP und SCP einzeln oder rekursiv ändern. FTP und FTPS unterstützen rekursive Modusänderungen mit `SITE CHMOD`.
- Netzwerkprofile und Transfervorgaben speichern Regeln für die Schreibweise von Zielnamen und die Windows-Namenskonvertierung. Einzeltransfers, Listen und Ordnerkopien verwenden die Regeln auch im Skript.
- Netzwerkabgleich und Ordnerüberwachung ordnen umgewandelte Namen einander zu. Die Vorschau zeigt beide Pfade. Vor dem Löschen prüft Arco erneut, ob inzwischen ein gleichwertiger Gegenname existiert.
- Kopien zwischen zwei Serverfenstern berücksichtigen die Downloadregel der Quelle und die Uploadregel des Ziels. Die Vorschau zeigt beide Servernamen.
- Netzwerkskripte übernehmen `FileNameCase` und die reversible Windows-Namensumwandlung über WinSCP-kompatible `-rawtransfersettings`.
- SFTP- und SCP-Serverbefehle laufen über Arcos SSH-Verbindung. Befehlsvorlagen nehmen markierte Dateien oder den aktuellen Serverordner als Argumente. Im Skript stehen dafür `call` und `call -template` bereit.
- Benannte SSH-Befehlsvorlagen lassen sich über das Netzwerkmenü verwalten und per `call -saved` aufrufen.
- SSH-Befehlsvorlagen können Host, Benutzer, Port, Profilname und Sitzungsadresse als Argumente übergeben.
- SCP-Prüfsummen werden mit GNU-Prüfprogrammen am Server berechnet.
- Gespeicherte Tabs unterstützen auch native SFTP-, SCP-, WebDAV- und S3-Pfade.
- Netzwerkprofile können die Navigation beim Verbinden koppeln. Der WinSCP-Import übernimmt die Option `SynchronizeBrowsing`.

## 1.7.0 · 19. November 2026

- SFTP-Profile können die vorherige Serverdatei beim bestätigten Überschreiben im Server-Papierkorb sichern. Die Sicherung ist getrennt vom Papierkorb für Löschvorgänge einstellbar.
- Gekoppelte Navigation über Menü, Symbolleiste und Befehlspalette für die beiden aktiven lokalen oder Netzwerk-Tabs. Arco kann einen fehlenden Gegenordner nach Bestätigung anlegen.
- Einzeldateien lassen sich als authentifizierte age-Pakete mit verschlüsseltem Namen und Inhalt hochladen und entschlüsselt herunterladen. `put-encrypted` und `get-encrypted` bieten dieselbe Funktion im Skript.
- `ls` und `dir` unterstützen Serverordner und Dateimasken. Geplante Netzwerkskripte können beim Windows-Systemstart ausgeführt werden.

- SFTP-Anmeldung mit Ed25519-, ECDSA- und RSA-Schlüsseldateien, Windows OpenSSH-Agent und Pageant. SSH-Sprungserver werden als eigene Profile mit separater Hostschlüsselprüfung eingebunden.
- SFTP mit HTTP-CONNECT und SOCKS5, einstellbarem SSH-Zeitlimit und Keepalive.
- Fortsetzbare SFTP-Downloads mit geprüftem Wiederanlauf nach Verbindungsverlust, Teildateiprüfung und Bytefortschritt. Serverpfade und URLs lassen sich aus der Dateiauswahl kopieren.
- SSH-Serverabfragen per Keyboard-Interactive einschließlich mehrerer Eingabeschritte und zweitem Faktor nach Schlüsselanmeldung. SCP-Einzeldateitransfer aus gespeicherten SSH-Profilen.
- Fortsetzbarer SFTP-Upload zu einem neuen Ziel mit geprüfter Teildatei und automatischem Wiederanlauf. SFTP-Änderungszeit für markierte Einträge und SHA-256-Prüfsummen für markierte Dateien.
- Netzwerkprofile für SCP-Dateifenster, WebDAV und S3. Die Anbieter nutzen dieselben Tabs, Kopierdialoge, Konfliktregeln und den Transfermanager.
- WebDAV über HTTP und HTTPS mit geschützten Schreiboperationen. S3-Endpunkte mit SigV4, Bucket- und Präfixnavigation, Multipart-Uploads mit Bereinigung nach Abbruch und Objekt-Tag-Bearbeitung.
- Rekursive Serversuche und bidirektionaler Abgleich mit lokalen Ordnern für SCP, WebDAV und S3. Dateien lassen sich auch zwischen Netzwerk-Dateifenstern mit unterschiedlichen Protokollen kopieren.
- Windows-Kerberos-Anmeldung für SFTP und SCP über GSSAPI mit SSH-Hostschlüsselprüfung.
- WebDAV-Schreibsperren anzeigen, anlegen, verlängern und aufheben. Eigene Sperrtoken gelten für Dateioperationen in derselben Verbindung.
- AWS-Anmeldekette für S3 mit gemeinsam genutzten AWS-Profilen und erneuerbaren temporären Zugangsdaten. Die S3-Versionshistorie zeigt Dateiversionen und Löschmarkierungen. Ältere Versionen lassen sich separat herunterladen.
- Zuschaltbare Kerberos-Ticketdelegation pro SSH-Profil und WinSCP-INI-Import mit Vorschau.
- Hintergrundüberwachung lokaler Ordner mit Übertragung stabiler Änderungen zum Server. `arco-net.exe` führt UTF-8-Skripte fensterlos mit gespeicherten Netzwerkprofilen aus und gibt JSON-Ereignisse aus.
- Netzwerkautomatisierung für .NET und x64-COM mit strukturierten Ergebnissen, stdin-Skripten und den Befehlen `lpwd`, `stat`, `chmod` und `checksum`.
- Importvorschau für WinSCP, PuTTY und KiTTY aus der Windows-Registrierungsdatenbank, FileZilla, OpenSSH und Ghisler-SFTP-Profile. `known_hosts` kann fehlende SSH-Fingerabdrücke ergänzen.
- Getrennte Proxy-Anmeldung für HTTP Basic und SOCKS5. Proxy-Passwörter bleiben in den Windows-Anmeldeinformationen.
- Direkte Sitzungs-URLs, Arco-Profil-Links und Windows-Verknüpfungen für Serverordner. `arco-net.exe` kann Sitzungs-URLs in Skripten öffnen.
- Tägliche Netzwerkskripte in der Windows-Aufgabenplanung mit Verwaltung in Arco und JSONL-Protokollen je Lauf.
- Datei-URLs öffnen den Serverordner mit Dateifokus oder laden nach Wahl eines Zielpfads herunter. Arco erstellt Datei-Links und Windows-Verknüpfungen, Skripte können `get-url` verwenden.
- Geplante Skripte können auch bei abgemeldetem Benutzer unter dem aktuellen Windows-Konto ausgeführt werden.
- Die Windows-Zuordnung für `arco://` lässt sich pro Benutzer in Arco aktivieren und entfernen.

- Eingebauter GSSAPI-Schlüsselaustausch nach RFC 8732 für SFTP und SCP unter Windows. Profile können Kerberos beim Austausch verlangen und einen ausdrücklich vorgegebenen Dienstprinzipal zur Serverprüfung verwenden.
- Änderung der Server-Zeitstempel auch in SCP-Fenstern. Die Netzwerkkommandozeile listet lokale Dateien per `lls` und protokolliert Meldungen per `echo`.

- `arco-net synchronize` unterstützt Vorschau, beide Transferrichtungen, Spiegeln, Löschsynchronisierung, Dateimasken und Prüfsummenvergleich.
- `arco-net` hält mehrere Netzwerksitzungen offen und wechselt gezielt mit `session` zwischen ihnen.
- Netzwerkprofile speichern Übertragungsmodus, Durchsatzlimit, Dateimasken und Zeitstempelregeln. Die auswählbaren Vorgaben greifen im Ordnerabgleich und bei den einzelnen Netzwerktransfers.
- Benannte Transfervorgaben lassen sich verwalten, fest einem Netzwerkprofil zuweisen oder anhand von Server, Benutzer und beteiligten Ordnern automatisch auswählen.
- Der Ordnerabgleich übernimmt auf Wunsch nur Änderungszeiten für gemeinsam vorhandene Dateien. Der Größenfilter ist standardmäßig aktiv und lässt sich auch in Netzwerkskripten umstellen.
- `put`, `get` und `get-url` akzeptieren Vorgabe, Durchsatzlimit, Dateimaske, Übertragungsmodus und Übernahme der Änderungszeit als Optionen pro Befehl.
- `get` und `put` übertragen Dateilisten und Maskentreffer mit Vorabprüfung der Zielnamen. `-latest` wählt die neueste Datei aus und `option failonnomatch` steuert leere Treffer.
- `-resume` aktiviert die geprüfte binäre Wiederaufnahme von FTP- und SFTP-Transfers auch in Skripten.
- Windows-Aufgaben können einmalig, täglich, wöchentlich, monatlich oder bei der Anmeldung am aktuellen Windows-Konto starten.
- Die Ordnerüberwachung kann lokale Löschungen auf bestätigte und unveränderte Serverdateien übertragen. Die Option ist anfangs ausgeschaltet.
- `keepuptodate` bietet die Ordnerüberwachung im Skript mit Startabgleich, Masken, Laufzeitbegrenzung und Löschoption.
- `get` und `put` übertragen ausgewählte Ordnerbäume einschließlich leerer Verzeichnisse. `-recursive` bezieht bei Platzhaltern auch Ordner ein.
- Der Server-Papierkorb kann Löschvorgänge abfangen und Einträge in einen ausgewählten Serverordner wiederherstellen.
- SFTP- und SCP-Dateifenster öffnen ein SSH-Terminal im aktuellen Serverordner oder Anmeldeordner. Arco verwendet dabei die SSH-Einstellungen der aktiven Verbindung.

## 1.6.0 · 19. November 2026

- Eingebautes SFTP mit Hostschlüsselprüfung, Passwort- und RSA-Schlüsselanmeldung, binären Transfers und Abgleich mit einem lokalen Ordner.
- Rekursive Dateimaskensuche in FTP-, FTPS- und SFTP-Fenstern mit Sprung zum Treffer. Die Funktionsmatrix führt die übrigen WinSCP-Funktionen als integrierte Erweiterungen.
- FTP-Profile mit IP-Version, Serverzeichensatz, eigenem LIST-Format, versteckten Dateien und FTPS-Zertifikatsoptionen. Unix-/IIS-/VMS-Listings, USER-Gateway, MODE Z im Binär- und Textmodus sowie RFC-2289-OTP mit MD4, MD5 und SHA-1.
- FTP-Serverbefehle, Verbindungsdetails und Rohliste. Download von Pfaden, URLs und Textlisten, Textmodus und geprüfte Wiederaufnahme von Uploads.
- Loopback-Referenzen für FTPS, OTP, IPv6, MODE Z und ältere Prüfsummenbefehle. [FTP-Bedienung und verbleibende Varianten](docs/FTP.md).
- Portables Paket für Windows 10 22H2 und Windows 11 (x64) mit [SFTP-Anleitung](docs/SFTP.md). [Release-Hinweise](docs/releases/v1.6.0.md).

## 1.5.0 · 17. November 2026

- Persistente Archiveinstellungen für Kompressionsmethoden/-raten, AES/ZipCrypto, 7z-Solid/Header sowie eigene Speicher-/Threadwerte, TAR/GZIP, Prüfung, Archivdatum und Zonenübernahme.
- Gemeinsame Packauswahl für eingebaute und externe Packer: Pfade, Rekursion, Basisordner, getrennte Archive und Quellen erst nach vollständiger Prüfung verschieben.
- Passwortgeschütztes ACE, ACE-Mehrteiler und verschlüsseltes ZIP-Brotli. Transaktionale Ergänzung, Umbenennung und Löschung für weitere Archiv-/Teilesätze; vollständige Veröffentlichung externer Ergebnisse mit Kollisionsschutz.
- Ergänzende Rust-Leser für LHA-Header 3 und CAB-LZX, formatabhängige Volume-/SFX-Erkennung, TGZ-Dateibaum sowie gemeinsamer Zonenparameter einschließlich WCX.
- TC-Packerimport, neue englische/französische Texte, unabhängiger Archivkorpus und wiederholbarer Referenzlauf. [Bedienung](docs/ARCHIVE.md), [Nachweise](docs/AREA4-ABSCHLUSS.md).
- ARC-01–ARC-07 im definierten Matrixumfang implementiert, 7/7 IDs. 351 Tests plus 16 explizite Archiv-Referenztests bestanden; Fenster-/Originalencoderabnahme separat offen. Keine Anwendung oder erzeugtes SFX gestartet.
- Portables Windows-x64-Paket einschließlich 7-Zip 26.06, Lizenz und zugehörigem Quellcode. ACE/UC2 verwenden weiterhin eine separat eingerichtete optionale Python-Umgebung. Paketierung und Windows-CI können Anwendung und SFX vollständig ungestartet lassen.


## 1.4.0 · 16. November 2026

### Oberfläche, Navigation und Dateioperationen

- Portable WDX-/WFX-Spalten, Layoutoptionen, Sortierung und getrennte Standard-/Plugin-/eigene Ansichten pro Tab. Native TC-WFX-Spaltennummern werden unabhängig von lokalen Spaltensätzen importiert und exportiert.
- Zusammengesetzte native/WDX-/WFX-Spalten über einen gemeinsamen Hintergrundleser, typgerechte Einzelwerte, SDK-Abfragebündelung, Abbruch und begrenzte Caches. Zusätzliche TC-Spaltenoptionen, automatische Modusbefehle und Tab-Symbole.
- Windows-Orte für Systemsteuerung, Schriftarten und Drucker. Stabile gespeicherte Shell-Identitäten auch für Anbieter ohne erneut auflösbaren Anzeigenamen.
- Ignorierregeln beim Archivabgleich beziehen sich auf ursprüngliche Archivpfade. Ignorierte Gegenstücke werden beim Spiegeln erhalten.
- Unveränderte Windows-Parameterzeilen bei `.bar`-Import/-Export und im Buttoneditor, `%Q`, `%B`-Varianten, `%Y&y` und FTP-Pfadparameter.
- Rekursives Kopieren/Verschieben von Junctions und symbolischen Datei-/Verzeichnislinks ohne Traversierung ihrer Ziele. Metadatenoptionen behandeln das Linkobjekt; Linkerstellung benötigt passende Windows-Rechte.
- Klassische OLE-Verschiebungen mit geschützten Verzeichnis-/Hardlink-Snapshots, stabilen Identitäten und geprüftem Rückmeldeprotokoll. CF_HDROP-Fallback für nicht vom Shell-Parser akzeptierte Dateipfade.
- Ergänzte englische/französische Fachdialoge, Hilfetexte, Auswahlaktionen und dynamische Meldungen. Dateinamen und technische Eingabeformate bleiben erhalten.


### Veröffentlichung und Prüfung

- UI-01–UI-10 und FILE-01–FILE-10 sind im definierten Matrixumfang implementiert: jeweils 10/10 IDs. Die ersten drei Bereiche sind damit in der Implementierung abgeschlossen.
- 343 automatisierte Tests in acht Suiten bestanden; 41 explizite Umgebungsfälle im Standardlauf ausgelassen. Zusätzlich vier SDK-Referenztests für ANSI-/Unicode-WFX und zusammengesetzte WDX-/WFX-Spalten bestanden.
- Portables Windows-x64-Paket mit aktuellen Plugin-Hosts beider Architekturen (Protokoll 13), ZIP-SFX, Bedienungsanleitung und Abschlussnachweis für die Bereiche 1 und 2.
- Paketierung mit `--no-app-run`: kein Start von Arco Commander, auch nicht für `--version` oder `--help`. Fenster-, Rechte- und Hardwareabnahme bleiben separat offen. Der Symlink-Referenzlauf konnte seine Quellen hier wegen Windows-Fehler 1314 nicht anlegen.

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
