# FTP und FTPS

Unter **Netz → FTP-Verbindungen** speicherst du Server, Anmeldedaten und Startordner. Die Schnellverbindung öffnet einen Server mit den Standardoptionen. Für Einstellungen zu Zeichensatz, Zertifikaten, Proxy oder Anmeldung verwende ein Profil.

Mit **Windows-Proxy ermitteln** kannst du im Profil einen HTTP-Proxy für die Serveradresse vorschlagen lassen. Bei einem Proxyvorschlag stellt Arco den FTP-Übertragungsmodus auf passiv. [Windows-Proxy-Erkennung](SFTP.md)

| Profiloption | Verwendung |
| --- | --- |
| IP-Version | Automatisch, ausschließlich IPv4 oder ausschließlich IPv6 |
| FTPS | Explizites TLS über `ftp://` oder implizites TLS über `ftps://`. Die Zertifikatsprüfung ist aktiv. Für eine private CA kann das Profil eine PEM-Datei verwenden. Der optionale Fingerabdruck sucht ein Clientzertifikat im persönlichen Windows-Zertifikatspeicher. |
| Proxy | HTTP(S)-Tunnel, HTTP(S)-Forward, SOCKS4/4a/5/5h und passive Klartext-FTP-Gateways. Unter **Proxy-Anmeldung …** hinterlegst du die SOCKS4-Benutzerkennung oder die Zugangsdaten für HTTP(S) Basic, SOCKS5 und FTP-Gateways. |
| Serverzeichensatz | Standard ist UTF-8. Für ältere Server kannst du beispielsweise `windows-1252` oder `Shift_JIS` angeben. Arco benutzt die Einstellung für Dateinamen, LIST/MLSD und FTP-Dateibefehle. |
| Verzeichnisliste | Automatisch, MLSD oder LIST. LIST erkennt Unix, IIS und VMS. VMS liefert Blockzahlen. Die genaue Dateigröße bleibt dort leer. |
| Versteckte Dateien | In der automatischen Auswahl fordert Arco mit `LIST -a` alle Einträge an. Bei ausdrücklich gewähltem MLSD entscheidet der Server, welche Dateien er darüber listet. |
| Eigenes LIST-Format | Ein regulärer Ausdruck für eine ganze Zeile mit den Gruppen `name`, `kind` und optional `size`. `kind` akzeptiert `f` oder `-` für Dateien, `d` für Ordner und `l` für Verknüpfungen. |
| MODE Z | Komprimiert Binär- und Texttransfers, sofern der Server MODE Z anbietet. Für Wiederaufnahme verwendet Arco binäre Bytepositionen. |
| OTP / S/KEY | Einmalcodes nach RFC 2289 mit MD4, MD5 oder SHA-1 sowie S/KEY nach RFC 1760 mit MD4 und sechs Wörtern. Aktiviere **Einmalpasswort (OTP / S/KEY)** bei einer passiven Klartext-Direktverbindung und gib die Passphrase im Passwortfeld ein. Arco wählt das Format anhand der Serverabfrage und sendet den berechneten Einmalcode. |

Bei Verbindungsfehlern zeigt Arco die betroffene Operation an. **FTP-Verbindungsdetails** zeigt die Einstellungen und die Serverantwort auf `FEAT`. **FTP-Rohliste** zeigt die Ausgabe von LIST, etwa um ein eigenes Listenformat einzurichten. **FTP-Serverbefehl** sendet einzelne Befehle wie `FEAT`, `PWD`, `SIZE datei.txt` oder `SITE`-Unterbefehle an die aktive Verbindung.

### FTP über HTTP-Forward

Wähle im Profil **Klartext-FTP**, trage die HTTP- oder HTTPS-Adresse des Proxys ein und aktiviere **FTP über HTTP-Forward-Proxy (nur Lesen)**. Verwende den passiven Modus und die automatische Verzeichnisliste. Zugangsdaten für den Proxy und den FTP-Server pflegst du getrennt. Mit einem HTTPS-Proxy schützt TLS den Weg von Arco zum Proxy. Bei Bedarf kannst du dort eine PEM-Datei für die Proxy-CA hinterlegen.

Der Proxy muss HTTP-GET-Anfragen mit FTP-Adressen und HTML-Verzeichnislisten unterstützen. Arco liest Links in Listenblöcken, wie sie etwa Apache ausgibt. Es öffnet die Ordner im Dateifenster, unterstützt F3 und kann markierte Dateien oder Ordner mit F5 herunterladen. Fehlende Größenangaben erscheinen als unbekannt. **FTP-Rohliste** zeigt in diesem Modus den HTML-Quelltext. Upload, Wiederaufnahme, serverseitige Änderungen und Abgleich stehen für diese Verbindung nicht zur Verfügung. [Apache mod_proxy_ftp](https://httpd.apache.org/docs/current/en/mod/mod_proxy_ftp.html) · [curl HTTP-Proxy-Tunnel](https://curl.se/libcurl/c/CURLOPT_HTTPPROXYTUNNEL.html)

### FTP-Gateways

Trage eine Proxy-Adresse wie `ftp://gateway.example:2121` ein und wähle im Netzwerkprofil den Gateway-Ablauf. Der Standard sendet `USER Benutzer@Server`. `SITE` und `OPEN` unterstützen eine optionale Gateway-Anmeldung vor der Zielauswahl. Die weiteren Verfahren unterstützen die zweistufigen Anmeldungen und kombinierten Benutzernamen nach WinSCP. In den dafür vorgesehenen Varianten beantwortet Arco eine `ACCT`-Abfrage mit dem Gateway-Kennwort. Die Passiv-Verbindung für Daten verwendet dieselbe Gegenstelle wie der Steuerkanal. [WinSCP-Gateway-Verfahren](https://winscp.net/eng/docs/rawsettings) · [Total-Commander-Firewall-Hilfe im CHM-Paket](https://www.totalcommander.ch/help/tcmdd1158_chm.zip)

Der WinSCP-Profilimport übernimmt den Gateway-Typ, die Adresse und den Proxy-Benutzernamen. Das jeweilige Kennwort kannst du nach dem Import unter **Proxy-Anmeldung …** hinterlegen.

Für `socks4://` löst Arco den Zielhost lokal nach IPv4 auf. Mit `socks4a://` löst der Proxy Zielhostnamen auf. Das Profil speichert die SOCKS4-Benutzerkennung direkt. [SOCKS4-Protokoll](https://www.openssh.org/txt/socks4.protocol) · [curl-Proxyvarianten](https://github.com/curl/everything-curl/blob/master/usingcurl/proxies/socks.md)

## Dateien übertragen

In **Netz → FTP herunterladen** kannst du einen Serverpfad oder eine FTP-/FTPS-URL angeben. URLs müssen zu der aktiven Verbindung gehören. Zugangsdaten werden über die Anmeldemaske eingegeben. Binär erhält den Dateiinhalt bytegenau. Text verwendet FTP-Typ A für Zeilenenden. Synchronisierung, Vorschau und Rückübertragung aus dem Editor arbeiten weiterhin binär.

In der **FTP-Downloadliste** steht ein Pfad oder eine URL pro Zeile. `@C:\Pfad\downloadliste.txt` lädt eine Textdatei. Leerzeilen und mit `# ` beginnende Zeilen werden übersprungen. Die Ziele liegen im lokalen Fenster. Arco prüft alle Einträge vor dem Übertragen auf Namenskollisionen und vorhandene Dateien. Bis zu 1000 Dateien werden als ein Hintergrundauftrag abgearbeitet.

Bei **FTP herunterladen** kannst du einen fortsetzbaren Binärtransfer wählen. Arco speichert eine lokale Teildatei und prüft beim Wiederanlauf die Servermetadaten und bis zu 64 KiB am Ende der empfangenen Daten. Das endgültige Ziel erscheint erst nach der Abschlussprüfung. Der Server muss MLSD und `REST` unterstützen.

Bei **FTP hochladen** kannst du den Modus und die Behandlung vorhandener Ziele auswählen. Der fortsetzbare Binärtransfer vergleicht den gesamten Inhalt der bereits vorhandenen Teildatei mit dem Anfang der lokalen Quelle. Nach dem Anhängen prüft Arco die Größe und lädt das Ergebnis zur Inhaltsprüfung zurück. Der Server benötigt MLSD und muss `APPE` unterstützen. Ein Abbruch nach dem Beginn des Anhängens kann eine Teildatei hinterlassen.

Bei erkannter Verbindungsunterbrechung versucht Arco fortsetzbare FTP- und FTPS-Transfers bis zu zweimal erneut. Nach dem Verbindungsaufbau prüft Arco die Teildatei erneut. Ein Upload mit verlorener Abschlussantwort gilt erst nach erfolgreichem Vollvergleich als abgeschlossen. Der Transferdialog zeigt den Verbindungsaufbau und die Prüfung an. `arco-net` nutzt denselben Wiederanlauf für `get -resume` und `put -resume` mit vorhandener Teildatei.

Der FTP-Abgleich verwendet MLSD-Metadaten und angebotene Prüfsummen wie `HASH`, `XSHA1`, `XMD5`, `MD5` oder `XCRC`. Ohne verwendbare Antwort lädt Arco den Dateiinhalt zum Vergleich vorübergehend herunter.

### Server-zu-Server-Transfer (FXP)

Unter **Netz → Server-zu-Server (FXP)** merkst du den Quellordner und startest den direkten Transfer im Zielordner. Beide Klartext-FTP-Server müssen FXP erlauben und über eine gemeinsame IP-Version erreichbar sein. Sind beide Profile auf **Automatisch** eingestellt, versucht Arco IPv4 und bei fehlender Verbindung IPv6. IPv4 verwendet `PASV` und `PORT`, IPv6 verwendet `EPSV` und `EPRT`. Die Dateidaten laufen direkt zwischen den Servern. Für FTPS steht die Kopie über Arco mit **F5** zur Verfügung. [FTP über IPv6](https://www.rfc-editor.org/rfc/rfc2428) · [FTPS-Datenverbindungen](https://www.rfc-editor.org/rfc/rfc4217#section-7)

Arco prüft freie Zielnamen am tatsächlich verbundenen Zielserver und vergleicht die Größe vor dem Umbenennen. Wenn ein Transfer abbricht oder der Zielname währenddessen belegt wird, zeigt der Auftrag den Namen der möglichen Teildatei auf dem Zielserver an.

Sind beide Dateifenster mit geeigneten FTP-Servern verbunden, wählst du bei **F5 → Übertragungsweg → Direkt zwischen den Servern (FXP)** den direkten Weg. Die Kopiervorschau berücksichtigt markierte Dateien, Ordner, Masken und Ignorierregeln. Im Konfliktdialog stehen die Dateimetadaten, Überspringen, Überschreiben und ein freier Zielname zur Verfügung. Die Option **Über diesen PC** bietet zusätzlich Inhaltsvorschau und Anhängen. Die Server bestimmen den Durchsatz bei FXP. Setze das Limit der Kopie auf `0`, wenn eine Transfervorgabe ein Limit enthält.

## Verbleibende Referenzfälle

- Die Fensterbedienung steht als manuelle Abnahme aus. Die Loopback-Tests prüfen explizites und implizites FTPS mit privater CA, nicht vertrauenswürdiger CA, abweichendem Servernamen und Clientzertifikat aus dem persönlichen Windows-Speicher.
- USB-Direkttransfer und LPT-Direkttransfer sind weiterhin offen. Für USB brauchen wir ein unterstütztes Easy-Transfer-Kabel und eine Referenzgegenstelle zum Abgleich des Protokolls. Für LPT sind ein geeigneter Parallelport, ein Nibble-Kabel und eine auf Windows 10/11 funktionsfähige Treiber- oder Brückenlösung erforderlich. Beide Gerätetypen waren beim Abgleich nicht angeschlossen.

[FTP-Herstellerhilfe](https://www.ghisler.com/dfaqftp.htm) · [USB-Kabelreferenz](https://www.ghisler.com/cables/index.htm) · [LPT-Herstellerhilfe](https://www.ghisler.com/efaqport.htm) · [OTP nach RFC 2289](https://www.rfc-editor.org/rfc/rfc2289) · [S/KEY und Standardwörterbuch nach RFC 1760](https://www.rfc-editor.org/rfc/rfc1760)

## Referenztests ohne Fenster

In einer Windows Developer PowerShell mit Python und OpenSSL:

```powershell
py -3 scripts/test-ftp-area5-reference.py --openssl 'C:\Pfad\openssl.exe'
```

Die Tests verwenden temporäre Daten und Zertifikate auf der Loopback-Schnittstelle. Sie starten ausschließlich das Testprogramm und die benötigten Konsolenwerkzeuge. Mit `--client-cert` importiert der Test ein eigens erzeugtes Clientzertifikat in `CurrentUser\My`. Beide TLS-Modi müssen den Download mit Zertifikat erlauben und die Verbindung ohne Zertifikat ablehnen. Der Test entfernt das Zertifikat samt privatem Schlüssel am Ende.

Für den zusätzlichen FTPS-Editorfall baust du mit `cargo test -p arco-app --bin arco-commander --no-run` das Rust-Testprogramm und übergibst dessen von Cargo ausgegebenen Pfad mit `--app-harness`. Explizites und implizites FTPS verlangen dabei TLS auf Steuer- und Datenkanal. Zwei getrennte Sitzungen veröffentlichen konkurrierende Arbeitskopien mit gleicher Größe und gleichem Zeitstempel. Der Test prüft, ob beide lokalen Fassungen erhalten bleiben, der Server genau eine Veröffentlichung übernimmt und eine weitere Änderung regulär hochgeladen werden kann.

Die Bereich-5-Loopback-Suite umfasst elf Referenztests. Der FXP-Fall prüft für beide IP-Versionen den direkten Datenfluss, die Serverablehnung, zu kurze Übertragungen und Zielkonflikte während des Transfers. Der HTTP-Forward-Fall prüft FTP- und Proxy-Anmeldung, HTTP und HTTPS mit privater CA, HTML-Listen, Downloads mit und ohne bekannte Größe, Größenlimits und HTTP-Fehler. Mit `--arco-net target/debug/arco-net.exe` prüft der Runner zusätzlich Profil, Skriptliste sowie Einzeldatei- und Ordnerdownloads.

Die F5-Referenz verwendet `pyftpdlib` und den Testbuild von Arco. Führe `scripts/test-ftp-sync-reference.py` mit `ARCO_TEST_FXP_COPY=1` aus. Die beiden Testläufe verwenden IPv4 und IPv6 und prüfen auch, dass der Dateifluss während der Kopie direkt zwischen den Servern bleibt.
