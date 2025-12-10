use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

fn default_prefix() -> String {
    "rcrd-call-".into()
}

fn default_language() -> String {
    "en".into()
}

fn default_backend() -> String {
    "openblas".into()
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct Config {
    /// Prefix used for generated output filenames (datetime appended).
    pub file_prefix: String,
    /// Path to whisper.cpp model file (ggml/gguf).
    pub whisper_model: Option<PathBuf>,
    /// Default transcription language (e.g., "en", "fr").
    pub language: Option<String>,
    /// Whisper backend: "vulkan" (GPU) or "openblas" (CPU).
    pub backend: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            file_prefix: default_prefix(),
            whisper_model: None,
            language: Some(default_language()),
            backend: default_backend(),
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

pub fn save_config(cfg: &Config) -> Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_string_pretty(cfg)?;
    fs::write(&path, data)?;
    Ok(())
}

pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("rcrd")
        .join("config.json")
}
