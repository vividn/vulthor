//! Panic → crash log pipeline.
//!
//! Installs a panic hook that (a) restores the terminal so the user gets
//! their shell back in a usable state and (b) writes a structured crash
//! log to `~/.cache/vulthor/crash-<unix-ts>.log` containing the panic
//! location, message, and backtrace. The path is also printed to stderr
//! so the user can find it after the alt-screen has been torn down.
//!
//! The hook itself is hard to unit-test (it mutates global panic state
//! and touches crossterm), so the testable seams are split out:
//!
//! * [`CrashInfo`] — plain data describing the panic.
//! * [`write_crash_log`] — pure function `(dir, info) -> PathBuf`. This
//!   is what the tests exercise.
//! * [`install_panic_hook`] — the wiring. Called once from `main`.

use std::backtrace::Backtrace;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crossterm::{
    event::DisableMouseCapture,
    execute,
    terminal::{LeaveAlternateScreen, disable_raw_mode},
};

/// Captured panic data ready to be serialized to a crash log.
#[derive(Debug, Clone)]
pub struct CrashInfo {
    /// `file:line:column` of the panic, if known.
    pub location: Option<String>,
    /// The panic's payload rendered as a string (best-effort).
    pub message: String,
    /// Backtrace at the panic site. May be the literal string `"disabled"`
    /// if `RUST_BACKTRACE` was not set and capture is unavailable.
    pub backtrace: String,
    /// Unix timestamp (seconds) used in the crash file name.
    pub timestamp: u64,
}

/// Write a crash log to `dir/crash-<timestamp>.log`. Creates `dir` if it
/// doesn't already exist. Returns the path that was written.
///
/// Pulled out of the panic hook so it can be unit-tested without
/// actually panicking the test process.
pub fn write_crash_log(dir: &Path, info: &CrashInfo) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let path = dir.join(format!("crash-{}.log", info.timestamp));
    let mut file = fs::File::create(&path)?;

    writeln!(file, "Vulthor crash log")?;
    writeln!(file, "Timestamp: {}", info.timestamp)?;
    writeln!(
        file,
        "Location: {}",
        info.location.as_deref().unwrap_or("<unknown>")
    )?;
    writeln!(file, "Message: {}", info.message)?;
    writeln!(file)?;
    writeln!(file, "Backtrace:")?;
    write!(file, "{}", info.backtrace)?;
    if !info.backtrace.ends_with('\n') {
        writeln!(file)?;
    }
    file.flush()?;
    Ok(path)
}

/// Default crash log directory: `$XDG_CACHE_HOME/vulthor` (or
/// `~/.cache/vulthor` on platforms without one). Falls back to `/tmp` if
/// neither is available, so we always have *somewhere* to write.
pub fn default_crash_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("vulthor")
}

/// Install a panic hook for the lifetime of the process. On panic:
///
/// 1. Restore the terminal (drop alt-screen, mouse capture, raw mode)
///    so the user's shell is usable again.
/// 2. Write a crash log to [`default_crash_dir`].
/// 3. Print the crash log path to stderr.
/// 4. Defer to the previously-installed hook for the usual stderr panic
///    message.
///
/// Safe to call exactly once near the top of `main`.
pub fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);

        let info = CrashInfo {
            location: panic_info
                .location()
                .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column())),
            message: panic_message(panic_info),
            backtrace: Backtrace::force_capture().to_string(),
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        };

        let dir = default_crash_dir();
        match write_crash_log(&dir, &info) {
            Ok(path) => {
                eprintln!("Vulthor panicked. Crash log written to {}", path.display());
            }
            Err(e) => {
                eprintln!(
                    "Vulthor panicked. Failed to write crash log to {}: {}",
                    dir.display(),
                    e
                );
            }
        }

        default_hook(panic_info);
    }));
}

/// Best-effort extraction of the panic message string. `PanicHookInfo`
/// stores the payload as `&dyn Any`; we try `&str` and `String`, then
/// fall back to a placeholder.
fn panic_message(panic_info: &std::panic::PanicHookInfo<'_>) -> String {
    let payload = panic_info.payload();
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_info(ts: u64) -> CrashInfo {
        CrashInfo {
            location: Some("src/foo.rs:42:7".to_string()),
            message: "something went wrong".to_string(),
            backtrace: "0: foo\n1: bar\n".to_string(),
            timestamp: ts,
        }
    }

    #[test]
    fn write_crash_log_creates_file_at_expected_path() {
        let dir = TempDir::new().unwrap();
        let info = sample_info(1_234_567_890);

        let path = write_crash_log(dir.path(), &info).expect("write succeeds");

        assert_eq!(path, dir.path().join("crash-1234567890.log"));
        assert!(path.exists(), "crash log file should exist on disk");
    }

    #[test]
    fn write_crash_log_creates_missing_parent_directory() {
        // The `vulthor` subdir doesn't exist yet; the writer must
        // mkdir -p so first-ever panics aren't lost.
        let parent = TempDir::new().unwrap();
        let nested = parent.path().join("nested").join("vulthor");
        assert!(!nested.exists());

        let info = sample_info(42);
        let path = write_crash_log(&nested, &info).expect("write succeeds");

        assert!(nested.is_dir(), "nested dir should be created");
        assert!(path.starts_with(&nested));
    }

    #[test]
    fn write_crash_log_preserves_location_and_message() {
        let dir = TempDir::new().unwrap();
        let info = sample_info(1);
        let path = write_crash_log(dir.path(), &info).unwrap();

        let contents = fs::read_to_string(&path).unwrap();
        assert!(
            contents.contains("src/foo.rs:42:7"),
            "log should record panic location, got: {contents}"
        );
        assert!(
            contents.contains("something went wrong"),
            "log should record panic message, got: {contents}"
        );
    }

    #[test]
    fn write_crash_log_includes_backtrace() {
        let dir = TempDir::new().unwrap();
        let info = sample_info(2);
        let path = write_crash_log(dir.path(), &info).unwrap();

        let contents = fs::read_to_string(&path).unwrap();
        assert!(
            contents.contains("Backtrace:"),
            "log should have a Backtrace section"
        );
        assert!(
            contents.contains("0: foo") && contents.contains("1: bar"),
            "log should preserve backtrace frames, got: {contents}"
        );
    }

    #[test]
    fn write_crash_log_handles_unknown_location() {
        let dir = TempDir::new().unwrap();
        let info = CrashInfo {
            location: None,
            message: "no location".to_string(),
            backtrace: "(disabled)".to_string(),
            timestamp: 99,
        };

        let path = write_crash_log(dir.path(), &info).unwrap();
        let contents = fs::read_to_string(&path).unwrap();
        assert!(
            contents.contains("<unknown>"),
            "missing location should render as <unknown>, got: {contents}"
        );
    }

    #[test]
    fn write_crash_log_timestamp_drives_filename() {
        // Two different timestamps must yield two distinct files in the
        // same dir — otherwise back-to-back panics would clobber the
        // earlier log.
        let dir = TempDir::new().unwrap();
        let p1 = write_crash_log(dir.path(), &sample_info(100)).unwrap();
        let p2 = write_crash_log(dir.path(), &sample_info(200)).unwrap();
        assert_ne!(p1, p2);
        assert!(p1.exists() && p2.exists());
    }
}
