//! Persistent user preferences.

use std::io;
use std::path::PathBuf;

use crate::trigger::Trigger;

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Config {
    pub trigger: Trigger,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            trigger: Trigger::LeftOption,
        }
    }
}

impl Config {
    pub fn load() -> Self {
        std::fs::read(config_path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> io::Result<()> {
        let path = config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        std::fs::write(path, bytes)
    }
}

pub fn config_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("see.computer")
        .join("config.json")
}
