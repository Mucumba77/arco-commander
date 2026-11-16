# Headless WFX content-column references

`columns.c` provides controlled SDK exports for field units, typed values, sort defaults and optional default views. `ansi.def` is used for x86; `unicode.def` with `WIDE` for x64; `no-default-view.def` tests an ordinary missing-export fallback. These plugins have no login or preview window.

Build from the repository root in the matching Visual Studio developer command prompts. Create `target/content-fixtures` first. Keep all compiler output in that directory, including `/IMPLIB`:

```bat
rem x86 developer command prompt
cl /nologo /LD /W4 /WX /Fotarget/content-fixtures/ansi.obj crates/arco-core/tests/fixtures/wfx-columns/columns.c /link /DEF:crates/arco-core/tests/fixtures/wfx-columns/ansi.def /IMPLIB:target/content-fixtures/ansi.lib /OUT:target/content-fixtures/ansi.wfx
rem x64 developer command prompt
cl /nologo /LD /W4 /WX /DWIDE /Fotarget/content-fixtures/unicode.obj crates/arco-core/tests/fixtures/wfx-columns/columns.c /link /DEF:crates/arco-core/tests/fixtures/wfx-columns/unicode.def /IMPLIB:target/content-fixtures/unicode.lib /OUT:target/content-fixtures/unicode.wfx64
cl /nologo /LD /W4 /WX /Fotarget/content-fixtures/plain.obj crates/arco-core/tests/fixtures/wfx-columns/columns.c /link /DEF:crates/arco-core/tests/fixtures/wfx-columns/no-default-view.def /IMPLIB:target/content-fixtures/plain.lib /OUT:target/content-fixtures/plain.wfx64
```

Build both plugin hosts with the current protocol version and place copies next to the test executables in `target/debug/deps`. Set these environment variables to absolute fixture paths:

- `ARCO_TEST_WFX_COLUMNS_X86`: `ansi.wfx`
- `ARCO_TEST_WFX_COLUMNS_X64`: `unicode.wfx64`
- `ARCO_TEST_WFX_NO_DEFAULT_VIEW`: `plain.wfx64`
- `ARCO_TEST_WDX_COLUMNS`: x64 DLL built from `../wdx-search/score.c` (extension `.wdx64`)

Run only the named ignored references; other ignored tests may need Windows privileges, servers, codecs or native windows:

```text
cargo test -p arco-core plugin::wfx_content::tests::wfx_ -- --ignored --test-threads=1
cargo test -p arco-app composite_sdk_columns_roundtrip_ansi_unicode_typed_keys_and_local_wdx -- --ignored --test-threads=1
```

The app test calls the actual shared background-column evaluator directly: composite native/WFX values, typed numeric keys, ANSI/Unicode hosts, and composite local WDX fields. It does not construct an application window or run Arco Commander.
