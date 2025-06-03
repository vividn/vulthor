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

    /// Override MailDir path (takes precedence over config file)
    #[arg(short = 'm', long = "maildir")]
    pub maildir_path: Option<PathBuf>,
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_cli_args_default_values() {
        use clap::Parser;

        let args = CliArgs::parse_from(&["vulthor"]);
        assert_eq!(args.port, 8080);
        assert!(args.config_path.is_none());
    }

    #[test]
    fn test_cli_args_port_override() {
        use clap::Parser;

        let args = CliArgs::parse_from(&["vulthor", "-p", "3000"]);
        assert_eq!(args.port, 3000);

        let args = CliArgs::parse_from(&["vulthor", "--port", "9090"]);
        assert_eq!(args.port, 9090);
    }

    #[test]
    fn test_cli_args_config_path() {
        use clap::Parser;

        let args = CliArgs::parse_from(&["vulthor", "-c", "/custom/config.toml"]);
        assert_eq!(args.config_path, Some(PathBuf::from("/custom/config.toml")));

        let args = CliArgs::parse_from(&["vulthor", "--config", "/another/path.toml"]);
        assert_eq!(args.config_path, Some(PathBuf::from("/another/path.toml")));
    }

    #[test]
    fn test_cli_args_maildir_path() {
        use clap::Parser;

        let args = CliArgs::parse_from(&["vulthor", "-m", "/custom/maildir"]);
        assert_eq!(args.maildir_path, Some(PathBuf::from("/custom/maildir")));

        let args = CliArgs::parse_from(&["vulthor", "--maildir", "/another/maildir"]);
        assert_eq!(args.maildir_path, Some(PathBuf::from("/another/maildir")));
    }

    #[test]
    fn test_cli_args_combined() {
        use clap::Parser;

        let args = CliArgs::parse_from(&[
            "vulthor",
            "-p", "9000",
            "-c", "/config.toml",
            "-m", "/maildir"
        ]);
        assert_eq!(args.port, 9000);
        assert_eq!(args.config_path, Some(PathBuf::from("/config.toml")));
        assert_eq!(args.maildir_path, Some(PathBuf::from("/maildir")));
    }

    #[test]
    fn test_config_default() {
        let config = Config::default();
        assert!(config.maildir_path.to_string_lossy().contains("Mail"));

        // Should either be ~/Mail or ./Mail as fallback
        let path_str = config.maildir_path.to_string_lossy();
        assert!(path_str.ends_with("Mail"));
    }

    #[test]
    fn test_config_serialization() {
        let config = Config {
            maildir_path: PathBuf::from("/test/maildir"),
        };

        let toml_str = toml::to_string(&config).unwrap();
        assert!(toml_str.contains("maildir_path"));
        assert!(toml_str.contains("/test/maildir"));

        // Test deserialization
        let deserialized: Config = toml::from_str(&toml_str).unwrap();
        assert_eq!(deserialized.maildir_path, PathBuf::from("/test/maildir"));
    }

    #[test]
    fn test_config_load_with_explicit_path_exists() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("config.toml");

        let test_config = Config {
            maildir_path: PathBuf::from("/custom/mail/path"),
        };

        // Write test config
        let contents = toml::to_string(&test_config).unwrap();
        fs::write(&config_path, contents).unwrap();

        // Load it back
        let loaded_config = Config::load(Some(config_path)).unwrap();
        assert_eq!(
            loaded_config.maildir_path,
            PathBuf::from("/custom/mail/path")
        );
    }

    #[test]
    fn test_config_load_with_explicit_path_not_exists() {
        let non_existent_path = PathBuf::from("/definitely/does/not/exist/config.toml");
        let result = Config::load(Some(non_existent_path));
        assert!(result.is_err());

        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("Config file not found"));
    }

    #[test]
    fn test_config_load_fallback_to_default() {
        // When no explicit path and no config files exist
        let result = Config::load(None);
        assert!(result.is_ok());

        let config = result.unwrap();
        // Should be default config
        assert!(config.maildir_path.to_string_lossy().contains("Mail"));
    }

    #[test]
    fn test_config_load_from_home_config_dir() {
        // This test creates a config file in a temp directory and simulates
        // the ~/.config/vulthor/config.toml scenario
        let temp_dir = TempDir::new().unwrap();
        let config_content = r#"maildir_path = "/home/user/TestMail""#;

        let config_file = temp_dir.path().join("config.toml");
        fs::write(&config_file, config_content).unwrap();

        // Load from the file directly (simulating home config scenario)
        let result = Config::load_from_file(&config_file);
        assert!(result.is_ok());

        let config = result.unwrap();
        assert_eq!(config.maildir_path, PathBuf::from("/home/user/TestMail"));
    }

    #[test]
    fn test_config_load_from_local_config() {
        let temp_dir = TempDir::new().unwrap();
        let config_content = r#"maildir_path = "/project/local/mail""#;

        let config_file = temp_dir.path().join("vulthor.toml");
        fs::write(&config_file, config_content).unwrap();

        // Load from the file directly (simulating local config scenario)
        let result = Config::load_from_file(&config_file);
        assert!(result.is_ok());

        let config = result.unwrap();
        assert_eq!(config.maildir_path, PathBuf::from("/project/local/mail"));
    }

    #[test]
    fn test_config_load_from_file_invalid_toml() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("invalid.toml");

        // Write invalid TOML content
        fs::write(&config_path, "invalid toml content [[[").unwrap();

        let result = Config::load_from_file(&config_path);
        assert!(result.is_err());
    }

    #[test]
    fn test_config_load_from_file_missing_fields() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("incomplete.toml");

        // Write TOML with missing required field
        fs::write(&config_path, r#"some_other_field = "value""#).unwrap();

        let result = Config::load_from_file(&config_path);
        assert!(result.is_err());
    }

    #[test]
    fn test_config_with_relative_path() {
        let config = Config {
            maildir_path: PathBuf::from("./relative/mail/path"),
        };

        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("config.toml");

        // Save and load relative path
        let contents = toml::to_string(&config).unwrap();
        fs::write(&config_path, contents).unwrap();
        let loaded_config = Config::load(Some(config_path)).unwrap();

        assert_eq!(
            loaded_config.maildir_path,
            PathBuf::from("./relative/mail/path")
        );
    }

    #[test]
    fn test_config_with_unicode_path() {
        let config = Config {
            maildir_path: PathBuf::from("/home/用户/邮件"),
        };

        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("unicode_config.toml");

        // Save and load unicode path
        let contents = toml::to_string(&config).unwrap();
        let save_result = fs::write(&config_path, contents);
        assert!(save_result.is_ok());

        let loaded_config = Config::load(Some(config_path)).unwrap();
        assert_eq!(loaded_config.maildir_path, PathBuf::from("/home/用户/邮件"));
    }

    #[test]
    fn test_config_error_handling_file_permission() {
        // This test might not work on all systems, but tests error handling
        let config = Config {
            maildir_path: PathBuf::from("/test/path"),
        };

        // Try to save to a path that should fail (like root directory on Unix)
        let invalid_path = PathBuf::from("/root/cannot_write_here.toml");
        let contents = toml::to_string(&config).unwrap();
        let result = fs::write(&invalid_path, contents);

        // Should fail gracefully (unless running as root)
        if !std::env::var("USER").unwrap_or_default().eq("root") {
            assert!(result.is_err());
        }
    }
}
