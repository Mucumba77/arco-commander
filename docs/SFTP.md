# SFTP in Arco Commander

Der eingebaute SFTP-Anbieter öffnet SSH-Verbindungen in beiden Dateifenstern. Unter **Netz → FTP- und SFTP-Verbindungen …** legst du ein Profil an und wählst **SFTP (SSH)**. Gib Serveradresse, Benutzername und den mit dem Serverbetreiber abgeglichenen SHA256-Fingerabdruck des Hostschlüssels ein. In der Adresse kannst du einen Startordner hinterlegen, etwa `sftp://server:22/projekte/`.

Bei Passwortanmeldung bleibt das Feld für die private Schlüsseldatei leer. Für RSA-Schlüsselanmeldung gib den absoluten Pfad zur privaten Schlüsseldatei an. Das Passwortfeld nimmt dann die Passphrase des Schlüssels auf. Zugangsdaten können im Windows-Anmeldeinformationsspeicher abgelegt werden. Das Profil speichert den Fingerabdruck und den Dateipfad. Bei einer Änderung des Server-Fingerabdrucks beendet Arco den Verbindungsaufbau vor dem Senden von Zugangsdaten.

Die **Netzwerk-Schnellverbindung** akzeptiert ebenfalls eine `sftp://`-Adresse. Sie fragt den Fingerabdruck und anschließend das Passwort ab. Für eine Schlüsseldatei verwende ein Profil.

## Dateioperationen

- Dateien und Ordner im Dateifenster öffnen. Kopien, Änderungen und Löschungen verwenden die vorhandenen Dialoge und den Transfermanager.
- Binäre Uploads werden zuerst unter einem temporären Namen übertragen. Arco prüft den erwarteten Zielstand und die Größe vor dem Umbenennen. Bei einem Fehlschlag meldet Arco, wenn temporäre Dateien oder eine Sicherung auf dem Server geprüft werden müssen.
- **Alt+F7** sucht in FTP-, FTPS- und SFTP-Fenstern rekursiv nach Dateimasken. Treffer lassen sich im Dateifenster fokussieren. Die Ignorierliste gilt auch für die Suche.
- **Netz → Serverordner mit lokalem Ordner abgleichen** vergleicht einen lokalen Ordner mit einem SFTP-Fenster. Änderungen werden vor der Ausführung angezeigt. Für den Inhaltsvergleich liest Arco die betroffenen Dateien vom Server.

## Geprüfter Umfang

Der Windows-Anbieter verwendet libssh2 mit Windows CNG. Der Referenzlauf verwendet RSA-SHA2, Passwort und RSA-Schlüsseldatei. Der Prüfstand für weitere Schlüsselalgorithmen, SSH-Agenten, Sprungserver, Wiederanlauf und die übrigen WinSCP-Funktionen steht in der [Funktionsmatrix](../FUNKTIONSMATRIX.md#eingebaute-netzwerkfunktionen-nach-winscp). Bestehende WFX-Verbindungen bleiben verfügbar.

Beide SFTP-Referenztests sind bestanden. Sie prüfen die Hostschlüsselablehnung vor der Anmeldung, Dateinamen mit Unicode und Leerzeichen, Dateioperationen, Suche in Unterordnern und den bidirektionalen Abgleich. Der Windows-Build und 353 weitere Tests ohne Fenster sind bestanden. Die [Fensterprüfliste](UI-PRUEFUNG.md#eingebautes-sftp-und-serversuche) führt die manuelle Abnahme.

Der Loopback-Test verwendet einen temporären SSH-Server und die Rust-Testprogramme. In einer Windows Developer PowerShell mit Python und Paramiko:

```powershell
py -3 scripts/test-sftp-reference.py
```
