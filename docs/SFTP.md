# SFTP, SCP, WebDAV und S3 in Arco Commander

Unter **Netz → Netzwerkprofile …** legst du die Verbindung an und öffnest sie in einem der beiden Dateifenster. Für **SFTP (SSH)** gibst du Serveradresse, Benutzername und den mit dem Serverbetreiber abgeglichenen SHA256-Fingerabdruck des Hostschlüssels ein. In der Adresse kannst du einen Startordner hinterlegen, etwa `sftp://server:22/projekte/`.

## Anmeldung und Verbindungsweg

- **Passwort oder Schlüsseldatei:** Für eine Passwortanmeldung bleibt das Feld für die Schlüsseldatei leer. Für Ed25519, ECDSA oder RSA gib den absoluten Pfad zur privaten Schlüsseldatei ein. OpenSSH, PEM, PKCS#8 und PuTTY PPK werden eingelesen. Das Passwortfeld nimmt dann die Schlüsselpassphrase auf. Zugangsdaten können im Windows-Anmeldeinformationsspeicher abgelegt werden.
- **OpenSSH-Agent oder Pageant:** Lade den Schlüssel in den Agenten und wähle ihn als Anmeldemethode. Arco nutzt die lokale Windows-Named-Pipe aus `SSH_AUTH_SOCK` beziehungsweise die Standard-Pipe des Windows OpenSSH-Agenten oder die lokale Pageant-Schnittstelle. Der Agent signiert lokal.
- **Windows-Kerberos (GSSAPI)** verwendet die Kerberos-Tickets des Windows-Kontos für SFTP und SCP. Im Standardmodus prüft Arco den SSH-Hostschlüssel und meldet sich anschließend per `gssapi-with-mic` an. Als Dienstprinzipal verwendet es standardmäßig `host/<DNS-Name>`. [Windows SSPI und Kerberos](https://learn.microsoft.com/en-us/windows/win32/secauthn/sspi-kerberos-interoperability-with-gssapi)
- **GSSAPI-Schlüsselaustausch verlangen** aktiviert Kerberos bereits beim SSH-Schlüsselaustausch. Arco bietet Curve25519/SHA-256, Group 16/SHA-512 und Group 14/SHA-256 an. Windows bestätigt den Dienstprinzipal und verifiziert den Austausch-Hash. Ein im Profil hinterlegter SHA256-Fingerabdruck muss zusätzlich mit dem SSH-Hostschlüssel übereinstimmen. Für Server ohne Hostschlüssel lässt du den Fingerabdruck leer und trägst den vertrauenswürdigen Dienstprinzipal ausdrücklich ein. Bei fehlendem gemeinsamen Verfahren bricht die Verbindung ab. [RFC 4462](https://www.rfc-editor.org/rfc/rfc4462.html) · [RFC 8732](https://www.rfc-editor.org/rfc/rfc8732.html)
- Die Option **Kerberos-Tickets an diesen Server delegieren** verlangt die Delegation im Schritt `gssapi-with-mic` nach der Serverprüfung. Ohne Delegation versucht Arco bei aktivem GSSAPI-Schlüsselaustausch zuerst die Anmeldung über den verifizierten Austauschkontext (`gssapi-keyex`). Bietet der Server nach einer vollständigen Ablehnung `gssapi-with-mic` an, versucht Arco dieses Verfahren. [GSSAPI-Anmeldung nach RFC 4462](https://www.rfc-editor.org/rfc/rfc4462.html#section-4)
- **Sprungserver:** Lege für jeden Sprungserver ein SFTP-Profil an und wähle im Zielprofil das davorliegende **Sprungserverprofil** aus. Sprungprofile können selbst einen Vorgänger haben. Arco verwendet pro Station die konfigurierte Serverprüfung und Anmeldemethode. In einer Kette sind bis zu acht Sprungserver möglich. Die jeweils vorherige Station löst den Namen der folgenden Station auf.
- **Keyboard-Interactive und zweiter Faktor:** Aktiviere die Option im betreffenden SSH-Profil, auch bei einem Sprungserver. Arco zeigt die Fragen des Servers einzeln an und maskiert vertrauliche Felder. Die Option funktioniert als Anmeldemethode und nach einer vom Server teilweise bestätigten Schlüssel-, Agent- oder Passwortanmeldung. Bestätigungscodes und Antworten werden nur für den jeweiligen Anmeldevorgang verwendet.
- **Proxy.** Unterstützt werden `http://proxy:8080` (HTTP-CONNECT), `socks5://proxy:1080` (lokale DNS-Auflösung) und `socks5h://proxy:1080` (DNS-Auflösung durch den Proxy). Speichere zuerst die Adresse im Netzwerkprofil und öffne danach **Proxy-Anmeldung …**. Arco nutzt HTTP Basic oder die SOCKS5-Anmeldung mit Benutzername und Passwort. Das Proxy-Passwort liegt getrennt vom Serverpasswort in den Windows-Anmeldeinformationen. HTTP-CONNECT ohne TLS und SOCKS5 übertragen die Proxy-Anmeldung vor dem verschlüsselten SSH-Tunnel. Bei einer Sprungserverkette gehört der Proxy ins Profil der ersten Station. [RFC 7617](https://www.rfc-editor.org/rfc/rfc7617.html) · [RFC 1929](https://www.rfc-editor.org/rfc/rfc1929.html)
- **Zeitlimit und Keepalive:** Das SSH-Zeitlimit beträgt standardmäßig 15 Sekunden je Station beim Verbindungsaufbau und je SFTP-Anfrage. Keepalive lässt sich pro Profil in Sekunden aktivieren.

Die **Netzwerk-Schnellverbindung** akzeptiert ebenfalls eine `sftp://`-Adresse. Sie fragt den Fingerabdruck und anschließend das Passwort ab. Für eine Schlüsseldatei verwende ein Profil.

## Transfervorgaben

Unter **Netz → Transfervorgaben verwalten …** legst du benannte Vorgaben für Durchsatzlimit, Dateimaske, Übertragungsmodus und Änderungszeiten an. Unter **Netz → Netzwerkprofile … → Transfervorgaben** kannst du eine davon fest für die Verbindung wählen. Bei **Automatische Auswahl** prüft Arco beim Auftragsstart die Regeln von oben nach unten. Die erste passende Regel gilt. Ohne Treffer verwendet Arco die Einstellungen des Netzwerkprofils. [WinSCP-Auswahlregeln](https://winscp.net/eng/docs/ui_transfer_preset)

Eine Regel kann Server, Benutzer und lokalen sowie entfernten Ordner gemeinsam abgleichen. Leere Felder gelten für jeden Wert. Mehrere Muster trennst du durch `;`, Ausschlüsse folgen nach `|`, und `**` erfasst Unterordner. Servernamen und lokale Windows-Pfade werden ohne Beachtung der Groß- und Kleinschreibung verglichen. Auf Serverpfaden und beim Benutzer zählt die Schreibweise. Beim Kopieren zwischen zwei Servern bleibt das Feld für den lokalen Ordner leer. Die Pfeiltasten-Schaltflächen im Dialog ändern die Reihenfolge. Beim Duplizieren ist die Auswahlregel der Kopie zunächst deaktiviert.

FTP und FTPS bieten **Binär**, **Text (ASCII)** und **Automatisch nach Dateiendung** für einzelne Uploads und Downloads. Andere Protokolle verwenden aus derselben Vorgabe den Binärmodus. Abgleich und Kopien im Dateifenster übertragen unveränderte Dateibytes. [WinSCP-Transfervorgaben](https://winscp.net/eng/docs/transfer_settings)

Die Maske verwendet Semikolon für mehrere Muster und `|` vor Ausschlüssen. Sie gilt für die Netzwerk-Transferdialoge, den Ordnerabgleich, die Ordnerüberwachung und `put`/`get` in Skripten. Durchsatzlimits gelten pro Transfer. Die Übernahme von Server-Zeitstempeln beim Upload hängt vom Protokoll und Server ab. Bei FTP braucht sie MFMT, bei SFTP und SCP die passenden Schreibrechte. WebDAV und S3 unterstützen in Arco die Übernahme beim Download.

## Änderungszeiten abgleichen

Aktiviere im Synchronisierungsdialog **Nur Änderungszeiten abgleichen**. Die Vorschau zeigt Änderungen für Dateien, die auf beiden Seiten vorhanden sind. Standardmäßig ist **Nur bei gleicher Dateigröße** aktiv. Bei **Beide Seiten** wird die jeweils neuere Änderungszeit übernommen. Bei einer festen Richtung gibt die gewählte Quelle die Zeit vor. Arco gleicht sekundengenau ab und ändert ausschließlich die Änderungszeit am Ziel. Inhalt, Verzeichnisstruktur und einseitig vorhandene Dateien bleiben erhalten. [WinSCP-Zeitstempelabgleich](https://winscp.net/eng/docs/task_synchronize_full)

Der Modus unterstützt zwei lokale Ordner sowie FTP/FTPS mit MFMT, SFTP und SCP. Bei WebDAV und S3 wählst du **Server → lokal**. Mit `synchronize … -timestamps -preview` erreichst du dieselbe Vorschau für Netzwerkordner im Skript.

## SSH-Terminal

Für die offene SFTP- oder SCP-Verbindung findest du unter **Netz → Serverdateien** die Befehle **SSH-Terminal im Serverordner öffnen** und **SSH-Terminal im Anmeldeordner öffnen**. Arco öffnet eine eigene Windows-Konsole und baut über denselben eingebauten SSH-Transport eine zweite Verbindung auf. Anmeldemethode, Hostprüfung, Proxy und Sprungserver stammen aus der aktiven Verbindung. Die Auswahl im Serverordner setzt eine Unix-kompatible Shell voraus. Der Anmeldeordner verwendet die Standardshell des Servers und funktioniert auch mit Windows-SSH-Servern. [Windows-Konsole und VT-Sequenzen](https://learn.microsoft.com/en-us/windows/console/console-virtual-terminal-sequences) · [WinSCP-Terminalintegration](https://winscp.net/eng/docs/integration_putty)

## Dateioperationen

- Dateien und Ordner im Dateifenster öffnen. Kopien, Änderungen und Löschungen verwenden die vorhandenen Dialoge und den Transfermanager.
- Binäre Uploads werden zuerst unter einem temporären Namen übertragen. Arco prüft den erwarteten Zielstand und die Größe vor dem Umbenennen. Bei einem Fehlschlag meldet Arco, wenn temporäre Dateien oder eine Sicherung auf dem Server geprüft werden müssen.
- Über **Netz → FTP herunterladen → Fortsetzbaren Download verwenden** startest du einen Download, dessen Teildatei bei einem Abbruch erhalten bleibt. Bei vorübergehenden Verbindungsfehlern versucht Arco bis zu zweimal, den Transfer fortzusetzen. Nach einem manuellen Abbruch kannst du dieselbe Option mit demselben Ziel erneut aufrufen. Arco gleicht Größe, Änderungszeit und den überlappenden Dateibereich ab. Die fertige Datei erscheint nach der Abschlussprüfung. Ein bereits vorhandenes Ziel bleibt erhalten.
- Über **Netz → FTP hochladen → Fortsetzbar · Ziel muss neu sein** bleibt bei einem Abbruch eine `.filepart`-Datei im Serverordner. Wiederhole den Auftrag mit derselben Quelle und demselben Ziel, um den bereits übertragenen Teil zu prüfen und fortzusetzen. Bei einem vorübergehenden Verbindungsfehler versucht Arco die Fortsetzung automatisch bis zu zweimal. Arco prüft den vollständigen Inhalt auf dem Server, bevor es die fertige Datei unter dem gewünschten Namen veröffentlicht.
- **Dateien → Zwischenablage → Dateipfade kopieren** kopiert die ausgewählten Serverpfade. **Netz → Server-URLs kopieren** kopiert die entsprechenden URLs. URLs enthalten keine Anmeldedaten.
- **Netz → Server-Änderungszeit setzen …** setzt einen UTC-Zeitstempel für markierte SFTP- und SCP-Einträge. **Netz → SFTP-Prüfsummen berechnen** liest markierte reguläre Dateien als Datenstrom und berechnet SHA-256 lokal ohne Arbeitskopie.
- **Alt+F7** sucht in den Netzwerk-Dateifenstern rekursiv nach Dateimasken, wenn der Server die Eintragstypen liefert. Treffer lassen sich im Dateifenster fokussieren. Die Ignorierliste gilt auch für die Suche.
- **Netz → Serverordner mit lokalem Ordner abgleichen** vergleicht einen lokalen Ordner mit einem Netzwerk-Dateifenster. Änderungen werden vor der Ausführung angezeigt. Für den Inhaltsvergleich liest Arco die betroffenen Dateien vom Server.

Bei einem Server mit ausschließlich GSSAPI-Schlüsselaustausch muss das Verbindungsprofil diesen ausdrücklich aktivieren.

## Verbindungsprofile und SSH-Hostschlüssel importieren

Öffne **Netz → Verbindungsprofile importieren …**. Die Vorschau zeigt Sitzungen, Namenskonflikte und Einstellungen, die eine manuelle Einrichtung brauchen. Passwörter gibst du beim ersten Verbinden ein. [WinSCP-Importquellen](https://winscp.net/eng/docs/ui_import)

- **WinSCP, PuTTY und KiTTY.** Gespeicherte Profile lassen sich aus der Registrierungsdatenbank des aktuellen Benutzers übernehmen. WinSCP unterstützt außerdem exportierte INI-Dateien. Ein ausdrücklich aktivierter GSSAPI-Schlüsselaustausch erscheint in der Importvorschau. Fehlt der Fingerabdruck, ist vor dem Verbinden der Dienstprinzipal des Serverbetreibers einzutragen. Arco berechnet aus gespeicherten Ed25519-, ECDSA- und RSA-Schlüsseln den SHA256-Fingerabdruck für den passenden Host und Port.
- **FileZilla.** Wähle `sitemanager.xml` oder einen XML-Export mit Site-Manager-Profilen. Arco übernimmt FTP, FTPS und SFTP, Ordnernamen als Teil der Profilnamen sowie unterstützte Startpfade. FileZillas FTP-Modus mit optionaler Verschlüsselung wird als explizites FTPS übernommen.
- **OpenSSH.** Wähle eine `config`-Datei. Arco übernimmt konkrete Host-Aliase und passende Standardregeln. Eine `known_hosts` aus demselben Ordner liefert die SSH-Fingerabdrücke. Eine ausdrückliche Vorgabe `GSSAPIKeyExchange yes` wird für Kerberos-Profile übernommen. Ein abweichender Dienstprinzipal wird in Arco manuell eingetragen. `Include`, `Match`, Sprungserver und eigene Proxybefehle brauchen derzeit eine manuelle Zuordnung. Beim separaten Import von `known_hosts` gleicht Arco vorhandene Profile mit konkreten oder gehashten Host-/Port-Einträgen ab und ergänzt fehlende Fingerabdrücke. Widerrufene oder mehrdeutige Schlüssel brauchen eine manuelle Prüfung. [OpenSSH-Clientkonfiguration](https://man.openbsd.org/ssh_config)
- **Total Commander.** Wähle `sftpplug.ini` für SFTP- und SCP-Sitzungen. Bei einem älteren MD5-Fingerabdruck verlangt Arco vor dem Verbinden den SHA256-Fingerabdruck. [Ghisler SFTP-Plugin](https://www.ghisler.com/plugins.htm)

## Ordnerüberwachung

Öffne einen lokalen Ordner neben dem gewünschten Serverordner und wähle **Netz → Lokalen Ordner überwachen …**. Die Überwachung erfasst den lokalen Ausgangsstand, prüft Unterordner wiederholt und veröffentlicht neue oder geänderte Dateien nach zwei übereinstimmenden Prüfläufen. Änderungen können vorhandene Serverdateien ersetzen. **Netz → Ordnerüberwachung beenden** hält den Hintergrundauftrag an. [WinSCP-Ordnerüberwachung](https://winscp.net/eng/docs/task_keep_up_to_date)

Mit **Löschungen auf dem Server übernehmen** prüft Arco beim Start, welche Dateipaare denselben Inhalt haben. Später erfolgreich hochgeladene Dateien kommen hinzu. Fehlt eine solche Datei lokal in zwei aufeinanderfolgenden Prüfungen, prüft Arco die Serverdatei erneut anhand von Metadaten und Inhalt. Bei einer Abweichung hält die Überwachung an. Reine Serverdateien und Serverordner bleiben erhalten. Voreingestellt ist **Serverdateien behalten**. Im Skript ist die gleiche Überwachung mit `keepuptodate` verfügbar. [Skriptbefehl](AUTOMATISIERUNG.md#ordnerüberwachung-per-skript)

## Server-Papierkorb

Aktiviere unter **Netz → Netzwerkprofile … → Transfervorgaben** die Option **Gelöschte Servereinträge in den Papierkorb verschieben**. Sie gilt für reguläre Löschvorgänge im Netzwerk-Dateifenster, im Skript und beim Abgleich. Arco speichert die Einträge unter `.arco-trash` im Startordner des Profils. Jeder Eintrag bekommt einen eigenen Ordner mit Zeitstempel und behält seinen Namen. Bei F8 kann ein ausgewählter Ordner mitsamt Inhalt verschoben werden. Dauerhaftes Löschen und Löschen innerhalb des Papierkorbs entfernen die ausgewählten Einträge unmittelbar. [WinSCP Server-Papierkorb](https://winscp.net/eng/docs/ui_login_recycle_bin)

Über **Netz → Serverdateien → Server-Papierkorb öffnen** wechselst du zum Papierkorb. Markiere dort einen Eintrag und wähle **Servereintrag wiederherstellen …**. Gib den Zielordner relativ zum Netzwerk-Startordner oder als vollständige Server-URL an. `.` steht für den Startordner. Arco prüft den Zielnamen vor dem Verschieben und meldet einen Konflikt, wenn er bereits belegt ist.

Arco unterstützt den Papierkorb für SFTP, SCP, FTP, FTPS und WebDAV mit serverseitigem Verschieben. S3 bietet seine eigene Versionshistorie.

Bei SFTP kannst du zusätzlich **Alte Serverdateien beim Überschreiben im Papierkorb sichern** aktivieren. Die Option funktioniert unabhängig vom Löschen in den Papierkorb. Arco lädt eine neue Version zuerst unter einem temporären Namen hoch und prüft sie. Anschließend verschiebt Arco die vorherige Version in den Papierkorb und veröffentlicht die neue. Schlägt die Veröffentlichung fehl, versucht Arco, das Original am bisherigen Pfad wiederherzustellen. Ist dieser Pfad belegt oder der Server nicht erreichbar, kannst du die alte Version über den gemeldeten Papierkorbpfad wiederherstellen.

## Navigation koppeln

Öffne die gewünschten Startordner in zwei lokalen oder Netzwerk-Tabs. Aktiviere **Navigation → Navigation koppeln oder lösen** oder das Kettensymbol in der oberen Leiste. Wenn du in einem Fenster den Ordner wechselst, öffnet Arco auf der anderen Seite denselben Pfad relativ zu deren Startordner. Meldet das Dateisystem einen fehlenden Gegenordner, bietet Arco an, ihn nach Bestätigung anzulegen. Die Kopplung gilt für das ausgewählte Tab-Paar innerhalb der Sitzung und pausiert beim Wechsel auf andere Tabs. Wenn du einen gekoppelten Tab mit einem anderen Server verbindest oder die Verbindung trennst, wird die Kopplung aufgehoben. [Synchronized browsing in WinSCP](https://winscp.net/eng/docs/task_navigate#synchronized_browsing)

## Dateien clientseitig verschlüsseln

Markiere im lokalen Fenster eine Datei und öffne im Gegenfenster den Zielordner auf dem Server. Wähle **Netz → Serverdateien → Datei verschlüsselt hochladen …** und gib eine Passphrase mit mindestens zwölf Zeichen ein. Arco verschlüsselt die Datei samt Originalnamen in einem einzelnen Paket. Auf dem Server erscheint ein zufällig benanntes `.arco.age`-Paket.

Markiere das Paket im Serverfenster und wähle **Datei entschlüsselt herunterladen …**. Arco prüft den gesamten verschlüsselten Inhalt und veröffentlicht die Datei unter ihrem Originalnamen im gegenüberliegenden lokalen Ordner. Bei falscher Passphrase, veränderten Daten oder einem bereits belegten Ziel bricht der Auftrag ab. Die wirksame Transfervorgabe prüft den ursprünglichen Dateinamen. Die Passphrase wird pro Auftrag eingegeben und nicht im Netzwerkprofil gespeichert.

Die Pakete verwenden age v1 mit einem TAR-Datenstrom für eine Datei. Du kannst die Daten auch außerhalb von Arco mit einem age-Programm entschlüsseln und das resultierende TAR öffnen. Arco unterstützt derzeit einzelne Dateien über eigene Transferbefehle. Transparente Verschlüsselung ganzer Serverordner und das Dateiformat von WinSCP bleiben offen. [age-Dateiformat](https://c2sp.org/age) · [WinSCP-Dateiverschlüsselung](https://winscp.net/eng/docs/file_encryption)

## Sitzungs-URLs

Über **Netz → Sitzungs-URL öffnen …** oder `arco-commander.exe --session-url URL` öffnest du einen Serverordner oder eine einzelne Datei. Bei einem Ordner endet der Pfad mit `/`. Bei einer Datei wählst du zwischen **Datei im Dateifenster zeigen** und **Datei herunterladen**. Für den Download wählst du den lokalen Zielpfad. Arco schützt vorhandene Zieldateien.

Arco erkennt FTP, FTPS, FTPES, SFTP, SCP, WebDAV und S3 sowie WinSCP-Präfixe wie `winscp-sftp://`. Eine SSH-URL kann den SHA256-Fingerabdruck mit `?fingerprint=...` oder im WinSCP-Format mit `;fingerprint=...` enthalten. Arco zeigt Server und Fingerabdruck vor dem Verbinden an und prüft Abweichungen von gespeicherten SSH-Hostschlüsseln. Zugangsdaten gibst du im Anmeldedialog ein. [WinSCP-Sitzungs-URLs](https://winscp.net/eng/docs/session_url)

Im Netzwerk-Dateifenster kopieren **Sitzungs-URL kopieren** und **Datei-URL kopieren** direkte Links. **Arco-Profil-Link kopieren** und **Arco-Datei-Link kopieren** verwenden das gespeicherte Profil mit dessen Anmelde- und Proxyeinstellungen. **Sitzungsverknüpfung speichern …** und **Dateiverknüpfung speichern …** legen Windows-`.lnk`-Dateien an. Die Verknüpfungen verwenden die aktuelle Arco-Profildatei.

**Arco-Links in Windows …** registriert bei Bedarf `arco://` für das aktuelle Benutzerkonto und die aktuelle Profildatei. Danach lassen sich Arco-Profil- und Datei-Links aus Windows oder einem Browser öffnen. Im selben Dialog kannst du die Registrierung entfernen.

## Skriptbetrieb

`arco-net.exe` arbeitet mit gespeicherten Netzwerkprofilen. Standardmäßig liest es das Arco-Profil aus `%LOCALAPPDATA%\Arco Commander\profile.json`. Mit `--profile DATEI` wählst du eine andere Datei. Für eine Sitzung lassen sich Zugangsdaten über `--password-env NAME` aus einer Umgebungsvariablen lesen. Alternativ verwendet das Werkzeug die im Netzwerkprofil hinterlegte Anmeldung.

```text
open "Mein Server"
lcd "C:\Projekte\Veröffentlichung"
ls
put "Bericht Juli.txt"
get "Ergebnis.txt" "Ergebnis lokal.txt"
close
exit
```

Speichere das Skript als UTF-8 und rufe `arco-net.exe --script Ablauf.txt` auf oder lies die Eingabe mit `--script -` von stdin. Weitere Befehle sind `cd`, `pwd`, `lpwd`, `mkdir`, `mv`, `rm`, `rmdir` und `stat DATEI`. `chmod 0640 DATEI` setzt Rechte auf geeigneten Servern. `checksum DATEI` liest eine Prüfsumme von FTP- oder FTPS-Servern mit passender Erweiterung. Bei SFTP berechnet Arco SHA-256 aus dem Dateistrom. `put` und `get` verlangen freie Ziele. `rmdir` löscht leere Verzeichnisse. Ausgabe und Fehler verwenden JSON-Zeilen. Der Rückgabecode ist `0` für Erfolg, `1` für einen Skript- oder Verbindungsfehler und `2` für fehlerhafte Programmargumente. [.NET und COM](AUTOMATISIERUNG.md)

`open "DATEI-URL"` wechselt in den Serverordner und gibt den gefundenen Dateinamen als JSON-Feld `file` aus. `get-url "DATEI-URL" "LOKALER PFAD"` lädt die Datei direkt in ein freies Ziel. Der lokale Pfad ist optional. Ein bereits geöffnetes Netzwerkprofil bleibt danach aktiv.

## SCP

Wähle **SCP (SSH)** als Protokoll und verwende eine Adresse wie `scp://server:22/projekte/`. Die SSH-Anmeldung, Hostschlüsselprüfung und Sprungserver funktionieren wie bei SFTP. Ordnerwechsel, Kopieren, Ordner anlegen, Umbenennen und Löschen sind im Dateifenster verfügbar. SCP verwendet auf dem Server eine Unix-kompatible Shell, `scp`, GNU `find` und die passenden Dateibefehle wie `ln -T` und `mv -T -n`. Die Ordnerliste zeigt reguläre Dateien und Verzeichnisse. [WinSCP-Anforderungen für SCP](https://winscp.net/eng/docs/requirements#scp_requirements)

**SCP…** bleibt für einzelne Dateien aus einem gespeicherten SSH-Profil verfügbar. Relative Pfade beziehen sich auf die Startordner des Profils. Der Einzeldateitransfer verwendet ein freies Ziel.

## WebDAV

Wähle **WebDAV** und gib die Adresse des Stammordners ein, zum Beispiel `https://server/dav/projekte/`. Navigation und gespeicherte Tabs beziehen sich auf diesen Stammordner. Anmeldung und TLS-Prüfung verwenden das von Windows bereitgestellte curl. Für eine eigene Vertrauenskette kannst du im Profil eine CA-Datei angeben.

Kopieren, Ordner anlegen, Umbenennen und Löschen verwenden die eingebauten Dateifenster und Aufträge. Arco lädt Dateien zuerst unter einem temporären Namen hoch. Für das Ersetzen prüft es den Ziel-ETag. Beim Löschen einer leeren Sammlung verhindert eine WebDAV-Schreibsperre, dass zwischen Prüfung und Löschen neue Einträge hinzukommen. Dafür muss der Server die Sperre unterstützen. [WebDAV-Protokoll](https://datatracker.ietf.org/doc/html/rfc4918)

Unter **Netz → WebDAV-Sperren verwalten** kannst du für den fokussierten Eintrag Sperren anzeigen, eine exklusive Schreibsperre anlegen, deine Sperre verlängern oder sie aufheben. Die Sperre umfasst bei Ordnern auch deren Inhalt. Arco verwendet die in der Verbindung gehaltenen Sperrtoken bei Dateioperationen. Die Laufzeit bestimmt der Server. Eigene Sperren lassen sich vor dem Trennen der Verbindung freigeben. [WebDAV-Sperren nach RFC 4918](https://www.rfc-editor.org/rfc/rfc4918.html#section-6)

## S3

Wähle **S3** und hinterlege einen HTTPS-Endpunkt, eine Region und optional einen Startpfad. Das Profil verwendet Pfadadressierung, zum Beispiel `https://s3.eu-central-1.amazonaws.com/mein-bucket/projekte/`. Der erste Pfadteil ist der Bucket. Am Endpunktstamm erscheint die Liste der Buckets, soweit die Berechtigungen das zulassen.

Für die SigV4-Anmeldung gibst du Zugriffsschlüssel-ID und geheimen Schlüssel an. Bei leerer ID nutzt Arco `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` und ein optionales `AWS_SESSION_TOKEN` aus der Prozessumgebung. Die mit Windows installierte curl-Version muss `--aws-sigv4` unterstützen.

Mit **AWS-Anmeldekette verwenden** übernimmt das AWS SDK die Auflösung. Ein angegebener Profilname wählt das Profil aus den lokalen AWS-Dateien. Bei leerem Namen gelten die Reihenfolge des SDK und gegebenenfalls `AWS_PROFILE`. Die Kette umfasst unter anderem IAM-Rollen über STS, Web Identity, IAM Identity Center mit vorhandener Anmeldung, Container-Anmeldedaten, EC2 IMDS und `credential_process` aus der lokalen AWS-Konfiguration. Arco erneuert temporäre Anmeldedaten und speichert im Netzwerkprofil den Namen des AWS-Profils. [AWS-Anmeldekette für Rust](https://docs.aws.amazon.com/sdk-for-rust/latest/dg/credproviders.html)

Arco blättert durch alle Seiten eines Präfixes und verwendet Multipart-Uploads für größere Dateien. Ein abgebrochener Multipart-Auftrag wird am Endpunkt bereinigt. Vorhandene Objekte werden nur mit der bestätigten Zielversion ersetzt. Beim Umbenennen kopiert Arco den Inhalt und die Tags und entfernt anschließend die bestätigte Quelle. **Netz → S3-Objekt-Tags bearbeiten** öffnet die Schlüssel-Wert-Paare als JSON-Objekt. [S3-Multipart-Uploads](https://docs.aws.amazon.com/AmazonS3/latest/userguide/mpuoverview.html) · [S3-Objekt-Tags](https://docs.aws.amazon.com/AmazonS3/latest/userguide/object-tagging.html)

**Netz → S3-Versionshistorie** zeigt die Versionen und Löschmarkierungen eines Objekts. Du kannst den exakten Namen auch eingeben, wenn das Objekt inzwischen gelöscht wurde. Öffne im anderen Dateifenster einen lokalen Ordner, um eine frühere Version unter einem neuen Namen herunterzuladen. Dafür werden am Server `s3:ListBucketVersions` und `s3:GetObjectVersion` benötigt. [S3-Versionsliste](https://docs.aws.amazon.com/AmazonS3/latest/API/API_ListObjectVersions.html) · [Versionsdownload](https://docs.aws.amazon.com/AmazonS3/latest/API/API_GetObject.html)

## Geprüfter Umfang

Der eingebaute SSH-Anbieter verwendet [Russh](https://docs.rs/russh/0.65.2/russh/). Die [Funktionsmatrix](../FUNKTIONSMATRIX.md#eingebaute-netzwerkfunktionen-nach-winscp) führt die weiteren WinSCP-Funktionen und die [Fensterprüfliste](UI-PRUEFUNG.md#eingebautes-sftp-und-serversuche) die manuelle Abnahme. Bestehende WFX-Verbindungen bleiben verfügbar.

Die Windows-Loopback-Tests decken RSA-SHA2, Ed25519, ECDSA mit Passphrase, PPK 3 und PKCS#8, die lokale Windows-Agent-Pipe, zwei Sprungserver, HTTP-CONNECT, SOCKS5 mit Auflösung am Proxy und falsche Hostschlüssel ab. Geprüft sind außerdem binäre SFTP-Dateihandles, Unicode-Dateinamen, rekursive Suche, bidirektionaler Abgleich und Wiederanlauf nach einer getrennten Verbindung. Für Pageant steht der Lauf mit einem gestarteten Agenten aus.

Der erweiterte Lauf prüft mehrere Keyboard-Interactive-Runden, Schlüssel plus Einmalcode und den Abbruch einer Rückfrage. Er übt SCP ohne SFTP-Subsystem, Kollisionen an beiden Transferzielen, den SFTP-Upload mit falscher Teildatei und Wiederanlauf ab dem bestätigten Offset sowie UTC-Änderungszeiten und SHA-256.

Die HTTP-Referenz verwendet lokale WsgiDAV- und Moto-Gegenstellen. Sie prüft TLS, WebDAV-Sperren und Zielkonflikte sowie S3-Signaturen, Folgeseiten, Multipart-Abbruch, Objekt-Tags und das Entfernen von Präfixen ohne eigene Ordnermarkierung. Die Tests der Dateifensterlogik prüfen die rekursive Suche und den Abgleich in beiden Richtungen für SFTP, SCP, WebDAV und S3 sowie eine Kopie von WebDAV nach S3.

Mit `--net-exe target/debug/arco-net.exe` prüft die SFTP-Referenz per Skript auch die Sicherung beim Überschreiben. Sie simuliert einen Fehler bei der Veröffentlichung und prüft die Wiederherstellung am ursprünglichen Pfad. Der Verschlüsselungstest prüft Originalname und Inhalt, falsche Passphrasen, manipulierte Pakete und belegte lokale Ziele. Derselbe Schalter aktiviert bei der HTTP-Referenz verschlüsselte Rundläufe für WebDAV und S3.

Der Loopback-Test nutzt einen temporären Paramiko-Server, die Rust-Testprogramme und den Windows-OpenSSH-Client als SCP-Gegenstelle. Starte ihn in einer Windows Developer PowerShell mit Python, Paramiko und installiertem Windows-OpenSSH-Client.

```powershell
py -3 scripts/test-sftp-reference.py
py -3 scripts/test-native-http-reference.py
```

Die zweite Referenz verwendet `WsgiDAV`, `moto[server,s3]` und `Werkzeug`. Mit `--openssl <Pfad-zu-openssl.exe>` prüft sie HTTPS mit einem temporären Zertifikat. `--digest` verlangt Digest-Anmeldung. Mit `--aws-credentials profile`, `--aws-credentials container` und `--aws-credentials role` prüft sie nacheinander ein benanntes Profil mit Sitzungstoken, temporäre Container-Anmeldedaten und eine STS-Rollenübernahme gegen lokale Test-Endpunkte. Für die Prüfung der Dateifensterlogik übergibst du das mit `cargo test -p arco-app --bin arco-commander --no-run` gebaute Rust-Testprogramm an `--app-harness`.

Der testbare Kerberos-Fall `reference_kerberos_sftp` benötigt einen Windows-Benutzer mit gültigen Kerberos-Tickets und einen entsprechend eingerichteten SSH-Server. Er liest `ARCO_TEST_KERBEROS_URL`, `ARCO_TEST_KERBEROS_USER`, `ARCO_TEST_KERBEROS_FINGERPRINT` und optional `ARCO_TEST_KERBEROS_SPN`. `ARCO_TEST_KERBEROS_KEX=1` verlangt GSSAPI beim SSH-Schlüsselaustausch. Ohne Fingerabdruck erfordert dieser Modus `ARCO_TEST_KERBEROS_SPN`. `ARCO_TEST_KERBEROS_DELEGATE=1` fordert zusätzlich die Delegation an. Der Domänenlauf steht aus. Der SFTP-Loopback prüft auch die Ordnerüberwachung und mit `--net-exe <Pfad-zu-arco-net.exe>` den Skriptbetrieb.
