use slint_build::CompilerConfiguration;

fn main() {
    #[cfg(target_os = "macos")]
    let theme = "cupertino";
    #[cfg(not(target_os = "macos"))]
    let theme = "fluent";

    slint_build::compile_with_config(
        "ui/ui.slint",
        CompilerConfiguration::new().with_style(theme.into()),
    )
    .unwrap();
}
