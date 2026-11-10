# Windows-Veröffentlichung

Ein Release besteht aus einem portablen Windows-x64-ZIP, einer SHA-256-Prüfsumme, dem zugehörigen Git-Tag und den GitHub-Release-Hinweisen. Die [Änderungshistorie](../CHANGELOG.md) beschreibt die Version. Die [Funktionsmatrix](../FUNKTIONSMATRIX.md) führt die offenen Paritätsfälle.

## Build und Paket

Setze zuerst die Workspace-Version in `Cargo.toml`, aktualisiere `Cargo.lock` und ergänze den Changelog. Der Release-Build verwendet den eingecheckten Lockfile. Benötigt werden Rust, das Windows SDK, die MSVC-Tools für x64 und x86, Python 3.10 oder neuer sowie Zugriff auf NuGet für die Lizenztexte des WebView2 SDK.

Führe diese Befehle im Repository in einer **x64 Developer PowerShell** aus. Die Pfadabbildungen halten lokale Benutzer- und Buildpfade aus den Release-Binärdateien heraus.

```powershell
rustup target add x86_64-pc-windows-msvc i686-pc-windows-msvc
$arcoRepo = (Get-Location).Path
$env:CARGO_ENCODED_RUSTFLAGS = @(
    "--remap-path-prefix=$arcoRepo=/src/arco-commander",
    "--remap-path-prefix=$env:USERPROFILE=/build/home"
) -join [char]31
$env:SLINT_COMPILER_DENY_WARNINGS = '1'
cargo build --workspace --locked --release --target x86_64-pc-windows-msvc
```

Setze in einer **x86 Developer PowerShell** im selben Repository dieselben Pfadabbildungen und baue den Host für 32-Bit-Plugins.

```powershell
$arcoRepo = (Get-Location).Path
$env:CARGO_ENCODED_RUSTFLAGS = @(
    "--remap-path-prefix=$arcoRepo=/src/arco-commander",
    "--remap-path-prefix=$env:USERPROFILE=/build/home"
) -join [char]31
cargo build --locked --release --target i686-pc-windows-msvc -p arco-plugin-host
```

Committe den Release-Stand vor dem Paketieren. Das Skript verwendet `HEAD` für den Quellverweis in der Kurzanleitung und schreibt nach `target/distribution/v<VERSION>/`.

```powershell
py -3 scripts/package-windows.py
```

Das Skript prüft Architektur, GUI-Subsystem, lokale Buildpfade und die Programmversion. Es entpackt das ZIP für Tests des Programmaufrufs, der beiden Plugin-Hosts und des ZIP-SFX-Moduls. Die Fensterbedienung und Zielsysteme haben separate manuelle Prüffälle.

## GitHub

Lege den annotierten Tag `v<VERSION>` auf den geprüften Commit. Lade das ZIP und `SHA256SUMS.txt` in einen Release-Entwurf zum Tag und vergleiche die Hashes. Veröffentliche den geprüften Entwurf als reguläre aktuelle Version. Die Release-Hinweise nennen Betriebssysteme, optionale Voraussetzungen, Signaturstatus und Stand der manuellen Prüfung. Füge für Slint den offiziellen **Made with Slint**-Badge auf der Downloadseite ein.
