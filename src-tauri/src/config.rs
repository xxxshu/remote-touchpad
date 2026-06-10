use std::path::Path;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// Application configuration — persisted as JSON.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppConfig {
    /// Custom IME toggle key combo (e.g. "shift", "ctrl+space", "capslock").
    /// `None` means use the platform default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ime_toggle_key: Option<String>,
}

/// Load config from a JSON file. Returns default if the file doesn't exist or is invalid.
pub fn load_config(path: &Path) -> AppConfig {
    let config_path = path.join("config.json");
    match std::fs::read_to_string(&config_path) {
        Ok(content) => {
            match serde_json::from_str::<AppConfig>(&content) {
                Ok(config) => {
                    info!("Config loaded from {:?}", config_path);
                    config
                }
                Err(e) => {
                    warn!("Invalid config file {:?}: {}, using defaults", config_path, e);
                    AppConfig::default()
                }
            }
        }
        Err(_) => {
            info!("No config file at {:?}, using defaults", config_path);
            AppConfig::default()
        }
    }
}

/// Save config to a JSON file. Creates the directory if needed.
pub fn save_config(path: &Path, config: &AppConfig) -> Result<(), String> {
    let config_path = path.join("config.json");
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create config dir: {}", e))?;
    }
    let json = serde_json::to_string_pretty(config)
        .map_err(|e| format!("Failed to serialize config: {}", e))?;
    std::fs::write(&config_path, json)
        .map_err(|e| format!("Failed to write config: {}", e))?;
    info!("Config saved to {:?}", config_path);
    Ok(())
}
