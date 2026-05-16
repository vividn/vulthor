use crate::error::{Result, VulthorError};
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Command-line arguments parsed by `clap` at startup. Each option may
/// override the corresponding value from the resolved [`Config`] —
/// `maildir_path` in particular wins over both the config file and
/// `default_account`'s maildir.
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

/// A single configured account. One per `[accounts.<key>]` section in
/// `vulthor.toml`. The TOML table key becomes the [`AccountId`]; `name`
/// is the human-facing display label rendered in the Accounts pane.
///
/// [`AccountId`]: crate::components::AccountId
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct AccountConfig {
    /// Human-facing label rendered in the Accounts pane.
    pub name: String,
    /// Account's "From" address. Used by compose / send.
    pub email: String,
    /// On-disk MailDir root for this account.
    pub maildir_path: PathBuf,
    /// Optional; required for sending mail, but read-only multi-account
    /// switching does not need it.
    #[serde(default)]
    pub smtp_command: Option<String>,
    /// Optional trailing signature appended by the compose flow when
    /// templating a new draft.
    #[serde(default)]
    pub signature: Option<String>,
}

/// Top-level configuration loaded from `vulthor.toml`. Search order is
/// `-c <path>` → `~/.config/vulthor/config.toml` → `./vulthor.toml` →
/// [`Config::default`]. Holds the global maildir fallback plus the
/// `[accounts.*]` table that drives the Accounts pane.
#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    /// Path to the MailDir directory. Used when no `[accounts.*]` table
    /// is configured (single-account compat) or as a final fallback when
    /// `default_account` does not resolve.
    pub maildir_path: PathBuf,
    /// TOML key of the account active on startup. Falls back to the
    /// first account in alphabetical order when unset.
    #[serde(default)]
    pub default_account: Option<String>,
    /// `[accounts.<key>]` sections. `BTreeMap` gives a deterministic
    /// (alphabetical) iteration order for the Accounts pane.
    #[serde(default)]
    pub accounts: BTreeMap<String, AccountConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            maildir_path: dirs::home_dir()
                .map(|home| home.join("Mail"))
                .unwrap_or_else(|| PathBuf::from("./Mail")),
            default_account: None,
            accounts: BTreeMap::new(),
        }
    }
}

impl Config {
    /// Ordered list of `(account_id, account)` pairs. Empty when no
    /// `[accounts.*]` tables are configured. Stable across calls.
    pub fn ordered_accounts(&self) -> Vec<(String, AccountConfig)> {
        self.accounts
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Index of the account to activate on startup. Resolves
    /// `default_account` against the ordered list; falls back to 0 when
    /// the key is missing or unset. Returns `None` when no accounts are
    /// configured.
    pub fn default_account_index(&self) -> Option<usize> {
        if self.accounts.is_empty() {
            return None;
        }
        let ordered = self.ordered_accounts();
        if let Some(key) = &self.default_account
            && let Some(idx) = ordered.iter().position(|(k, _)| k == key)
        {
            return Some(idx);
        }
        Some(0)
    }

    /// MailDir path that should back the store on startup. Returns the
    /// `default_account`'s `maildir_path` when multi-account is
    /// configured, otherwise the top-level `maildir_path`.
    pub fn active_maildir(&self) -> PathBuf {
        if let Some(idx) = self.default_account_index() {
            let ordered = self.ordered_accounts();
            return ordered[idx].1.maildir_path.clone();
        }
        self.maildir_path.clone()
    }

    /// True when more than one account is configured. Drives the
    /// Accounts pane visibility (single-account installs hide it per
    /// VISION.md § "Multi-Account").
    pub fn is_multi_account(&self) -> bool {
        self.accounts.len() > 1
    }
}

impl Config {
    /// Load configuration from file, falling back to default locations
    pub async fn load(config_path: Option<PathBuf>) -> Result<Self> {
        // Try explicit config path first
        if let Some(path) = config_path {
            if path.exists() {
                return Self::load_from_file(&path).await;
            } else {
                return Err(VulthorError::ConfigNotFound(path));
            }
        }

        // Try ~/.config/vulthor/config.toml
        if let Some(home) = dirs::home_dir() {
            let config_dir_path = home.join(".config/vulthor/config.toml");
            if config_dir_path.exists() {
                return Self::load_from_file(&config_dir_path).await;
            }
        }

        // Try ./vulthor.toml
        let local_config = PathBuf::from("./vulthor.toml");
        if local_config.exists() {
            return Self::load_from_file(&local_config).await;
        }

        // Return default config if no config file found
        Ok(Self::default())
    }

    async fn load_from_file(path: &PathBuf) -> Result<Self> {
        let contents = tokio::fs::read_to_string(path).await?;
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

        let args = CliArgs::parse_from(["vulthor"]);
        assert_eq!(args.port, 8080);
        assert!(args.config_path.is_none());
    }

    #[test]
    fn test_cli_args_port_override() {
        use clap::Parser;

        let args = CliArgs::parse_from(["vulthor", "-p", "3000"]);
        assert_eq!(args.port, 3000);

        let args = CliArgs::parse_from(["vulthor", "--port", "9090"]);
        assert_eq!(args.port, 9090);
    }

    #[test]
    fn test_cli_args_config_path() {
        use clap::Parser;

        let args = CliArgs::parse_from(["vulthor", "-c", "/custom/config.toml"]);
        assert_eq!(args.config_path, Some(PathBuf::from("/custom/config.toml")));

        let args = CliArgs::parse_from(["vulthor", "--config", "/another/path.toml"]);
        assert_eq!(args.config_path, Some(PathBuf::from("/another/path.toml")));
    }

    #[test]
    fn test_cli_args_maildir_path() {
        use clap::Parser;

        let args = CliArgs::parse_from(["vulthor", "-m", "/custom/maildir"]);
        assert_eq!(args.maildir_path, Some(PathBuf::from("/custom/maildir")));

        let args = CliArgs::parse_from(["vulthor", "--maildir", "/another/maildir"]);
        assert_eq!(args.maildir_path, Some(PathBuf::from("/another/maildir")));
    }

    #[test]
    fn test_cli_args_combined() {
        use clap::Parser;

        let args = CliArgs::parse_from([
            "vulthor",
            "-p",
            "9000",
            "-c",
            "/config.toml",
            "-m",
            "/maildir",
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
            ..Config::default()
        };

        let toml_str = toml::to_string(&config).unwrap();
        assert!(toml_str.contains("maildir_path"));
        assert!(toml_str.contains("/test/maildir"));

        // Test deserialization
        let deserialized: Config = toml::from_str(&toml_str).unwrap();
        assert_eq!(deserialized.maildir_path, PathBuf::from("/test/maildir"));
    }

    #[tokio::test]
    async fn test_config_load_with_explicit_path_exists() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("config.toml");

        let test_config = Config {
            maildir_path: PathBuf::from("/custom/mail/path"),
            ..Config::default()
        };

        // Write test config
        let contents = toml::to_string(&test_config).unwrap();
        fs::write(&config_path, contents).unwrap();

        // Load it back
        let loaded_config = Config::load(Some(config_path)).await.unwrap();
        assert_eq!(
            loaded_config.maildir_path,
            PathBuf::from("/custom/mail/path")
        );
    }

    #[tokio::test]
    async fn test_config_load_with_explicit_path_not_exists() {
        let non_existent_path = PathBuf::from("/definitely/does/not/exist/config.toml");
        let result = Config::load(Some(non_existent_path)).await;
        assert!(result.is_err());

        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("Config file not found"));
    }

    #[tokio::test]
    async fn test_config_load_fallback_to_default() {
        // When no explicit path and no config files exist
        let result = Config::load(None).await;
        assert!(result.is_ok());

        let config = result.unwrap();
        // Should be default config
        assert!(config.maildir_path.to_string_lossy().contains("Mail"));
    }

    #[tokio::test]
    async fn test_config_load_from_home_config_dir() {
        // This test creates a config file in a temp directory and simulates
        // the ~/.config/vulthor/config.toml scenario
        let temp_dir = TempDir::new().unwrap();
        let config_content = r#"maildir_path = "/home/user/TestMail""#;

        let config_file = temp_dir.path().join("config.toml");
        fs::write(&config_file, config_content).unwrap();

        // Load from the file directly (simulating home config scenario)
        let result = Config::load_from_file(&config_file).await;
        assert!(result.is_ok());

        let config = result.unwrap();
        assert_eq!(config.maildir_path, PathBuf::from("/home/user/TestMail"));
    }

    #[tokio::test]
    async fn test_config_load_from_local_config() {
        let temp_dir = TempDir::new().unwrap();
        let config_content = r#"maildir_path = "/project/local/mail""#;

        let config_file = temp_dir.path().join("vulthor.toml");
        fs::write(&config_file, config_content).unwrap();

        // Load from the file directly (simulating local config scenario)
        let result = Config::load_from_file(&config_file).await;
        assert!(result.is_ok());

        let config = result.unwrap();
        assert_eq!(config.maildir_path, PathBuf::from("/project/local/mail"));
    }

    #[tokio::test]
    async fn test_config_load_from_file_invalid_toml() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("invalid.toml");

        // Write invalid TOML content
        fs::write(&config_path, "invalid toml content [[[").unwrap();

        let result = Config::load_from_file(&config_path).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_config_load_from_file_missing_fields() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("incomplete.toml");

        // Write TOML with missing required field
        fs::write(&config_path, r#"some_other_field = "value""#).unwrap();

        let result = Config::load_from_file(&config_path).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_config_with_relative_path() {
        let config = Config {
            maildir_path: PathBuf::from("./relative/mail/path"),
            ..Config::default()
        };

        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("config.toml");

        // Save and load relative path
        let contents = toml::to_string(&config).unwrap();
        fs::write(&config_path, contents).unwrap();
        let loaded_config = Config::load(Some(config_path)).await.unwrap();

        assert_eq!(
            loaded_config.maildir_path,
            PathBuf::from("./relative/mail/path")
        );
    }

    #[tokio::test]
    async fn test_config_with_unicode_path() {
        let config = Config {
            maildir_path: PathBuf::from("/home/用户/邮件"),
            ..Config::default()
        };

        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("unicode_config.toml");

        // Save and load unicode path
        let contents = toml::to_string(&config).unwrap();
        let save_result = fs::write(&config_path, contents);
        assert!(save_result.is_ok());

        let loaded_config = Config::load(Some(config_path)).await.unwrap();
        assert_eq!(loaded_config.maildir_path, PathBuf::from("/home/用户/邮件"));
    }

    #[test]
    fn multi_account_toml_round_trips() {
        // Per VISION.md § "Multi-Account": `[accounts.<key>]` tables
        // carry name / email / maildir_path (required) plus optional
        // smtp_command / signature. We confirm the deserializer picks
        // them up, that ordered_accounts() sorts by key, and that
        // default_account_index() honors the configured key.
        let toml_str = r#"
maildir_path = "/legacy/path"
default_account = "personal"

[accounts.work]
name = "Work"
email = "me@company.com"
maildir_path = "/Mail/work"
smtp_command = "msmtp -a work"
signature = "Best,\nMe"

[accounts.personal]
name = "Personal"
email = "me@personal.tld"
maildir_path = "/Mail/personal"
smtp_command = "msmtp -a personal"
"#;
        let cfg: Config = toml::from_str(toml_str).expect("parses");
        assert_eq!(cfg.accounts.len(), 2);
        assert_eq!(cfg.default_account.as_deref(), Some("personal"));

        let ordered = cfg.ordered_accounts();
        // BTreeMap → alphabetical: personal before work.
        assert_eq!(ordered[0].0, "personal");
        assert_eq!(ordered[1].0, "work");
        assert_eq!(ordered[0].1.name, "Personal");
        assert_eq!(ordered[1].1.email, "me@company.com");

        // default_account "personal" lives at index 0.
        assert_eq!(cfg.default_account_index(), Some(0));
        // active_maildir resolves to the default account's path, not
        // the top-level legacy fallback.
        assert_eq!(cfg.active_maildir(), PathBuf::from("/Mail/personal"));
        assert!(cfg.is_multi_account());
    }

    #[test]
    fn single_account_config_is_not_multi_account() {
        // One [accounts.*] block is still "single-account" for the
        // purposes of hiding the Accounts pane (per VISION.md).
        let toml_str = r#"
maildir_path = "/legacy/path"

[accounts.solo]
name = "Solo"
email = "me@solo.tld"
maildir_path = "/Mail/solo"
"#;
        let cfg: Config = toml::from_str(toml_str).expect("parses");
        assert_eq!(cfg.accounts.len(), 1);
        assert!(!cfg.is_multi_account());
        // No default_account configured — falls back to first account.
        assert_eq!(cfg.default_account_index(), Some(0));
        assert_eq!(cfg.active_maildir(), PathBuf::from("/Mail/solo"));
    }

    #[test]
    fn no_accounts_falls_back_to_legacy_maildir_path() {
        // Existing single-account configs that pre-date the
        // `[accounts.*]` schema must keep working unchanged.
        let toml_str = r#"maildir_path = "/legacy/Mail""#;
        let cfg: Config = toml::from_str(toml_str).expect("parses");
        assert!(cfg.accounts.is_empty());
        assert_eq!(cfg.default_account_index(), None);
        assert!(!cfg.is_multi_account());
        assert_eq!(cfg.active_maildir(), PathBuf::from("/legacy/Mail"));
    }

    #[test]
    fn default_account_with_unknown_key_falls_back_to_first() {
        // Typo in default_account → don't crash; pick the first
        // account in the alphabetical iteration order.
        let toml_str = r#"
maildir_path = "/legacy/path"
default_account = "does-not-exist"

[accounts.work]
name = "Work"
email = "w@x.tld"
maildir_path = "/Mail/work"
"#;
        let cfg: Config = toml::from_str(toml_str).expect("parses");
        assert_eq!(cfg.default_account_index(), Some(0));
        assert_eq!(cfg.active_maildir(), PathBuf::from("/Mail/work"));
    }

    #[test]
    fn test_config_error_handling_file_permission() {
        // This test might not work on all systems, but tests error handling
        let config = Config {
            maildir_path: PathBuf::from("/test/path"),
            ..Config::default()
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
