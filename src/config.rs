use serde::Deserialize;
use std::path::Path;
use tracing::info;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub port: u16,
    pub log_level: String,
    pub audio_sample_rate: u32,
    pub audio_channels: u16,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: 8080,
            log_level: "info".to_string(),
            audio_sample_rate: 48000,
            audio_channels: 1,
        }
    }
}

impl Config {
    /// Load config from file (if it exists) then apply environment variable overrides.
    pub fn load() -> anyhow::Result<Self> {
        let mut config = if Path::new("config.toml").exists() {
            let contents = std::fs::read_to_string("config.toml")?;
            info!("Loaded config from config.toml");
            toml::from_str(&contents)?
        } else {
            info!("No config.toml found, using defaults");
            Config::default()
        };

        // Apply environment variable overrides (WHCANRC_ prefix)
        if let Ok(val) = std::env::var("WHCANRC_PORT") {
            config.port = val.parse()?;
        }
        if let Ok(val) = std::env::var("WHCANRC_LOG_LEVEL") {
            config.log_level = val;
        }
        if let Ok(val) = std::env::var("WHCANRC_AUDIO_SAMPLE_RATE") {
            config.audio_sample_rate = val.parse()?;
        }
        if let Ok(val) = std::env::var("WHCANRC_AUDIO_CHANNELS") {
            config.audio_channels = val.parse()?;
        }

        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.port, 8080);
        assert_eq!(config.log_level, "info");
        assert_eq!(config.audio_sample_rate, 48000);
        assert_eq!(config.audio_channels, 1);
    }

    #[test]
    fn test_parse_config_toml() {
        let toml_str = r#"
            port = 9090
            log_level = "debug"
            audio_sample_rate = 44100
            audio_channels = 2
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.port, 9090);
        assert_eq!(config.log_level, "debug");
        assert_eq!(config.audio_sample_rate, 44100);
        assert_eq!(config.audio_channels, 2);
    }

    #[test]
    fn test_partial_config_uses_defaults() {
        let toml_str = r#"
            port = 3000
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.port, 3000);
        assert_eq!(config.log_level, "info");
        assert_eq!(config.audio_sample_rate, 48000);
        assert_eq!(config.audio_channels, 1);
    }

    #[test]
    fn test_env_var_overrides() {
        // Set env vars
        std::env::set_var("WHCANRC_PORT", "7777");
        std::env::set_var("WHCANRC_LOG_LEVEL", "trace");

        let config = Config::load().unwrap();
        assert_eq!(config.port, 7777);
        assert_eq!(config.log_level, "trace");

        // Clean up
        std::env::remove_var("WHCANRC_PORT");
        std::env::remove_var("WHCANRC_LOG_LEVEL");
    }
}
