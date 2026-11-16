param(
    [string]$SevenZip = "target/debug/7zip/7z.exe",
    [string]$ArchivePython = "target/debug/archive-python/Scripts/python.exe",
    [string]$Lha = "target/area4/tools/LHA32.EXE",
    [string]$WcxPlugin = "",
    [switch]$PrepareCorpus,
    [switch]$Offline
)

# Run in an x64 Visual Studio developer shell. This never runs Arco Commander,
# an SFX result, DOSBox or any program stored inside an archive.
$ErrorActionPreference = "Stop"
$workspaceRoot = Split-Path -Parent $PSScriptRoot
$saved = @{}
foreach ($name in @("ARCO_7ZIP_PATH", "ARCO_ARCHIVE_PYTHON", "ARCO_LHA_PATH", "ARCO_ARCHIVE_CORPUS", "ARCO_TEST_WCX_ZONE")) {
    $saved[$name] = [Environment]::GetEnvironmentVariable($name, "Process")
}
Push-Location -LiteralPath $workspaceRoot
try {
    $env:ARCO_7ZIP_PATH = (Resolve-Path -LiteralPath $SevenZip).Path
    $env:ARCO_ARCHIVE_PYTHON = (Resolve-Path -LiteralPath $ArchivePython).Path
    $env:ARCO_LHA_PATH = (Resolve-Path -LiteralPath $Lha).Path
    if ($PrepareCorpus) {
        & py -3 scripts/prepare-archive-corpus.py --seven-zip $env:ARCO_7ZIP_PATH
        if ($LASTEXITCODE -ne 0) { throw "Corpus preparation failed." }
    }
    $env:ARCO_ARCHIVE_CORPUS = (Resolve-Path -LiteralPath "target/area4/corpus").Path
    if (-not (Test-Path -LiteralPath "target/area4/corpus/manifest.json")) { throw "Prepare the corpus with -PrepareCorpus first." }
    $filters = @("archive_tests::", "external_archives::tests::", "packer_profile::tests::", "ace::tests::")
    if ($WcxPlugin) {
        $env:ARCO_TEST_WCX_ZONE = (Resolve-Path -LiteralPath $WcxPlugin).Path
        $filters += "plugin::operations::tests::wcx_zone_policy"
    }
    foreach ($filter in $filters) {
        $arguments = @("test", "-p", "arco-core", "--lib", $filter, "--locked")
        if ($Offline) { $arguments += "--offline" }
        $arguments += @("--", "--ignored", "--test-threads=1")
        if (Get-Command rtk -ErrorAction SilentlyContinue) { & rtk cargo @arguments }
        else { & cargo @arguments }
        if ($LASTEXITCODE -ne 0) { throw "Archive reference tests failed: $filter" }
    }
}
finally {
    foreach ($name in $saved.Keys) { [Environment]::SetEnvironmentVariable($name, $saved[$name], "Process") }
    Pop-Location
}
