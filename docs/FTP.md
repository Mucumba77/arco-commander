# FTP und FTPS

Unter **Netz → FTP-Verbindungen** speicherst du Server, Anmeldedaten und Startordner. Die Schnellverbindung öffnet einen Server mit den Standardoptionen. Für Einstellungen zu Zeichensatz, Zertifikaten, Proxy oder Anmeldung verwende ein Profil.

| Profiloption | Verwendung |
| --- | --- |
| IP-Version | Automatisch, ausschließlich IPv4 oder ausschließlich IPv6 |
| FTPS | Explizites TLS über `ftp://` oder implizites TLS über `ftps://`. Die Zertifikatsprüfung ist aktiv. Für eine private CA kann das Profil eine PEM-Datei verwenden. Der optionale Fingerabdruck sucht ein Clientzertifikat im persönlichen Windows-Zertifikatspeicher. |
| Proxy | HTTP(S)-Tunnel und SOCKS4/4a/5/5h. HTTP(S) Basic und SOCKS5 verwenden die getrennte **Proxy-Anmeldung …** im Netzwerkprofil. Eine `ftp://`-Proxyadresse wählt das Klartext-Gateway mit `USER Benutzer@Server[:Port]`. |
| Serverzeichensatz | Standard ist UTF-8. Für ältere Server kannst du beispielsweise `windows-1252` oder `Shift_JIS` angeben. Arco benutzt die Einstellung für Dateinamen, LIST/MLSD und FTP-Dateibefehle. |
| Verzeichnisliste | Automatisch, MLSD oder LIST. LIST erkennt Unix, IIS und VMS. VMS liefert Blockzahlen. Die genaue Dateigröße bleibt dort leer. |
| Versteckte Dateien | In der automatischen Auswahl fordert Arco mit `LIST -a` alle Einträge an. Bei ausdrücklich gewähltem MLSD entscheidet der Server, welche Dateien er darüber listet. |
| Eigenes LIST-Format | Ein regulärer Ausdruck für eine ganze Zeile mit den Gruppen `name`, `kind` und optional `size`. `kind` akzeptiert `f` oder `-` für Dateien, `d` für Ordner und `l` für Verknüpfungen. |
| MODE Z | Komprimiert Binär- und Texttransfers, sofern der Server MODE Z anbietet. Für Wiederaufnahme verwendet Arco binäre Bytepositionen. |
| OTP | RFC 2289 mit MD4, MD5 oder SHA-1 auf einer passiven Klartext-Direktverbindung. In das Passwortfeld kommt die Passphrase. Der Server erhält den berechneten Einmalcode. |

Bei Verbindungsfehlern zeigt Arco die betroffene Operation an. **FTP-Verbindungsdetails** zeigt die Einstellungen und die Serverantwort auf `FEAT`. **FTP-Rohliste** zeigt die Ausgabe von LIST, etwa um ein eigenes Listenformat einzurichten. **FTP-Serverbefehl** sendet einzelne Befehle wie `FEAT`, `PWD`, `SIZE datei.txt` oder `SITE`-Unterbefehle an die aktive Verbindung.

## Dateien übertragen

In **Netz → FTP herunterladen** kannst du einen Serverpfad oder eine FTP-/FTPS-URL angeben. URLs müssen zu der aktiven Verbindung gehören. Zugangsdaten werden über die Anmeldemaske eingegeben. Binär erhält den Dateiinhalt bytegenau. Text verwendet FTP-Typ A für Zeilenenden. Synchronisierung, Vorschau und Rückübertragung aus dem Editor arbeiten weiterhin binär.

In der **FTP-Downloadliste** steht ein Pfad oder eine URL pro Zeile. `@C:\Pfad\downloadliste.txt` lädt eine Textdatei. Leerzeilen und mit `# ` beginnende Zeilen werden übersprungen. Die Ziele liegen im lokalen Fenster. Arco prüft alle Einträge vor dem Übertragen auf Namenskollisionen und vorhandene Dateien. Bis zu 1000 Dateien werden als ein Hintergrundauftrag abgearbeitet.

Bei **FTP hochladen** kannst du den Modus und die Behandlung vorhandener Ziele auswählen. Der fortsetzbare Binärtransfer vergleicht zuerst den gesamten Inhalt der bereits vorhandenen Teildatei mit dem Anfang der lokalen Quelle. Nach dem Anhängen prüft Arco die Größe und lädt das Ergebnis zur Inhaltsprüfung zurück. Der Server benötigt MLSD und muss das Anhängen unterstützen. Ein Abbruch nach dem Beginn des Anhängens kann eine Teildatei hinterlassen.

Der FTP-Abgleich verwendet MLSD-Metadaten und angebotene Prüfsummen wie `HASH`, `XSHA1`, `XMD5`, `MD5` oder `XCRC`. Ohne verwendbare Antwort lädt Arco den Dateiinhalt zum Vergleich vorübergehend herunter. Direktes FXP verwendet zwei geeignete IPv4-Klartextserver.

## Verbleibende Referenzfälle

- Die üblichen HTTP/SOCKS-Proxys und der `USER Benutzer@Server`-Gateway-Ablauf sind verfügbar. Zusätzliche TC-Firewall-Anmeldefolgen sowie ältere S/KEY-Server, die ausschließlich die Sechswort-Antwort unterstützen, bleiben offen.
- Eine Windows-Clientzertifikatsanmeldung mit installiertem Zertifikat und die Fensterbedienung stehen als manuelle Abnahme aus. Die Loopback-Tests prüfen explizites und implizites FTPS mit privater CA, nicht vertrauenswürdiger CA und abweichendem Servernamen.
- USB-Direkttransfer und LPT-Direkttransfer sind weiterhin offen. Für USB brauchen wir ein unterstütztes Easy-Transfer-Kabel und eine Referenzgegenstelle zum Abgleich des Protokolls. Für LPT sind ein geeigneter Parallelport, ein Nibble-Kabel und eine auf Windows 10/11 funktionsfähige Treiber- oder Brückenlösung erforderlich. Beide Gerätetypen waren beim Abgleich nicht angeschlossen.

[FTP-Herstellerhilfe](https://www.ghisler.com/dfaqftp.htm) · [USB-Kabelreferenz](https://www.ghisler.com/cables/index.htm) · [LPT-Herstellerhilfe](https://www.ghisler.com/efaqport.htm) · [OTP nach RFC 2289](https://www.rfc-editor.org/rfc/rfc2289)

## Referenztests ohne Fenster

In einer Windows Developer PowerShell mit Python und OpenSSL:

```powershell
py -3 scripts/test-ftp-area5-reference.py --openssl 'C:\Pfad\openssl.exe'
```

Die Tests verwenden temporäre Daten und Zertifikate auf der Loopback-Schnittstelle. Sie starten ausschließlich das Testprogramm und die benötigten Konsolenwerkzeuge.

In diesem Arbeitsstand sind 13 FTP-Referenzfälle bestanden. Dazu zählen die hier beschriebenen Profile und Transfermodi, FXP, Wiederaufnahme und der bidirektionale Abgleich einschließlich Spiegelung und Prüfsummenvergleich. Der Workspace-Testlauf ohne Fenster umfasst 353 bestandene Tests. Der Windows-Build ist geprüft.
