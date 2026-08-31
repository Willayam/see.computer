//! Persistent user preferences.

use crate::trigger::Trigger;
use std::io;

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Config {
    pub trigger: Trigger,
    #[serde(default = "history_default")]
    pub history: bool,
}

fn history_default() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self {
            trigger: Trigger::LeftOption,
            history: true,
        }
    }
}

impl Config {
    pub fn load() -> Self {
        std::fs::read(crate::paths::config())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> io::Result<()> {
        let path = crate::paths::config();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        std::fs::write(path, bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_config_shape_enables_history() {
        let config: Config = serde_json::from_str(r#"{"trigger":"left-option"}"#).unwrap();
        assert_eq!(config.trigger, Trigger::LeftOption);
        assert!(config.history);
    }
}
