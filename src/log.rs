//! Routine log file with size + age caps.
//!
//! Routine logs land in `~/.config/vulthor/logs/vulthor.log`. The writer
//! itself is dumb: every `write` checks whether appending would exceed
//! the configured byte cap; when it would, the current file is renamed
//! to `vulthor.log.1` (existing `.1` → `.2`, `.2` → `.3`), anything
//! beyond `.3` is dropped, and a fresh `vulthor.log` is opened. Age
//! pruning is a one-shot at startup that walks the directory and
//! deletes files modified longer ago than the configured cap.
//!
//! The writer is intentionally tiny — `vu-61a` already routes panics
//! through their own crash log under `~/.cache/vulthor/`, so this
//! module exists only to prevent the eventual routine-log stream from
//! growing without bound. The actual `tracing` / `log` integration
//! will land in a follow-up bead; today this module ships the disk
//! discipline so it's already in place when the framework is wired
//! up.
//!
//! Testable seams: `RotatingLogWriter::open` + `Write` impl drive the
//! rotation; `prune_old_logs` is pure given an explicit `now`; and
//! `log_dir_stats` reports size / oldest-age for the doctor check.

use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Default byte cap per log file before rotation, in MiB. Mirrors the
/// `[log].max_size_mb` config field.
pub const DEFAULT_MAX_SIZE_MB: u64 = 5;

/// Default age cap for any log file, in days. Files older than this
/// are deleted on startup. Mirrors the `[log].max_age_days` field.
pub const DEFAULT_MAX_AGE_DAYS: u64 = 30;

/// Number of rotated copies retained (`vulthor.log.1` .. `.N`). The
/// current `vulthor.log` is in addition to these.
const KEEP_ROTATED: u8 = 3;

/// Base filename for the routine log; rotated copies append `.1` ..
/// `.KEEP_ROTATED`.
const LOG_FILE_NAME: &str = "vulthor.log";

/// `[log]` configuration block. Both fields are overridable from
/// `~/.config/vulthor/config.toml`; defaults are 5 MiB / 30 days.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct LogConfig {
    /// Per-file size cap in MiB. `0` is clamped to `1` byte internally
    /// so a misconfiguration can't deadlock the writer.
    #[serde(default = "LogConfig::default_max_size_mb")]
    pub max_size_mb: u64,
    /// Maximum age (days) of any log file in the directory before it
    /// is deleted at startup.
    #[serde(default = "LogConfig::default_max_age_days")]
    pub max_age_days: u64,
}

impl LogConfig {
    fn default_max_size_mb() -> u64 {
        DEFAULT_MAX_SIZE_MB
    }
    fn default_max_age_days() -> u64 {
        DEFAULT_MAX_AGE_DAYS
    }
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            max_size_mb: Self::default_max_size_mb(),
            max_age_days: Self::default_max_age_days(),
        }
    }
}

/// Default routine-log directory: `$XDG_CONFIG_HOME/vulthor/logs`
/// (typically `~/.config/vulthor/logs`). Falls back to `./vulthor-logs`
/// when no config dir resolves — we always need *somewhere* to write.
pub fn default_log_dir() -> PathBuf {
    dirs::config_dir()
        .map(|d| d.join("vulthor").join("logs"))
        .unwrap_or_else(|| PathBuf::from("./vulthor-logs"))
}

/// Rotating, append-only writer over `vulthor.log` in `dir`.
///
/// Every `write` call checks whether appending `buf` would push the
/// current file past `max_bytes`. When it would (and the file is
/// non-empty — we never rotate an empty file), the writer rolls
/// existing copies (`.2` → `.3`, `.1` → `.2`, `vulthor.log` → `.1`),
/// deletes anything beyond `.KEEP_ROTATED`, reopens a fresh
/// `vulthor.log`, and only then performs the write.
pub struct RotatingLogWriter {
    dir: PathBuf,
    max_bytes: u64,
    current_bytes: u64,
    file: File,
}

impl RotatingLogWriter {
    /// Open (or create) `dir/vulthor.log` for appending. `max_size_mb`
    /// is the rotation threshold; `0` is clamped to `1` byte so the
    /// writer always makes forward progress.
    pub fn open(dir: impl Into<PathBuf>, max_size_mb: u64) -> io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        let path = dir.join(LOG_FILE_NAME);
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let current_bytes = file.metadata()?.len();
        let max_bytes = max_size_mb
            .checked_mul(1024 * 1024)
            .unwrap_or(u64::MAX)
            .max(1);
        Ok(Self {
            dir,
            max_bytes,
            current_bytes,
            file,
        })
    }

    /// Roll existing copies and reopen a fresh `vulthor.log`. Exposed
    /// for tests; production code should rely on the `Write` impl to
    /// trigger this implicitly.
    pub fn rotate(&mut self) -> io::Result<()> {
        let base = self.dir.join(LOG_FILE_NAME);

        // .N-1 → .N for N descending so we don't clobber.
        for n in (2..=KEEP_ROTATED).rev() {
            let src = self.dir.join(format!("{LOG_FILE_NAME}.{}", n - 1));
            let dst = self.dir.join(format!("{LOG_FILE_NAME}.{n}"));
            if src.exists() {
                fs::rename(&src, &dst)?;
            }
        }

        // vulthor.log → vulthor.log.1
        if base.exists() {
            let dst = self.dir.join(format!("{LOG_FILE_NAME}.1"));
            fs::rename(&base, &dst)?;
        }

        // Defensive: drop legacy copies past KEEP_ROTATED in case the
        // cap was lowered between runs.
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(suffix) = name.strip_prefix(&format!("{LOG_FILE_NAME}."))
                && let Ok(n) = suffix.parse::<u8>()
                && n > KEEP_ROTATED
            {
                let _ = fs::remove_file(entry.path());
            }
        }

        self.file = OpenOptions::new().create(true).append(true).open(&base)?;
        self.current_bytes = 0;
        Ok(())
    }
}

impl Write for RotatingLogWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.current_bytes > 0
            && self
                .current_bytes
                .saturating_add(buf.len() as u64)
                > self.max_bytes
        {
            self.rotate()?;
        }
        let n = self.file.write(buf)?;
        self.current_bytes = self.current_bytes.saturating_add(n as u64);
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// Delete every `vulthor.log[.<N>]` in `dir` whose mtime is older than
/// `max_age_days` before `now`. Returns the count of removed files. A
/// missing `dir`, an unresolvable cutoff, or a `max_age_days` of zero
/// are all no-ops — we never want a config typo to wipe the log
/// directory unprompted.
pub fn prune_old_logs(dir: &Path, max_age_days: u64, now: SystemTime) -> io::Result<usize> {
    if !dir.exists() || max_age_days == 0 {
        return Ok(0);
    }
    let Some(cutoff) = now.checked_sub(Duration::from_secs(max_age_days * 86_400)) else {
        return Ok(0);
    };
    let mut count = 0;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !is_log_filename(&name) {
            continue;
        }
        let metadata = entry.metadata()?;
        let modified = metadata.modified().unwrap_or(now);
        if modified < cutoff {
            fs::remove_file(entry.path())?;
            count += 1;
        }
    }
    Ok(count)
}

/// Aggregate stats over the log directory for the doctor check.
#[derive(Debug, Clone, Copy, Default)]
pub struct LogDirStats {
    /// Total bytes across every `vulthor.log[.N]` file.
    pub total_bytes: u64,
    /// Age of the oldest log file relative to the `now` passed in.
    /// `None` when the directory has no log files.
    pub oldest_age: Option<Duration>,
    /// Number of `vulthor.log[.N]` files found.
    pub file_count: usize,
}

/// Walk `dir` and aggregate stats over every `vulthor.log[.<N>]`. A
/// missing directory returns zeroed stats — caller decides whether
/// that's a warning.
pub fn log_dir_stats(dir: &Path, now: SystemTime) -> io::Result<LogDirStats> {
    if !dir.exists() {
        return Ok(LogDirStats::default());
    }
    let mut total_bytes = 0u64;
    let mut oldest: Option<SystemTime> = None;
    let mut file_count = 0;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !is_log_filename(&name) {
            continue;
        }
        let metadata = entry.metadata()?;
        total_bytes = total_bytes.saturating_add(metadata.len());
        file_count += 1;
        let modified = metadata.modified().unwrap_or(now);
        oldest = Some(match oldest {
            Some(t) if t < modified => t,
            _ => modified,
        });
    }
    let oldest_age = oldest.and_then(|t| now.duration_since(t).ok());
    Ok(LogDirStats {
        total_bytes,
        oldest_age,
        file_count,
    })
}

fn is_log_filename(name: &str) -> bool {
    if name == LOG_FILE_NAME {
        return true;
    }
    if let Some(suffix) = name.strip_prefix(&format!("{LOG_FILE_NAME}.")) {
        return suffix.parse::<u32>().is_ok();
    }
    false
}

/// One-shot startup wiring: prune aged-out files, open the rotating
/// writer at [`default_log_dir`], and stamp a `start` line so future
/// crash reports have a reference point. Returns the writer so the
/// caller can hold it for the process lifetime (dropping closes the
/// underlying file).
pub fn init(config: &LogConfig) -> io::Result<RotatingLogWriter> {
    let dir = default_log_dir();
    let _ = prune_old_logs(&dir, config.max_age_days, SystemTime::now());
    let mut writer = RotatingLogWriter::open(&dir, config.max_size_mb)?;
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let _ = writeln!(writer, "[vulthor] start ts={ts}");
    Ok(writer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use tempfile::TempDir;

    fn read_to_string(path: &Path) -> String {
        let mut s = String::new();
        File::open(path).unwrap().read_to_string(&mut s).unwrap();
        s
    }

    #[test]
    fn open_creates_log_dir_and_empty_file() {
        let parent = TempDir::new().unwrap();
        let dir = parent.path().join("nested").join("logs");
        assert!(!dir.exists());

        let _w = RotatingLogWriter::open(&dir, 5).expect("open");

        assert!(dir.is_dir(), "log dir should be created");
        assert!(dir.join("vulthor.log").is_file());
    }

    #[test]
    fn small_writes_do_not_rotate() {
        let dir = TempDir::new().unwrap();
        let mut w = RotatingLogWriter::open(dir.path(), 1).unwrap();
        for _ in 0..10 {
            w.write_all(b"hello\n").unwrap();
        }
        w.flush().unwrap();

        assert!(dir.path().join("vulthor.log").is_file());
        assert!(!dir.path().join("vulthor.log.1").exists());
    }

    #[test]
    fn write_exceeding_cap_rotates_to_dot1() {
        // 1 MiB cap; write enough to force a rotation. Use a tiny cap
        // by going under the MiB path directly — we want the test to
        // run in milliseconds, not allocate megabytes.
        let dir = TempDir::new().unwrap();
        let mut w = RotatingLogWriter::open(dir.path(), 0).unwrap(); // clamped to 1 byte
        w.write_all(b"first").unwrap();
        // current_bytes > 0 now, next write triggers rotation.
        w.write_all(b"second").unwrap();
        w.flush().unwrap();

        let dot1 = dir.path().join("vulthor.log.1");
        let cur = dir.path().join("vulthor.log");
        assert!(dot1.exists(), ".1 should hold the prior content");
        assert!(cur.exists(), "current log should be reopened");

        assert_eq!(read_to_string(&dot1), "first");
        assert_eq!(read_to_string(&cur), "second");
    }

    #[test]
    fn rotation_keeps_only_three_copies() {
        let dir = TempDir::new().unwrap();
        let mut w = RotatingLogWriter::open(dir.path(), 0).unwrap();

        // Each pair of writes triggers exactly one rotation.
        for tag in ["a", "b", "c", "d", "e"] {
            w.write_all(tag.as_bytes()).unwrap();
            // Force a second write so the rotation check fires next round.
            w.write_all(b"x").unwrap();
        }
        w.flush().unwrap();

        assert!(dir.path().join("vulthor.log").exists());
        assert!(dir.path().join("vulthor.log.1").exists());
        assert!(dir.path().join("vulthor.log.2").exists());
        assert!(dir.path().join("vulthor.log.3").exists());
        assert!(
            !dir.path().join("vulthor.log.4").exists(),
            "must not retain a 4th rotated copy"
        );
    }

    #[test]
    fn rotation_preserves_chronological_order() {
        // After three rotations the oldest content should be in .3 and
        // the newest in the live log — drop-order is what protects
        // operators who debug "what happened just before the crash".
        let dir = TempDir::new().unwrap();
        let mut w = RotatingLogWriter::open(dir.path(), 0).unwrap();

        w.write_all(b"one").unwrap();
        w.write_all(b"two").unwrap(); // rotates: .1=one
        w.write_all(b"three").unwrap(); // rotates: .1=two .2=one (and current was 'two', now 'three')
        // Wait — sequence is: write 'one' → cur='one'. write 'two' → rotate (cur='', .1='one'), then write 'two' → cur='two'.
        // write 'three' → rotate (cur='', .1='two', .2='one'), then write 'three' → cur='three'.
        w.flush().unwrap();

        assert_eq!(read_to_string(&dir.path().join("vulthor.log")), "three");
        assert_eq!(read_to_string(&dir.path().join("vulthor.log.1")), "two");
        assert_eq!(read_to_string(&dir.path().join("vulthor.log.2")), "one");
    }

    #[test]
    fn prune_old_logs_removes_files_older_than_cap() {
        // We can't easily backdate file mtimes from stable stdlib, so
        // instead we project `now` into the future and let the existing
        // files look "old".
        let dir = TempDir::new().unwrap();
        // Create a couple of files via the writer to seed real mtimes.
        let mut w = RotatingLogWriter::open(dir.path(), 5).unwrap();
        w.write_all(b"hello\n").unwrap();
        w.flush().unwrap();
        drop(w);

        // Drop a non-log file so we can prove we don't touch it.
        fs::write(dir.path().join("not-a-log.txt"), b"keep me").unwrap();

        let future = SystemTime::now() + Duration::from_secs(31 * 86_400);
        let removed = prune_old_logs(dir.path(), 30, future).unwrap();
        assert_eq!(removed, 1, "exactly the one log file should be pruned");

        assert!(!dir.path().join("vulthor.log").exists());
        assert!(
            dir.path().join("not-a-log.txt").exists(),
            "non-log files must be left alone"
        );
    }

    #[test]
    fn prune_old_logs_noop_when_max_age_zero() {
        // max_age_days = 0 is treated as "don't prune" — refuse to
        // wipe the directory just because the user fat-fingered the
        // config.
        let dir = TempDir::new().unwrap();
        let mut w = RotatingLogWriter::open(dir.path(), 5).unwrap();
        w.write_all(b"hi\n").unwrap();
        drop(w);

        let removed =
            prune_old_logs(dir.path(), 0, SystemTime::now() + Duration::from_secs(10_000_000))
                .unwrap();
        assert_eq!(removed, 0);
        assert!(dir.path().join("vulthor.log").exists());
    }

    #[test]
    fn prune_old_logs_handles_missing_dir() {
        let parent = TempDir::new().unwrap();
        let missing = parent.path().join("never-created");
        let removed = prune_old_logs(&missing, 30, SystemTime::now()).unwrap();
        assert_eq!(removed, 0);
    }

    #[test]
    fn log_dir_stats_aggregates_size_and_age() {
        let dir = TempDir::new().unwrap();
        let mut w = RotatingLogWriter::open(dir.path(), 0).unwrap();
        w.write_all(b"aa").unwrap();
        w.write_all(b"bb").unwrap(); // rotates
        w.flush().unwrap();
        drop(w);

        let future = SystemTime::now() + Duration::from_secs(7 * 86_400);
        let stats = log_dir_stats(dir.path(), future).unwrap();
        assert_eq!(stats.file_count, 2);
        assert_eq!(stats.total_bytes, 4);
        let age = stats.oldest_age.expect("must have an oldest age");
        // Should be at least ~7 days because we projected `now`
        // forward; allow a generous lower bound.
        assert!(
            age.as_secs() >= 6 * 86_400,
            "oldest age should reflect the projected `now`, got {age:?}"
        );
    }

    #[test]
    fn log_dir_stats_empty_for_missing_dir() {
        let parent = TempDir::new().unwrap();
        let missing = parent.path().join("nope");
        let stats = log_dir_stats(&missing, SystemTime::now()).unwrap();
        assert_eq!(stats.file_count, 0);
        assert_eq!(stats.total_bytes, 0);
        assert!(stats.oldest_age.is_none());
    }

    #[test]
    fn is_log_filename_recognizes_rotated_copies() {
        assert!(is_log_filename("vulthor.log"));
        assert!(is_log_filename("vulthor.log.1"));
        assert!(is_log_filename("vulthor.log.10"));
        assert!(!is_log_filename("vulthor.log.bak"));
        assert!(!is_log_filename("other.log"));
        assert!(!is_log_filename("vulthor.log.1.gz"));
    }
}
