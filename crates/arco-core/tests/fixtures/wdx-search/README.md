# WDX-Suchreferenz

Eigenes, schreibgeschütztes SDK-Testplugin: Feld `Score`, Einheiten `raw` und `scaled`, Werte 40 und 41. Es öffnet kein Fenster. In der x64-Entwicklershell:

```powershell
cl /nologo /LD /W4 /O2 score.c /link /OUT:score.wdx64
```

Für ANSI in der x86-Entwicklershell dasselbe mit `/OUT:score.wdx`. `ARCO_TEST_WDX_DUPLICATES` auf den absoluten DLL-Pfad setzen. Der passende `arco-plugin-host` muss im Test-Build vorhanden sein.

```powershell
cargo test -p arco-app shared_search_profile_filters_local_and_encrypted_archive_files_with_wdx -- --ignored --test-threads=1
cargo test -p arco-app duplicate_fields_read_typed_plugin_values_with_named_units -- --ignored --test-threads=1
```
