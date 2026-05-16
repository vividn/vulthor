//! Chromeless HTML viewer (Phase 3.b).
//!
//! Owns the lifecycle of a child browser pointed at the embedded web
//! server. The TUI binds `v` to a toggle: first press launches the
//! first available browser in chromeless mode; second press signals
//! the child to exit.
//!
//! Browser detection prefers Chromium-class browsers (`--app=<URL>`)
//! over Firefox (`--kiosk <URL>`), falling back to `xdg-open <URL>`.
//! Detection is split from launch so tests can stub `PATH` lookups.

use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// A browser binary we know how to launch in chromeless mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Browser {
    /// `chromium --app=<URL>`
    Chromium,
    /// `google-chrome --app=<URL>`
    Chrome,
    /// `firefox --kiosk <URL>`
    Firefox,
    /// `xdg-open <URL>` — last-resort fallback, not chromeless.
    XdgOpen,
}

impl Browser {
    /// Binary name as it appears on `PATH`.
    pub fn binary(&self) -> &'static str {
        match self {
            Browser::Chromium => "chromium",
            Browser::Chrome => "google-chrome",
            Browser::Firefox => "firefox",
            Browser::XdgOpen => "xdg-open",
        }
    }

    /// Argv tail for `binary <args>` given a target URL.
    pub fn args_for(&self, url: &str) -> Vec<String> {
        match self {
            Browser::Chromium | Browser::Chrome => vec![format!("--app={}", url)],
            Browser::Firefox => vec!["--kiosk".to_string(), url.to_string()],
            Browser::XdgOpen => vec![url.to_string()],
        }
    }
}

/// Preference order: chromeless-capable first, `xdg-open` last.
pub const DETECTION_ORDER: &[Browser] = &[
    Browser::Chromium,
    Browser::Chrome,
    Browser::Firefox,
    Browser::XdgOpen,
];

/// Pick the first browser the caller's `exists` predicate accepts.
/// Split from `binary_on_path` so tests can stub `PATH` without
/// touching the process environment.
pub fn detect_browser<F: Fn(&str) -> bool>(exists: F) -> Option<Browser> {
    DETECTION_ORDER.iter().copied().find(|b| exists(b.binary()))
}

/// True when `name` resolves to an executable entry on `PATH`.
pub fn binary_on_path(name: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| dir.join(name).is_file())
}

/// Spawn the browser with its stdio redirected to `/dev/null` so the
/// child never writes onto the terminal the TUI owns.
pub fn launch(browser: Browser, url: &str) -> std::io::Result<Child> {
    Command::new(browser.binary())
        .args(browser.args_for(url))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

/// Ask `child` to exit gracefully, escalate to `kill -9` if it does
/// not within `timeout`. Sends SIGTERM via `kill(1)` to avoid pulling
/// in `libc` for one signal. Always reaps the child so it does not
/// linger as a zombie.
pub fn terminate(child: &mut Child, timeout: Duration) -> std::io::Result<()> {
    let pid = child.id();
    let _ = Command::new("kill")
        .arg(pid.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    let deadline = Instant::now() + timeout;
    let step = Duration::from_millis(25);
    loop {
        if let Some(_status) = child.try_wait()? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(step);
    }

    child.kill()?;
    let _ = child.wait();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_prefers_chromium_when_all_present() {
        let pick = detect_browser(|_| true);
        assert_eq!(pick, Some(Browser::Chromium));
    }

    #[test]
    fn detect_falls_through_to_firefox() {
        let pick = detect_browser(|name| name == "firefox" || name == "xdg-open");
        assert_eq!(pick, Some(Browser::Firefox));
    }

    #[test]
    fn detect_falls_through_to_xdg_open() {
        let pick = detect_browser(|name| name == "xdg-open");
        assert_eq!(pick, Some(Browser::XdgOpen));
    }

    #[test]
    fn detect_returns_none_when_nothing_on_path() {
        let pick = detect_browser(|_| false);
        assert_eq!(pick, None);
    }

    #[test]
    fn detect_prefers_chrome_over_firefox_when_chromium_missing() {
        let pick = detect_browser(|name| matches!(name, "google-chrome" | "firefox" | "xdg-open"));
        assert_eq!(pick, Some(Browser::Chrome));
    }

    #[test]
    fn args_for_chromium_uses_app_flag() {
        assert_eq!(
            Browser::Chromium.args_for("http://127.0.0.1:8080"),
            vec!["--app=http://127.0.0.1:8080".to_string()],
        );
    }

    #[test]
    fn args_for_firefox_uses_kiosk_with_separate_url() {
        assert_eq!(
            Browser::Firefox.args_for("http://127.0.0.1:8080"),
            vec!["--kiosk".to_string(), "http://127.0.0.1:8080".to_string()],
        );
    }

    #[test]
    fn args_for_xdg_open_is_just_the_url() {
        assert_eq!(
            Browser::XdgOpen.args_for("http://127.0.0.1:9090"),
            vec!["http://127.0.0.1:9090".to_string()],
        );
    }

    /// `terminate` must reap a long-running child within the timeout.
    /// We spawn `sleep 60` as a stand-in for a real browser process,
    /// then call terminate and check the second `try_wait` reports
    /// the child as exited. SIGTERM is enough to drop `sleep`, so
    /// the SIGKILL escalation branch does not fire here — that
    /// branch is exercised by `terminate_escalates_to_kill_when_signal_ignored`.
    #[test]
    fn terminate_reaps_child_within_timeout() {
        let mut child = Command::new("sleep")
            .arg("60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("sleep(1) must exist on the test host");

        terminate(&mut child, Duration::from_secs(2)).expect("terminate succeeds");
        // After a successful terminate the child must already be reaped:
        // a follow-up try_wait reports `Some(_)` immediately and never
        // blocks waiting on the process table.
        assert!(matches!(child.try_wait(), Ok(Some(_)) | Err(_)));
    }

    /// SIGKILL escalation kicks in when the child traps and ignores
    /// SIGTERM. `sh -c 'trap "" TERM; sleep 60'` installs a no-op
    /// handler for SIGTERM, so the timeout branch must run and the
    /// child must still die.
    #[test]
    fn terminate_escalates_to_kill_when_signal_ignored() {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("trap '' TERM; sleep 60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("sh(1) must exist on the test host");

        terminate(&mut child, Duration::from_millis(150)).expect("terminate succeeds");
        assert!(matches!(child.try_wait(), Ok(Some(_)) | Err(_)));
    }
}
