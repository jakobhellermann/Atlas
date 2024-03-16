use slint_build::CompilerConfiguration;

fn main() {
    #[cfg(target_os = "macos")]
    let theme = "cupertino-dark";
    #[cfg(not(target_os = "macos"))]
    let theme = "fluent-dark";

    slint_build::compile_with_config(
        "ui/ui.slint",
        CompilerConfiguration::new().with_style(theme.into()),
    )
    .unwrap();
}
