use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

fn default_prefix() -> String {
    "rcrd-call-".into()
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct Config {
    /// Prefix used for generated output filenames (datetime appended).
    pub file_prefix: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            file_prefix: default_prefix(),
        }
    }
}

pub fn load_config() -> Result<Config> {
    let path = config_path();
    if !path.exists() {
        return Ok(Config::default());
    }
    let data =
        fs::read_to_string(&path).with_context(|| format!("reading config {}", path.display()))?;
    let cfg: Config = serde_json::from_str(&data)
        .with_context(|| format!("parsing config {}", path.display()))?;
    Ok(cfg)
}


pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("rcrd")
        .join("config.json")
}
