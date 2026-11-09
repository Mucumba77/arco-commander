fn main() {
    let translations = bundle_translations();
    // The default Windows thread stack is small for Slint's layout compiler.
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            let config = slint_build::CompilerConfiguration::new()
                .with_style("fluent".into())
                .with_default_translation_context(slint_build::DefaultTranslationContext::None)
                .with_bundled_translations(translations)
                .embed_resources(slint_build::EmbedResourcesKind::EmbedFiles);
            slint_build::compile_with_config("ui/main.slint", config)
                .expect("compile Arco Commander UI");
        })
        .expect("start UI compiler")
        .join()
        .expect("UI compiler failed");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bin=arco-commander=/STACK:8388608");
    }
}

fn bundle_translations() -> std::path::PathBuf {
    use std::fmt::Write;
    use std::path::PathBuf;

    let directory =
        PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR")).join("translations");
    for (language, source_file, catalog) in [
        ("en-US", "i18n/en.tsv", include_str!("i18n/en.tsv")),
        ("fr-FR", "i18n/fr.tsv", include_str!("i18n/fr.tsv")),
    ] {
        println!("cargo:rerun-if-changed={source_file}");
        let messages = directory.join(language).join("LC_MESSAGES");
        std::fs::create_dir_all(&messages).expect("create translation directory");
        let mut po = format!(
            "msgid \"\"\nmsgstr \"\"\n\"Content-Type: text/plain; charset=UTF-8\\n\"\n\"Language: {language}\\n\"\n",
        );
        let mut ids = std::collections::BTreeSet::new();
        for line in catalog.lines() {
            let (source, translation) = line.split_once('\t').expect("source and translation");
            assert!(
                ids.insert(source),
                "duplicate {language} translation for {source}"
            );
            writeln!(po, "\nmsgid {source:?}\nmsgstr {translation:?}").expect("write translation");
        }
        std::fs::write(messages.join("arco-app.po"), po).expect("write translations");
    }
    directory
}
