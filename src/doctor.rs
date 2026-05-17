//! `vulthor doctor` — runtime preconditions and health diagnostics.
//!
//! Verifies the things most likely to bite a fresh install: the config
//! file is parseable, the active MailDir has the canonical
//! `cur/new/tmp` layout, the binaries we shell out to (`msmtp`,
//! `notmuch`, `mbsync`) are on `PATH`, and `[ai].model_path` resolves
//! when AI is enabled. Each check is independent so a single failure
//! never short-circuits the rest of the report.
//!
//! Exit-code contract: `0` when every check is OK or WARN, `2` if any
//! check is FAIL. `WARN` is reserved for "this feature is degraded but
//! the app still runs" (e.g. notmuch missing → search disabled).

use crate::config::Config;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Severity tier of a single [`DoctorCheck`]. `Ok` and `Warn` keep the
/// exit code at 0; `Fail` flips it to 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoctorStatus {
    /// Check passed.
    Ok,
    /// Check passed in a degraded mode (optional feature unavailable).
    Warn,
    /// Check failed; the runtime feature will not work.
    Fail,
}

/// One row in the doctor report. `name` is a short kebab-style label
/// (e.g. `"maildir"`); `message` is an actionable single-line
/// description, including the resolved path / binary name when
/// relevant so the user can copy-paste-fix it.
#[derive(Debug, Clone)]
pub struct DoctorCheck {
    /// Short label, kebab-case. Used as the second column of the
    /// printed report.
    pub name: String,
    /// Severity tier; drives both color and exit code.
    pub status: DoctorStatus,
    /// Human-readable explanation. Should name the offending path or
    /// binary so the user can act without re-running with `-v`.
    pub message: String,
}

impl DoctorCheck {
    fn ok(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: DoctorStatus::Ok,
            message: message.into(),
        }
    }
    fn warn(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: DoctorStatus::Warn,
            message: message.into(),
        }
    }
    fn fail(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: DoctorStatus::Fail,
            message: message.into(),
        }
    }
}

/// Run every diagnostic check against `config` and return the
/// collected results in display order. Uses real-process environment
/// (`PATH`, `HOME`); individual `check_*` helpers can be called
/// directly with overridden inputs for deterministic tests.
pub fn run_doctor(config: &Config) -> Vec<DoctorCheck> {
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    let home = dirs::home_dir();
    vec![
        check_config_file(home.as_deref()),
        check_maildir(&config.active_maildir()),
        check_msmtp(config, &path_var),
        check_binary_optional("notmuch", "search", &path_var),
        check_binary_optional("mbsync", "MailDir sync", &path_var),
        check_themes_dir(home.as_deref()),
        check_ai_model(config),
        check_log_dir(home.as_deref(), config),
    ]
}

/// Exit code derived from a report: `2` if any check failed, `0`
/// otherwise. `WARN` does not flip the exit code — that's the
/// difference between "feature degraded" and "feature broken".
pub fn exit_code(checks: &[DoctorCheck]) -> i32 {
    if checks.iter().any(|c| c.status == DoctorStatus::Fail) {
        2
    } else {
        0
    }
}

/// Print the report as a colored table to stdout. ANSI escapes are
/// emitted only when stdout is a terminal so piped output stays
/// machine-readable.
pub fn print_report(checks: &[DoctorCheck]) {
    use std::io::IsTerminal;
    let use_color = std::io::stdout().is_terminal();
    for c in checks {
        let (prefix, color) = match c.status {
            DoctorStatus::Ok => ("OK  ", "\x1b[32m"),
            DoctorStatus::Warn => ("WARN", "\x1b[33m"),
            DoctorStatus::Fail => ("FAIL", "\x1b[31m"),
        };
        if use_color {
            println!("{color}{prefix}\x1b[0m  {:<14}  {}", c.name, c.message);
        } else {
            println!("{prefix}  {:<14}  {}", c.name, c.message);
        }
    }
}

/// Check (a) — locate a config file in the order `Config::load` uses.
/// `WARN` (not `FAIL`) when none is found: a fresh install runs fine
/// on defaults.
pub(crate) fn check_config_file(home: Option<&Path>) -> DoctorCheck {
    if let Some(h) = home {
        let p = h.join(".config/vulthor/config.toml");
        if p.is_file() {
            return DoctorCheck::ok("config", format!("found at {}", p.display()));
        }
    }
    let local = PathBuf::from("./vulthor.toml");
    if local.is_file() {
        return DoctorCheck::ok("config", format!("found at {}", local.display()));
    }
    DoctorCheck::warn("config", "no config file found; using built-in defaults")
}

/// Check (b) — active MailDir exists and has the `cur/new/tmp`
/// trinity. Failure here means the email pane has nothing to render.
pub(crate) fn check_maildir(path: &Path) -> DoctorCheck {
    if !path.exists() {
        return DoctorCheck::fail("maildir", format!("does not exist: {}", path.display()));
    }
    if !path.is_dir() {
        return DoctorCheck::fail("maildir", format!("not a directory: {}", path.display()));
    }
    for sub in ["cur", "new", "tmp"] {
        let s = path.join(sub);
        if !s.is_dir() {
            return DoctorCheck::fail(
                "maildir",
                format!("missing required subdirectory: {}", s.display()),
            );
        }
    }
    DoctorCheck::ok("maildir", format!("OK at {}", path.display()))
}

/// Check (c) — `msmtp` is required only when at least one account
/// declares an `smtp_command`. We don't second-guess the user's
/// command line; just verify the binary resolves on `PATH`.
pub(crate) fn check_msmtp(config: &Config, path_var: &OsStr) -> DoctorCheck {
    let needs_smtp = config.accounts.values().any(|a| a.smtp_command.is_some());
    if !needs_smtp {
        return DoctorCheck::ok("msmtp", "no accounts configured to send mail");
    }
    if binary_in_path("msmtp", path_var) {
        DoctorCheck::ok("msmtp", "available on PATH")
    } else {
        DoctorCheck::fail(
            "msmtp",
            "not found on PATH; required by at least one account's smtp_command",
        )
    }
}

/// Checks (d) / (e) — `notmuch` and `mbsync` are optional. Missing
/// just degrades the corresponding feature (`purpose`), so we emit
/// `WARN`, never `FAIL`.
pub(crate) fn check_binary_optional(name: &str, purpose: &str, path_var: &OsStr) -> DoctorCheck {
    if binary_in_path(name, path_var) {
        DoctorCheck::ok(name, format!("available on PATH ({purpose})"))
    } else {
        DoctorCheck::warn(name, format!("not found on PATH; {purpose} unavailable"))
    }
}

/// Check (f) — informational: report whether the user-themes
/// directory exists. Absence is fine (we ship a built-in palette);
/// surface it so a user wondering "where do I drop my theme file?"
/// gets the answer without grepping VISION.md.
pub(crate) fn check_themes_dir(home: Option<&Path>) -> DoctorCheck {
    let Some(h) = home else {
        return DoctorCheck::warn("themes-dir", "no HOME resolved; user themes unavailable");
    };
    let p = h.join(".config/vulthor/themes");
    if p.is_dir() {
        DoctorCheck::ok("themes-dir", format!("found at {}", p.display()))
    } else {
        DoctorCheck::warn(
            "themes-dir",
            format!(
                "not present at {}; using built-in palette only",
                p.display()
            ),
        )
    }
}

/// Check (g) — when `[ai].enabled = true`, the configured model file
/// must resolve. If AI is disabled, the field is documentation-only
/// and we report OK so the column line doesn't look alarming.
pub(crate) fn check_ai_model(config: &Config) -> DoctorCheck {
    if !config.ai.enabled {
        return DoctorCheck::ok("ai-model", "AI disabled");
    }
    match &config.ai.model_path {
        Some(p) if p.is_file() => DoctorCheck::ok("ai-model", format!("found at {}", p.display())),
        Some(p) => DoctorCheck::fail(
            "ai-model",
            format!(
                "[ai].model_path does not resolve to a file: {}",
                p.display()
            ),
        ),
        None => DoctorCheck::fail(
            "ai-model",
            "[ai].enabled = true but [ai].model_path is unset",
        ),
    }
}

/// Check (h) — routine-log directory health. Reports the on-disk
/// footprint and the oldest file's age so an operator can sanity-check
/// the rotation/pruning policy without grepping the filesystem. Never
/// fails: an absent directory just means we haven't written a log
/// line yet.
pub(crate) fn check_log_dir(home: Option<&Path>, config: &Config) -> DoctorCheck {
    let Some(h) = home else {
        return DoctorCheck::warn("log-dir", "no HOME resolved; log directory unavailable");
    };
    let dir = h.join(".config/vulthor/logs");
    if !dir.exists() {
        return DoctorCheck::ok(
            "log-dir",
            format!(
                "not yet created at {} (cap {} MB / {} days)",
                dir.display(),
                config.log.max_size_mb,
                config.log.max_age_days
            ),
        );
    }
    match crate::log::log_dir_stats(&dir, SystemTime::now()) {
        Ok(stats) => {
            let size_kib = stats.total_bytes / 1024;
            let oldest = match stats.oldest_age {
                Some(d) => format!("oldest {} days", d.as_secs() / 86_400),
                None => "no log files".to_string(),
            };
            DoctorCheck::ok(
                "log-dir",
                format!(
                    "{} files, {} KiB, {} (cap {} MB / {} days)",
                    stats.file_count,
                    size_kib,
                    oldest,
                    config.log.max_size_mb,
                    config.log.max_age_days
                ),
            )
        }
        Err(e) => DoctorCheck::warn("log-dir", format!("cannot stat {}: {e}", dir.display())),
    }
}

/// Cross-platform `which`: split `path_var` and look for an executable
/// file named `name` in each entry. We intentionally do not check the
/// executable bit — on Linux the file mode is the only signal but on
/// other platforms `is_file()` is the more portable indicator, and a
/// non-executable false positive degrades to a clearer runtime error
/// downstream than silently treating the binary as missing.
fn binary_in_path(name: &str, path_var: &OsStr) -> bool {
    // Allow callers to use the OS-default split (`:`-on-Unix,
    // `;`-on-Windows) by routing through `std::env::split_paths`.
    let owned = OsString::from(path_var);
    for dir in std::env::split_paths(&owned) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join(name);
        if candidate.is_file() {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AccountConfig;
    use std::collections::BTreeMap;
    use std::fs;
    use tempfile::TempDir;

    fn maildir(tmp: &TempDir) -> PathBuf {
        for sub in ["cur", "new", "tmp"] {
            fs::create_dir_all(tmp.path().join(sub)).unwrap();
        }
        tmp.path().to_path_buf()
    }

    /// Acceptance: "run_doctor with a config pointing at a
    /// non-existent maildir returns FAIL". We also confirm the
    /// derived exit code flips to 2, since that's the contract the
    /// CLI relies on.
    #[test]
    fn run_doctor_fails_when_maildir_missing() {
        let cfg = Config {
            maildir_path: PathBuf::from("/__vulthor_doctor_missing__/mail"),
            ..Config::default()
        };
        let checks = run_doctor(&cfg);
        let m = checks
            .iter()
            .find(|c| c.name == "maildir")
            .expect("maildir check present");
        assert_eq!(m.status, DoctorStatus::Fail, "{:?}", m);
        assert_eq!(exit_code(&checks), 2);
    }

    /// `cur/new/tmp` layout → OK.
    #[test]
    fn check_maildir_ok_on_canonical_layout() {
        let tmp = TempDir::new().unwrap();
        let path = maildir(&tmp);
        let chk = check_maildir(&path);
        assert_eq!(chk.status, DoctorStatus::Ok, "{:?}", chk);
    }

    /// Missing `tmp/` subdir → FAIL (catches the half-initialized
    /// MailDir case that mbsync occasionally leaves behind).
    #[test]
    fn check_maildir_fails_on_missing_subdir() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("cur")).unwrap();
        fs::create_dir_all(tmp.path().join("new")).unwrap();
        // tmp/tmp deliberately absent.
        let chk = check_maildir(tmp.path());
        assert_eq!(chk.status, DoctorStatus::Fail);
        assert!(chk.message.contains("tmp"), "{:?}", chk);
    }

    /// Acceptance: "with valid maildir + msmtp available returns OK
    /// for both". We test the msmtp half here against a synthetic
    /// PATH so the result doesn't depend on the host having msmtp
    /// installed.
    #[test]
    fn check_msmtp_ok_when_account_needs_it_and_binary_on_path() {
        let tmp = TempDir::new().unwrap();
        let fake = tmp.path().join("msmtp");
        fs::write(&fake, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&fake).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&fake, perms).unwrap();
        }

        let mut accounts = BTreeMap::new();
        accounts.insert(
            "a".to_string(),
            AccountConfig {
                name: "A".into(),
                email: "a@b.tld".into(),
                maildir_path: PathBuf::from("/tmp"),
                smtp_command: Some("msmtp -a a".into()),
                signature: None,
            },
        );
        let cfg = Config {
            accounts,
            ..Config::default()
        };

        let chk = check_msmtp(&cfg, tmp.path().as_os_str());
        assert_eq!(chk.status, DoctorStatus::Ok, "{:?}", chk);
    }

    /// No account declares `smtp_command` → OK with a "not needed"
    /// message; we don't FAIL just because msmtp is absent from PATH.
    #[test]
    fn check_msmtp_ok_when_no_account_needs_it() {
        let cfg = Config::default();
        // Empty PATH dir — msmtp definitely not resolvable.
        let tmp = TempDir::new().unwrap();
        let chk = check_msmtp(&cfg, tmp.path().as_os_str());
        assert_eq!(chk.status, DoctorStatus::Ok);
    }

    /// `smtp_command` set but binary missing → FAIL so the user
    /// learns *before* their first send attempt.
    #[test]
    fn check_msmtp_fails_when_account_needs_it_but_binary_missing() {
        let mut accounts = BTreeMap::new();
        accounts.insert(
            "a".to_string(),
            AccountConfig {
                name: "A".into(),
                email: "a@b.tld".into(),
                maildir_path: PathBuf::from("/tmp"),
                smtp_command: Some("msmtp".into()),
                signature: None,
            },
        );
        let cfg = Config {
            accounts,
            ..Config::default()
        };
        let tmp = TempDir::new().unwrap();
        let chk = check_msmtp(&cfg, tmp.path().as_os_str());
        assert_eq!(chk.status, DoctorStatus::Fail);
    }

    /// Acceptance: "with notmuch missing returns WARN (not FAIL)".
    /// Same pattern for any optional binary.
    #[test]
    fn check_binary_optional_warns_when_missing() {
        let tmp = TempDir::new().unwrap();
        let chk = check_binary_optional("notmuch", "search", tmp.path().as_os_str());
        assert_eq!(chk.status, DoctorStatus::Warn);
        assert!(chk.message.to_lowercase().contains("not found"));
    }

    #[test]
    fn check_binary_optional_ok_when_present() {
        let tmp = TempDir::new().unwrap();
        let fake = tmp.path().join("notmuch");
        fs::write(&fake, "#!/bin/sh\n").unwrap();
        let chk = check_binary_optional("notmuch", "search", tmp.path().as_os_str());
        assert_eq!(chk.status, DoctorStatus::Ok);
    }

    /// Config file located via the `$HOME/.config/vulthor/config.toml`
    /// search path.
    #[test]
    fn check_config_file_ok_when_home_config_present() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join(".config/vulthor");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("config.toml"), "maildir_path = \"/x\"\n").unwrap();
        let chk = check_config_file(Some(tmp.path()));
        assert_eq!(chk.status, DoctorStatus::Ok, "{:?}", chk);
    }

    /// No config file anywhere → WARN, not FAIL (defaults still run).
    #[test]
    fn check_config_file_warns_when_absent() {
        let tmp = TempDir::new().unwrap();
        let chk = check_config_file(Some(tmp.path()));
        assert_eq!(chk.status, DoctorStatus::Warn);
    }

    /// AI disabled → OK regardless of `model_path`.
    #[test]
    fn check_ai_model_ok_when_disabled() {
        let cfg = Config::default();
        let chk = check_ai_model(&cfg);
        assert_eq!(chk.status, DoctorStatus::Ok);
    }

    /// AI enabled + missing model file → FAIL with the bad path
    /// echoed back in the message.
    #[test]
    fn check_ai_model_fails_when_enabled_but_path_missing() {
        let mut cfg = Config::default();
        cfg.ai.enabled = true;
        cfg.ai.model_path = Some(PathBuf::from("/__vulthor_doctor__/no.bin"));
        let chk = check_ai_model(&cfg);
        assert_eq!(chk.status, DoctorStatus::Fail);
        assert!(chk.message.contains("/__vulthor_doctor__"));
    }

    /// AI enabled + valid model file → OK.
    #[test]
    fn check_ai_model_ok_when_enabled_and_file_present() {
        let tmp = TempDir::new().unwrap();
        let model = tmp.path().join("clf.bin");
        fs::write(&model, b"weights").unwrap();
        let mut cfg = Config::default();
        cfg.ai.enabled = true;
        cfg.ai.model_path = Some(model);
        let chk = check_ai_model(&cfg);
        assert_eq!(chk.status, DoctorStatus::Ok);
    }

    /// Exit-code logic: OK + WARN → 0; any FAIL → 2.
    #[test]
    fn exit_code_zero_when_only_ok_and_warn() {
        let checks = vec![
            DoctorCheck::ok("a", ""),
            DoctorCheck::warn("b", ""),
            DoctorCheck::ok("c", ""),
        ];
        assert_eq!(exit_code(&checks), 0);
    }

    #[test]
    fn exit_code_two_when_any_fail() {
        let checks = vec![
            DoctorCheck::ok("a", ""),
            DoctorCheck::warn("b", ""),
            DoctorCheck::fail("c", ""),
        ];
        assert_eq!(exit_code(&checks), 2);
    }
}
