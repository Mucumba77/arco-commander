# Arco Commander automatisieren

`arco-net.exe` nutzt dieselben Netzwerkprofile und dieselbe SSH-Hostschlüsselprüfung wie das Dateifenster. Mit `--profile` gibst du den Pfad zu einer anderen Arco-Profildatei an. `--script -` liest UTF-8 vom Standardeingang. Verwende für ein temporäres Kennwort `--password-env NAME` mit genau einem `open` oder `get-url` im Skript.

## Lokale Befehle und Protokollmeldungen

`lls` listet das aktuelle lokale Verzeichnis. Gib optional einen Ordner oder eine Maske im letzten Pfadteil an. `echo` schreibt eine Meldung als JSON-Ereignis. Beide Befehle funktionieren vor dem Verbindungsaufbau. [WinSCP lls](https://winscp.net/eng/docs/scriptcommand_lls) · [WinSCP echo](https://winscp.net/eng/docs/scriptcommand_echo)

```text
echo "Export vorbereiten"
lcd "C:\Arco Jobs"
lls "*.csv"
```

Nach `open` listet `ls` den aktuellen Serverordner. `dir` ist ein Alias. Gib optional einen Ordner oder eine Dateimaske im letzten Pfadteil an, etwa `ls "/export/2026/*.csv"`. `option failonnomatch on` behandelt eine Maske ohne Treffer als Skriptfehler. [WinSCP ls](https://winscp.net/eng/docs/scriptcommand_ls)

## Verschlüsselte Dateitransfers

`put-encrypted LOKALE_DATEI VARIABLENNAME` lädt ein Paket mit verschlüsseltem Inhalt und Namen hoch. Das JSON-Ergebnis enthält den generierten Servernamen unter `uploaded`. `get-encrypted SERVERPAKET VARIABLENNAME [LOKALER_ZIELORDNER]` stellt die geprüfte Originaldatei im vorhandenen Zielordner wieder her. Ohne Zielangabe verwendet Arco den aktuellen lokalen Ordner.

```text
open "Mein Server"
put-encrypted "Vertraulicher Bericht.txt" ARCO_DATEIPASSPHRASE
get-encrypted "arco-Beispiel.arco.age" ARCO_DATEIPASSPHRASE "C:\Arco Jobs\Wiederhergestellt"
```

Setze die Passphrase in der Umgebung des Skriptprozesses. Im Skript stehen nur die Namen der Variablen. Die Dateipassphrase ist von der Server-Anmeldung über `--password-env` unabhängig. [Dateipakete und Wiederherstellung](SFTP.md#dateien-clientseitig-verschlüsseln)

## Ordner per Skript abgleichen

`synchronize local|remote|both [lokaler Ordner] [Serverordner]` gleicht rekursiv ab. `local` aktualisiert den lokalen Ordner, `remote` den Serverordner. Ohne Pfadangabe verwendet der Befehl die durch `lcd` und `cd` gewählten Verzeichnisse. [WinSCP-Befehl synchronize](https://winscp.net/eng/docs/scriptcommand_synchronize)

```text
open "Mein Server"
synchronize remote -preview -delete "C:\Export" "/projekte"
synchronize remote -delete "C:\Export" "/projekte"
close
```

- `-preview` liefert die geplanten Änderungen als JSON und schreibt keine Dateien.
- `-delete` entfernt Einträge, die nur im Ziel liegen. Der Befehl prüft den Vergleichsstand vor dem Eingriff. Bei einem Kopierfehler überspringt er die Löschphase. Bei `both` wirkt die Option nicht.
- `-mirror` übernimmt auch ältere Quelldateien. Bei `both` entscheidet weiterhin die Änderungszeit über die Richtung.
- `-criteria=time,size,checksum` kombiniert die gewünschten Kriterien. Standard ist `time`. `none` kopiert in eine Richtung auch bei gleichen Metadaten. `both` benötigt `time`.
- `-filemask=*.txt;*.csv|*.tmp` ergänzt die Dateimaske des Profils. Arco berücksichtigt außerdem die aktivierte Ignorierliste. `--` leitet Pfadangaben ein, die mit einem Bindestrich beginnen.
- `-preset=NAME` wählt für diesen Abgleich eine benannte Transfervorgabe.
- `-preservetime` oder `-nopreservetime` legt die Übernahme der Zeitstempel bei Dateitransfers fest. Ohne Schalter gilt die Vorgabe oder das Netzwerkprofil. Bei fehlender Einstellung behandelt der Abgleich die Änderungszeit wie bisher.
- `-timestamps` aktualisiert ausschließlich die Änderungszeit gleich großer Dateipaare. Mit `-all-sizes` erlaubst du auch Paare mit unterschiedlicher Dateigröße. In der Vorschau heißen die Aktionen `timestamp-local`, `timestamp-remote` und bei übersprungenen Paaren `timestamp-skipped`. `-delete` und `-mirror` sind in diesem Modus gesperrt.

Dieselben Skripte laufen über die .NET-/COM-Schnittstelle und in geplanten Windows-Aufgaben.

## Vorgaben pro Einzeltransfer

Bei `put`, `get` und `get-url` kannst du mit `-preset=NAME` eine benannte Vorgabe für den einzelnen Befehl wählen. `-speed=32` setzt das Limit auf 32 KiB/s, `-speed=0` hebt es auf. Mit `-transfer=binary|ascii|automatic` änderst du den Übertragungsmodus. ASCII und die automatische Textauswahl setzen FTP oder FTPS voraus. `-preservetime` und `-nopreservetime` steuern die Übernahme der Änderungszeit. Eine zusätzliche `-filemask=MASKE` begrenzt die Dateiauswahl gemeinsam mit der Maske der Vorgabe. [WinSCP get](https://winscp.net/eng/docs/scriptcommand_get) · [WinSCP put](https://winscp.net/eng/docs/scriptcommand_put)

```text
open "Mein Server"
get bericht.csv "-preset=Monatlicher Export" -speed=128 -preservetime
synchronize local -timestamps -preview
close
```

## Mehrere Dateien übertragen

`get` und `put` nehmen Dateilisten und die Platzhalter `*` und `?` im Dateinamen an. Gib als letztes Argument einen vorhandenen Zielordner an, wenn du mehrere Quellen aufführst. Bei einer einzelnen Maske ohne Ziel gilt der aktuelle Ordner auf der Zielseite. Pfade mit Leerzeichen werden in Anführungszeichen übergeben. Die Aliase `recv` und `mget` entsprechen `get`, `send` und `mput` entsprechen `put`. [WinSCP get](https://winscp.net/eng/docs/scriptcommand_get) · [WinSCP put](https://winscp.net/eng/docs/scriptcommand_put)

```text
open "Mein Server"
lcd "C:\Export"
put "*.csv" bericht.txt "/Eingang/"
get "/Ausgang/*.pdf" "/Ausgang/index.txt" "C:\Ergebnisse\"
get -latest "/Ausgang/*.csv" "C:\Neueste\"
close
```

Ein Mehrfachtransfer prüft doppelte Zieldateinamen und bereits belegte Ziele vor dem ersten Transfer. Überschneidende Masken wählen dieselbe Quelldatei einmal aus. Erfolgreiche Mehrfachtransfers liefern die Liste der abgeschlossenen Einträge und `completed` als Anzahl.

- `-latest` beschränkt die Auswahl nach den Dateimasken auf die Datei mit der jüngsten Änderungszeit.
- `-resume` nutzt die geprüfte binäre Wiederaufnahme per FTP, FTPS oder SFTP. FTP kann eine passende, kleinere Teildatei am Ziel ergänzen. SFTP verwendet eigene Teildateien und veröffentlicht das fertige Ziel nach der Prüfung. Dieselbe Option ist für Downloads und einzelne Datei-URLs verfügbar. [WinSCP Wiederaufnahme](https://winscp.net/eng/docs/resume)
- Leere Maskentreffer ergeben standardmäßig `"completed": 0`. Mit `option failonnomatch on` gilt ein leerer Treffer für die nachfolgenden Befehle als Fehler. `option failonnomatch off` setzt das zurück. Für einen einzelnen Befehl gibt es `-failonnomatch`.

### Ordner rekursiv übertragen

Ein ausdrücklich angegebener Ordner wird mitsamt Unterordnern und leeren Verzeichnissen in den gewählten Zielordner kopiert. Mit `-recursive` können auch Platzhalter Ordner auswählen. Arco verwendet den Binärmodus und berücksichtigt Profilmasken, `-filemask`, die Ignorierliste und das Durchsatzlimit. Bereits vorhandene Zielordner lassen sich ergänzen. Eine vorhandene Zieldatei beendet die Vorbereitung vor dem ersten Transfer. `-latest` und `-resume` gelten für einzelne Dateien und flache Dateilisten.

```text
put "C:\Export\Projekt" "/Eingang/" -filemask=*.pdf
get "/Eingang/Projekt" "C:\Ergebnisse\"
get -recursive "/Ausgang/Projekt-*" "C:\Ergebnisse\"
```

## Ordnerüberwachung per Skript

`keepuptodate [LOKAL] [SERVER]` verwendet ohne Pfadangaben die mit `lcd` und `cd` gewählten Ordner. Es veröffentlicht stabile lokale Änderungen und meldet Start, Änderungen und Ende als JSON-Ereignisse. Strg+C oder Strg+Pause beendet die Überwachung und das Skript. `-duration=SEKUNDEN` beendet nur die Überwachung. Das Skript fährt anschließend mit der nächsten Zeile fort. [WinSCP keepuptodate](https://winscp.net/eng/docs/scriptcommand_keepuptodate)

- `-interval=SEKUNDEN` setzt das Prüfintervall. Möglich sind 2 bis 300 Sekunden. Die Voreinstellung beträgt 5 Sekunden.
- `-initial-sync` gleicht vorhandene lokale Dateien vor dem Überwachen ab. Zusätzliche Serverdateien bleiben erhalten.
- `-delete` aktiviert den Löschabgleich für verifizierte Dateipaare. Bei aktiviertem Server-Papierkorb werden die betroffenen Dateien dorthin verschoben.
- `-filemask=MASKE` und `-preset=NAME` grenzen die Dateiauswahl ein und wählen Transfervorgaben.

```text
open "Mein Server"
keepuptodate "C:\Export" "/Eingang" -interval=2 -duration=60 "-filemask=*.csv"
close
```

## Server-Papierkorb im Skript

`trash` listet die Kennungen im Server-Papierkorb der aktiven Verbindung. `trash put PFAD` verschiebt einen Eintrag bewusst dorthin. `rm` und `rmdir` verwenden bei eingeschaltetem Papierkorb das gleiche Verfahren. `rm -permanent DATEI` oder `rmdir -permanent ORDNER` löscht ausdrücklich dauerhaft. `trash restore KENNUNG [ZIELORDNER]` stellt den Namen im ausgewählten Zielordner wieder her. Ohne Zielordner gilt der Startordner des Netzwerkprofils. [Server-Papierkorb in Arco](SFTP.md#server-papierkorb)

## Mehrere Server im selben Skript

Jeder `open`-Befehl öffnet eine eigene Sitzung und gibt ihre Nummer aus. `session` listet die offenen Sitzungen. `session 1` aktiviert die Sitzung mit der Nummer 1 und stellt ihren zuletzt verwendeten Serverordner bereit. `close` schließt die aktive Sitzung. Mit `close 2` schließt du gezielt Sitzung 2. Für mehrere Anmeldungen im selben Skript verwendest du die Zugangsdaten der jeweiligen Profile. [WinSCP session](https://winscp.net/eng/docs/scriptcommand_session) · [WinSCP close](https://winscp.net/eng/docs/scriptcommand_close)

## Windows-Aufgabenplanung

Wähle **Netz → Automatisierung → Netzwerkskript planen …**, dann ein UTF-8-Skript, einen Auslöser, einen Namen und den Windows-Anmeldemodus. Arco bietet **täglich**, **einmalig**, **wöchentlich**, **monatlich**, **bei Windows-Anmeldung** und **beim Systemstart** an. Uhrzeiten entsprechen der lokalen Zeit. Ein Monatstag zwischen 29 und 31 greift in Monaten, die diesen Tag enthalten. Der Anmeldeauslöser gilt für das aktuelle Windows-Konto. Die Registrierung eines Systemstartauslösers erfordert Windows-Administratorrechte und die Ausführung ohne Anmeldung. [Windows-Aufgabenauslöser](https://learn.microsoft.com/windows/win32/taskschd/taskschedulerschema-triggers-tasktype-element) · [Systemstart](https://learn.microsoft.com/windows/win32/taskschd/boottrigger)

Für **Auch ohne Windows-Anmeldung** gibst du das Passwort deines aktuellen Windows-Kontos ein. Windows verwaltet die Anmeldung der Aufgabe. Der Modus setzt die Berechtigung zur Batchanmeldung voraus. [Windows-Sicherheitskontext von Aufgaben](https://learn.microsoft.com/windows/win32/taskschd/security-contexts-for-running-tasks)

Das Skript läuft in seinem eigenen Ordner und nutzt das ausgewählte Arco-Profil samt den für dieses Windows-Konto gespeicherten Anmeldedaten. Arco legt pro Lauf eine JSONL-Datei unter `Netzwerkprotokolle` neben der Profildatei an. Start, Skriptereignisse, Abschluss und Rückgabecode stehen darin.

**Geplante Netzwerkskripte …** zeigt Status und letzten Rückgabecode an. Dort kannst du eine Aufgabe von Hand ausführen oder entfernen. Die PowerShell-Entsprechung verwendet dieselbe Windows-Aufgabenplanung.

```powershell
.\arco-net.exe schedule create "Abendexport" 18:00 "C:\Arco Jobs\export.txt"
.\arco-net.exe schedule create "Einzeltermin" "2030-05-16 18:00" "C:\Arco Jobs\export.txt" --trigger once
.\arco-net.exe schedule create "Wochenexport" "18:00 Mo,Fr" "C:\Arco Jobs\export.txt" --trigger weekly
.\arco-net.exe schedule create "Monatsexport" "18:00 1,15" "C:\Arco Jobs\export.txt" --trigger monthly
.\arco-net.exe schedule create "Anmeldeexport" logon "C:\Arco Jobs\export.txt"
.\arco-net.exe schedule list
.\arco-net.exe schedule run "Abendexport"
.\arco-net.exe schedule remove "Abendexport"
```

Bei `schedule create` wählen `--profile DATEI` und `--log-dir ORDNER` andere Speicherorte. `--log-dir` ist auch für einen einzelnen Skriptaufruf verfügbar. Verwende für geplante Transfers gespeicherte Profil-Anmeldungen, Agentenschlüssel oder andere für das Windows-Konto verfügbare Zugangsdaten.

Für den Hintergrundmodus verwendet `schedule create` die Option `--background-password-stdin`. Ab PowerShell 7.1 liest dieser Aufruf das Windows-Kontopasswort maskiert ein und leitet es über stdin an Arco weiter. [PowerShell Read-Host](https://learn.microsoft.com/powershell/module/microsoft.powershell.utility/read-host)

```powershell
Read-Host "Windows-Kontopasswort" -MaskInput |
    .\arco-net.exe schedule create "Abendexport" 18:00 "C:\Arco Jobs\export.txt" --background-password-stdin
Read-Host "Windows-Kontopasswort" -MaskInput |
    .\arco-net.exe schedule create "Startexport" startup "C:\Arco Jobs\export.txt" --background-password-stdin
```

## .NET

Verwende .NET 10 (x64). Füge `automation/ArcoCommander.Automation.dll` als Assembly-Referenz hinzu oder referenziere `automation/ArcoCommander.Automation.csproj` im Quellcode.

```csharp
using ArcoCommander.Automation;

var network = new NetworkAutomation(@"C:\Tools\Arco\arco-net.exe");
var result = await network.RunAsync("open \"Mein Server\"\nls\nclose");
if (result.ExitCode != 0)
    throw new IOException(result.Failure?.GetProperty("error").GetString());
foreach (var item in result.Events)
    Console.WriteLine(item);
```

`ListAsync`, `UploadAsync` und `DownloadAsync` bauen die jeweiligen Befehle mit separat maskierten Argumenten. `RunAsync` akzeptiert einen `CancellationToken`. Ein Abbruch stoppt den Konsolenprozess. Bereits abgeschlossene Aktionen bleiben bestehen.

`ListMatchingAsync` listet einen Serverordner oder wendet eine Dateimaske an. `UploadEncryptedAsync` und `DownloadEncryptedAsync` verwenden die verschlüsselten Transferbefehle. Übergebe den Namen der Passphrasenvariable im Parameter `passphraseVariable`.

## COM

Die x64-COM-Klasse verwendet die CLSID `94BC3A55-4C2E-4285-9FCE-D7E68E6530CE`. Installiere die .NET-10-Laufzeit in x64 und halte DLLs, Runtime-Konfiguration und Manifest im gemeinsamen Verzeichnis `automation`.

Ein COM-Client kann die Klasse über das mitgelieferte Manifest `ArcoCommander.Automation.X.manifest` in einem Aktivierungskontext laden. Für klassische x64-COM-Clients wie Windows Script Host lässt sich alternativ `ArcoCommander.Automation.comhost.dll` manuell mit `%SystemRoot%\System32\regsvr32.exe` registrieren oder mit `/u` deregistrieren. Das Arco-ZIP richtet keine COM-Registrierung ein. [Windows-COM-Hosting für .NET](https://learn.microsoft.com/dotnet/core/native-interop/expose-components-to-com)

`Run(executable, profilePath, script, passwordVariable, timeoutMilliseconds)` liefert ein JSON-Objekt mit `exitCode`, `events` und `failure`. Ein leerer `profilePath` verwendet das normale Arco-Profil. Ein leerer `passwordVariable` verwendet die gespeicherte Anmeldemethode. Das Timeout muss größer als null sein.

## Referenzprüfung

```powershell
cargo build --locked -p arco-app --bin arco-net
dotnet run --project scripts/automation-reference/Automation.Reference.csproj -- `
    target/debug/arco-net.exe target/dotnet/net10.0-windows/ArcoCommander.Automation.X.manifest
```

Die Referenz prüft die strukturierte Ausgabe, einen Syntaxfehler vor Ausführung und den Aufruf der COM-Klasse über `IDispatch` mit Aktivierungsmanifest. Der Test startet nur den Konsolenprozess.
