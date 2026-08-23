fn main() {
    let config = slint_build::CompilerConfiguration::new()
        .with_style("fluent".into())
        .embed_resources(slint_build::EmbedResourcesKind::EmbedFiles);
    slint_build::compile_with_config("ui/app.slint", config).expect("failed to compile K3 GUI");
}
