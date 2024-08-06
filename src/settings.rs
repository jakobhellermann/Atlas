use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Default)]
pub struct Settings {
    pub celeste_path: Option<PathBuf>,
}

fn settings_dir() -> PathBuf {
    dirs::config_dir()
        .expect("could not find config dir")
        .join("Atlas")
}
const SETTINGS_FILE_NAME: &str = "settings.toml";

pub fn read_settings() -> Result<Settings> {
    let settings_file = settings_dir().join(SETTINGS_FILE_NAME);
    if !settings_file.exists() {
        return Ok(Settings::default());
    }

    let contents = std::fs::read_to_string(settings_file)?;
    let settings = toml::from_str(&contents)?;

    Ok(settings)
}
pub fn write_settings(settings: Settings) -> Result<()> {
    let dir = settings_dir();
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join(SETTINGS_FILE_NAME),
        toml::to_string_pretty(&settings)?,
    )?;

    Ok(())
}
