use clap::Parser;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct CliArgs {
    /// Port to serve HTML email content on
    #[arg(short = 'p', long = "port", default_value = "8080")]
    pub port: u16,

    /// Override config file path
    #[arg(short = 'c', long = "config")]
    pub config_path: Option<PathBuf>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    /// Path to the MailDir directory
    pub maildir_path: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            maildir_path: dirs::home_dir()
                .map(|home| home.join("Mail"))
                .unwrap_or_else(|| PathBuf::from("./Mail")),
        }
    }
}

impl Config {
    /// Load configuration from file, falling back to default locations
    pub fn load(config_path: Option<PathBuf>) -> Result<Self, Box<dyn std::error::Error>> {
        // Try explicit config path first
        if let Some(path) = config_path {
            if path.exists() {
                return Self::load_from_file(&path);
            } else {
                return Err(format!("Config file not found: {}", path.display()).into());
            }
        }

        // Try ~/.config/vulthor/config.toml
        if let Some(home) = dirs::home_dir() {
            let config_dir_path = home.join(".config/vulthor/config.toml");
            if config_dir_path.exists() {
                return Self::load_from_file(&config_dir_path);
            }
        }

        // Try ./vulthor.toml
        let local_config = PathBuf::from("./vulthor.toml");
        if local_config.exists() {
            return Self::load_from_file(&local_config);
        }

        // Return default config if no config file found
        Ok(Self::default())
    }

    fn load_from_file(path: &PathBuf) -> Result<Self, Box<dyn std::error::Error>> {
        let contents = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&contents)?;
        Ok(config)
    }

    /// Save current config to specified path  
    pub fn save(&self, path: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
        let contents = toml::to_string_pretty(self)?;

        // Create parent directory if it doesn't exist
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        std::fs::write(path, contents)?;
        Ok(())
    }
}
