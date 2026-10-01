fn main() {
    // Windows 的默认主线程栈不足以编译完整的 Slint UI。
    std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            let config = slint_build::CompilerConfiguration::new()
                .with_style("fluent".into())
                .embed_resources(slint_build::EmbedResourcesKind::EmbedFiles)
                .with_bundled_translations("translations")
                .with_default_translation_context(slint_build::DefaultTranslationContext::None);
            slint_build::compile_with_config("ui/app.slint", config)
                .expect("failed to compile K3 GUI");
        })
        .expect("failed to start K3 GUI build worker")
        .join()
        .expect("K3 GUI build worker panicked");
}
