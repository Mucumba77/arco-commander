fn main() {
    // The default Windows thread stack is small for Slint's layout compiler.
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let config = slint_build::CompilerConfiguration::new()
                .with_style("fluent".into())
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
