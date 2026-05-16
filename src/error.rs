// Unified error type for the thiserror migration epic (vu-ri8).
//
// Used across config, email, maildir, web, and main. `VulthorError` is
// Send + Sync so it can flow across tokio task boundaries (e.g. the web
// server). It implements `std::error::Error`, so the `?` operator coerces
// it to `Box<dyn Error>` at any boundary that still wants a boxed error.

use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum VulthorError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Directory walk error: {0}")]
    WalkDir(#[from] walkdir::Error),

    #[error("Failed to parse TOML config: {0}")]
    ParseToml(#[from] toml::de::Error),

    #[error("Config file not found: {0}")]
    ConfigNotFound(PathBuf),

    #[error("Failed to parse email")]
    MailParser,

    #[error("Invalid folder path")]
    #[allow(dead_code)]
    InvalidFolderPath,

    #[error("MailDir path does not exist: {0}")]
    MaildirPathNotFound(PathBuf),

    #[error("MailDir path is not a directory: {0}")]
    MaildirPathNotDirectory(PathBuf),
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
    fn maildir_path_not_found_display_includes_path() {
        let err = VulthorError::MaildirPathNotFound(PathBuf::from("/no/such/mail"));
        let msg = err.to_string();
        assert!(msg.contains("MailDir path does not exist"));
        assert!(msg.contains("/no/such/mail"));
    }

    #[test]
    fn maildir_path_not_directory_display_includes_path() {
        let err = VulthorError::MaildirPathNotDirectory(PathBuf::from("/tmp/some-file"));
        let msg = err.to_string();
        assert!(msg.contains("MailDir path is not a directory"));
        assert!(msg.contains("/tmp/some-file"));
    }

    #[test]
    fn walkdir_error_converts_via_from() {
        // walkdir::Error has no public constructor; build one by walking a
        // non-existent path and unwrapping the iterator's error.
        let err = walkdir::WalkDir::new("/definitely/does/not/exist/for/walkdir")
            .into_iter()
            .next()
            .expect("walkdir should yield an error entry")
            .unwrap_err();
        let err: VulthorError = err.into();
        assert!(matches!(err, VulthorError::WalkDir(_)));
        assert!(err.to_string().contains("Directory walk error"));
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

    #[test]
    fn vulthor_error_is_send_and_sync() {
        // Required so the async web server (which crosses thread boundaries
        // under tokio) can return VulthorError without bespoke boxing.
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<VulthorError>();
    }
}
