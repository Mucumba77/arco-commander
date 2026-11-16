# WLX-Miniaturreferenz

Eigenes SDK-Testplugin ohne Fenster. Es prüft Initialisierung, gewünschte Abmessungen und die ersten 8192 Headerbytes und liefert eine kleine Offscreen-Bitmap oder eine absichtlich zu große Bitmap zurück. Die x86-Version exportiert ANSI, die x64-Version Unicode.

In der passenden Visual-Studio-Entwicklershell:

```powershell
# x86
cl /nologo /LD /W4 /O2 preview.c gdi32.lib /link /OUT:preview.wlx
# x64
cl /nologo /LD /W4 /O2 preview.c gdi32.lib /link /OUT:preview.wlx64
```

`ARCO_TEST_WLX_BITMAP_X86` und `ARCO_TEST_WLX_BITMAP_X64` auf die absoluten DLL-Pfade setzen. Beide Plugin-Hosts müssen neben dem Build liegen und Protokoll 13 verwenden.

```powershell
cargo test -p arco-core wlx_thumbnail_batch_calls_both_native_exports_and_rejects_oversize -- --ignored --test-threads=1
cargo test -p arco-core wlx_load_budget_is_separate_from_the_preview_callback_deadline -- --ignored --test-threads=1
```

Der zweite Test benötigt etwa 33 Sekunden. Nur die Test-DLL wertet `ARCO_TEST_WLX_LOAD_DELAY_MS` und `ARCO_TEST_WLX_PREVIEW_DELAY_MS` aus (höchstens 15000 ms). Der Test setzt jeweils 11000 ms in einem eigenen Unterprozess: Langsames Laden muss gelingen, eine blockierte Vorschaufunktion muss weiterhin das 10-Sekunden-Limit erreichen. Es werden keine Fenster geöffnet.
