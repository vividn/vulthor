// First slice of the thiserror migration epic (vu-ri8).
//
// This enum is intentionally narrow: it covers what `config.rs` and `email.rs`
// produce today. Other modules still return `Box<dyn Error>` and will migrate
// in follow-on tasks. `VulthorError` implements `std::error::Error`, so the
// `?` operator coerces it to `Box<dyn Error>` at module boundaries — no
// bridging code is needed at callers.

use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum VulthorError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Failed to parse TOML config: {0}")]
    ParseToml(#[from] toml::de::Error),

    #[error("Config file not found: {0}")]
    ConfigNotFound(PathBuf),

    #[error("Failed to parse email")]
    MailParser,

    #[error("Invalid folder path")]
    InvalidFolderPath,

    // Bridge variant for errors bubbling up from modules not yet migrated
    // (e.g. maildir scanner). Remove once those modules return VulthorError.
    #[error("MailDir error: {0}")]
    MailDir(String),
}

pub type Result<T> = std::result::Result<T, VulthorError>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    #[test]
    fn io_error_converts_via_from() {
        let io_err = io::Error::new(io::ErrorKind::NotFound, "missing");
        let err: VulthorError = io_err.into();
        assert!(matches!(err, VulthorError::Io(_)));
        assert!(err.to_string().contains("I/O error"));
    }

    #[test]
    fn toml_error_converts_via_from() {
        let toml_err = toml::from_str::<toml::Value>("invalid [[[").unwrap_err();
        let err: VulthorError = toml_err.into();
        assert!(matches!(err, VulthorError::ParseToml(_)));
        assert!(err.to_string().contains("Failed to parse TOML config"));
    }

    #[test]
    fn config_not_found_display_includes_path() {
        let err = VulthorError::ConfigNotFound(PathBuf::from("/no/such/path.toml"));
        let msg = err.to_string();
        assert!(msg.contains("Config file not found"));
        assert!(msg.contains("/no/such/path.toml"));
    }

    #[test]
    fn mail_parser_variant_has_display() {
        let err = VulthorError::MailParser;
        assert!(err.to_string().contains("Failed to parse email"));
    }

    #[test]
    fn invalid_folder_path_variant_has_display() {
        let err = VulthorError::InvalidFolderPath;
        assert!(err.to_string().contains("Invalid folder path"));
    }

    #[test]
    fn maildir_variant_wraps_message() {
        let err = VulthorError::MailDir("scanner failed".to_string());
        assert!(err.to_string().contains("MailDir error"));
        assert!(err.to_string().contains("scanner failed"));
    }

    #[test]
    fn vulthor_error_is_convertible_to_box_dyn_error() {
        // Confirms the bridging contract: callers using Box<dyn Error> can
        // accept VulthorError via `?` without bespoke conversions.
        fn returns_box() -> std::result::Result<(), Box<dyn std::error::Error>> {
            Err(VulthorError::MailParser)?;
            Ok(())
        }
        assert!(returns_box().is_err());
    }
}
