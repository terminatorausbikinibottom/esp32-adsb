use slint_build::{CompilerConfiguration, EmbedResourcesKind};

fn main() {
    // Bitmap glyphs are baked in for the software renderer: no runtime font loading, same on every target.
    let cfg =
        CompilerConfiguration::new().embed_resources(EmbedResourcesKind::EmbedForSoftwareRenderer);
    slint_build::compile_with_config("ui/app.slint", cfg).unwrap();
}
