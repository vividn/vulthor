//! Phase 3 integration tests (vu-i7d).
//!
//! End-to-end coverage for the three Phase 3 features once 3.a-3.c
//! landed:
//!
//!   * 3.a — notmuch search via `/` (`SearchComponent`, the
//!     `apply_search_execute` shell-out, and the virtual-folder
//!     rendering in `EmailStore::search_results`).
//!   * 3.b — chromeless HTML viewer toggled by `v`
//!     (`html_viewer::{detect_browser, launch, terminate}` and the
//!     `AppRoot::apply_toggle_html_viewer` lifecycle).
//!   * 3.c — PWA manifest + service worker advertised by the embedded
//!     web server (`serve_manifest`, `serve_service_worker`, and the
//!     `<link rel="manifest">` in the rendered HTML shells).
//!
//! Per-test scope (mirrors vu-i7d acceptance):
//!
//! 1. `search_roundtrip_navigates_finds_two_messages_and_returns_on_esc`
//!    — `/` -> typed query -> Enter produces a virtual folder of two
//!    matching messages; Esc clears it and restores the prior view.
//! 2. `search_with_no_notmuch_surfaces_status_and_does_not_crash`
//!    — PATH-stripped run of `/` reports "notmuch not found" without
//!    panicking and without opening the modal.
//! 3. `html_viewer_toggle_spawns_and_terminates_child` — `v` spawns
//!    a child via a PATH-overridden stub `chromium`; second `v`
//!    terminates it.
//! 4. `pwa_manifest_sw_and_root_html_link_install_hooks` — the
//!    `/manifest.json`, `/sw.js`, and `/` (welcome) handlers each
//!    serve the contract that Chrome/Edge need for the install
//!    prompt.
//!
//! Stub binaries (`notmuch`, `chromium`) are written into a per-test
//! temp dir and exposed via `$PATH` prepending. The crate-wide
//! `test_fixtures::path_lock` serializes every PATH mutation so the
//! existing root.rs PATH-touching tests and the integration tests
//! here cannot trample each other when `cargo test` runs in parallel.

#![cfg(test)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, atomic::AtomicU8};
use std::time::{Duration, Instant};

use axum::body::to_bytes;
use axum::extract::State;
use axum::http::StatusCode;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use tempfile::TempDir;

use crate::components::AppRoot;
use crate::email::{Email, EmailStore, Folder};
use crate::layout::{ActivePane, View};
use crate::maildir::MaildirScanner;
use crate::test_fixtures::path_lock;
use crate::web::{WebState, serve_email, serve_manifest, serve_service_worker};

// ---- fixture helpers --------------------------------------------------

/// Two RFC822 messages used by the search round-trip test. Headers
/// are rich enough that `parse_headers_only` lands a real subject /
/// from, so the virtual folder shows non-blank rows.
const MSG_ONE: &str = "From: Alice <alice@example.com>\r\n\
To: Tester <tester@example.com>\r\n\
Subject: First match\r\n\
Message-ID: <m1@example.com>\r\n\
Date: Sat, 16 May 2026 12:00:00 +0000\r\n\
\r\n\
hit-1\r\n";

const MSG_TWO: &str = "From: Bob <bob@example.com>\r\n\
To: Tester <tester@example.com>\r\n\
Subject: Second match\r\n\
Message-ID: <m2@example.com>\r\n\
Date: Sat, 16 May 2026 13:00:00 +0000\r\n\
\r\n\
hit-2\r\n";

/// Build a single-INBOX `AppRoot` seeded with the two `MSG_*`
/// fixtures so the search round-trip starts from a familiar
/// "user is reading mail" state.
fn make_root_with_two_emails(root_path: PathBuf) -> (AppRoot, PathBuf, PathBuf) {
    let inbox_cur = root_path.join("INBOX").join("cur");
    std::fs::create_dir_all(&inbox_cur).unwrap();
    let p1 = inbox_cur.join("msg-1.eml");
    let p2 = inbox_cur.join("msg-2.eml");
    std::fs::write(&p1, MSG_ONE).unwrap();
    std::fs::write(&p2, MSG_TWO).unwrap();

    let mut store = EmailStore::new(root_path.clone());
    let mut inbox = Folder::new("INBOX".into(), root_path.join("INBOX"));
    for p in [&p1, &p2] {
        let mut e = Email::new(p.clone());
        e.parse_headers_only().unwrap();
        inbox.add_email(e);
    }
    inbox.is_loaded = true;
    store.root_folder.add_subfolder(inbox);
    store.enter_folder_by_path(&[0]);
    store.select_email(0);

    let scanner = MaildirScanner::new(root_path);
    let root = AppRoot::new(Arc::new(Mutex::new(store)), scanner);
    (root, p1, p2)
}

/// Drop a stub `notmuch` shell script into `dir` that:
///   * exits 0 on `notmuch --version` so `notmuch_available()` says yes,
///   * prints `paths` (newline-separated) for any other invocation,
///     which is what `apply_search_execute` parses via
///     `parse_notmuch_files_output`.
fn write_stub_notmuch(dir: &Path, paths: &[&Path]) -> PathBuf {
    let script = dir.join("notmuch");
    let mut body = String::from("#!/bin/sh\n");
    body.push_str("if [ \"$1\" = \"--version\" ]; then\n");
    body.push_str("  echo 'notmuch 0.38 (stub)'\n");
    body.push_str("  exit 0\n");
    body.push_str("fi\n");
    for p in paths {
        body.push_str(&format!("echo '{}'\n", p.display()));
    }
    body.push_str("exit 0\n");
    std::fs::write(&script, body).unwrap();
    let mut perms = std::fs::metadata(&script).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script, perms).unwrap();
    script
}

/// Drop a stub `chromium` script that records its own PID into
/// `pid_file` and then `exec`s `sleep <secs>`. Because `exec`
/// preserves the PID, the recorded PID is the one the AppRoot will
/// signal on the second `v` press.
fn write_stub_browser(dir: &Path, pid_file: &Path) -> PathBuf {
    let script = dir.join("chromium");
    let body = format!(
        "#!/bin/sh\necho $$ > '{}'\nexec sleep 60\n",
        pid_file.display(),
    );
    std::fs::write(&script, body).unwrap();
    let mut perms = std::fs::metadata(&script).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script, perms).unwrap();
    script
}

/// Press a single un-modified key through `AppRoot::process_event`.
fn press_key(root: &mut AppRoot, code: KeyCode) {
    let ev = Event::Key(KeyEvent::new(code, KeyModifiers::NONE));
    root.process_event(ev).unwrap();
}

/// Type a string into the AppRoot, one un-modified char at a time.
fn type_chars(root: &mut AppRoot, s: &str) {
    for c in s.chars() {
        press_key(root, KeyCode::Char(c));
    }
}

/// True while the process named by `pid` is alive. Uses `kill -0`
/// so we don't need a `libc` dep just for one signal probe.
fn pid_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Poll `cond` every 25ms until it returns true or `timeout` elapses.
/// Returns whether the condition was satisfied.
fn wait_until<F: FnMut() -> bool>(timeout: Duration, mut cond: F) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    cond()
}

/// Swap `$PATH` to `new` for the lifetime of the returned guard,
/// then restore the original on drop. The crate-wide `path_lock`
/// must be held while a guard exists; pass `_lock` to anchor the
/// borrow visually in the test body.
struct PathGuard {
    original: Option<std::ffi::OsString>,
}
impl PathGuard {
    fn set(new: &str) -> Self {
        let original = std::env::var_os("PATH");
        // SAFETY: caller holds `path_lock`; restore on drop.
        unsafe { std::env::set_var("PATH", new) };
        Self { original }
    }
}
impl Drop for PathGuard {
    fn drop(&mut self) {
        // SAFETY: same lock as `set`.
        unsafe {
            match self.original.take() {
                Some(v) => std::env::set_var("PATH", v),
                None => std::env::remove_var("PATH"),
            }
        }
    }
}

// ---- 1. Search round-trip --------------------------------------------

#[test]
fn search_roundtrip_navigates_finds_two_messages_and_returns_on_esc() {
    let _path_guard = path_lock().lock().unwrap();
    let tmp = TempDir::new().unwrap();
    let stub_dir = tmp.path().join("stubs");
    std::fs::create_dir_all(&stub_dir).unwrap();

    let (mut root, p1, p2) = make_root_with_two_emails(tmp.path().join("mail"));
    let _stub = write_stub_notmuch(&stub_dir, &[&p1, &p2]);

    // Only the stub is reachable, so `notmuch_available` sees the
    // stub and `apply_search_execute` runs it for the search call.
    let path_value = format!("{}", stub_dir.display());
    let _path = PathGuard::set(&path_value);

    // Sanity: start on the seeded INBOX, no search active yet.
    {
        let store = root.email_store_handle();
        let store = store.lock().unwrap();
        assert!(store.search_results.is_none(), "no search active at start",);
    }

    // `/` opens the search modal. With the stub on PATH,
    // `apply_open_search_input` does NOT close the modal.
    press_key(&mut root, KeyCode::Char('/'));

    // Type a query — every char gets routed to `SearchComponent` by
    // the modal-absorb branch in `process_event`.
    type_chars(&mut root, "tag:inbox");
    press_key(&mut root, KeyCode::Enter);

    // Enter fires `SearchExecute`, which shells out to the stub,
    // parses the two paths, and installs them as the virtual folder.
    {
        let store = root.email_store_handle();
        let store = store.lock().unwrap();
        let results = store
            .search_results
            .as_ref()
            .expect("search results virtual folder installed");
        assert_eq!(results.emails.len(), 2, "both stub-emitted hits resolved");
        assert_eq!(results.name, "Search: tag:inbox");
        let paths: Vec<_> = results.emails.iter().map(|e| e.file_path.clone()).collect();
        assert!(paths.contains(&p1), "hit-1 in results: {:?}", paths);
        assert!(paths.contains(&p2), "hit-2 in results: {:?}", paths);
    }
    // Layout switches to the Messages-only view so the breadcrumb
    // can show "Search: …" without a folder pane competing.
    assert_eq!(root.layout().current_view, View::Messages);

    // Esc exits the search results back to the prior folder view.
    press_key(&mut root, KeyCode::Esc);
    {
        let store = root.email_store_handle();
        let store = store.lock().unwrap();
        assert!(
            store.search_results.is_none(),
            "Esc clears the virtual folder",
        );
    }
    assert_eq!(
        root.layout().current_view,
        View::FolderMessages,
        "Esc restores the FolderMessages view",
    );
}

// ---- 2. Search with no notmuch ---------------------------------------

#[test]
fn search_with_no_notmuch_surfaces_status_and_does_not_crash() {
    let _path_guard = path_lock().lock().unwrap();
    let tmp = TempDir::new().unwrap();
    // Empty directory on PATH — no `notmuch` binary reachable.
    let stub_dir = tmp.path().join("empty");
    std::fs::create_dir_all(&stub_dir).unwrap();
    let path_value = format!("{}", stub_dir.display());
    let _path = PathGuard::set(&path_value);

    let (mut root, _p1, _p2) = make_root_with_two_emails(tmp.path().join("mail"));

    press_key(&mut root, KeyCode::Char('/'));

    // The status bar carries the install hint; no panic, no search.
    let status = root.status_message().clone().unwrap_or_default();
    assert!(
        status.contains("notmuch not found"),
        "expected notmuch-missing status; got {:?}",
        status,
    );
    let store = root.email_store_handle();
    let store = store.lock().unwrap();
    assert!(
        store.search_results.is_none(),
        "no virtual folder installed when notmuch is missing",
    );
}

// ---- 3. HTML viewer toggle -------------------------------------------

#[test]
fn html_viewer_toggle_spawns_and_terminates_child() {
    let _path_guard = path_lock().lock().unwrap();
    let tmp = TempDir::new().unwrap();
    let stub_dir = tmp.path().join("stubs");
    std::fs::create_dir_all(&stub_dir).unwrap();
    let pid_file = tmp.path().join("viewer.pid");

    let _stub = write_stub_browser(&stub_dir, &pid_file);
    // Keep `sleep` reachable too — the stub `exec`s it.
    let path_value = format!("{}:/usr/bin:/bin", stub_dir.display());
    let _path = PathGuard::set(&path_value);

    let (mut root, _p1, _p2) = make_root_with_two_emails(tmp.path().join("mail"));

    // First `v` press: `detect_browser` finds our stub via PATH, the
    // viewer launches, and `apply_toggle_html_viewer` records the
    // child + status message.
    press_key(&mut root, KeyCode::Char('v'));
    assert!(
        root.status_message()
            .as_deref()
            .map(|s| s.starts_with("HTML viewer launched"))
            .unwrap_or(false),
        "expected launch status; got {:?}",
        root.status_message(),
    );

    // The stub writes its PID to the sentinel before `exec sleep`,
    // so once the file appears we know the child is alive.
    let written = wait_until(Duration::from_secs(2), || pid_file.exists());
    assert!(written, "stub browser never wrote its PID file");
    let pid: u32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .expect("PID file holds a u32");
    assert!(pid_alive(pid), "stub child must be alive after launch");

    // Second `v`: AppRoot signals SIGTERM with a 1s escalation; the
    // stub (which `exec`d into `sleep`) exits and the status bar
    // confirms the close.
    press_key(&mut root, KeyCode::Char('v'));
    assert_eq!(
        root.status_message().as_deref(),
        Some("HTML viewer closed"),
        "second press must report close",
    );
    let died = wait_until(Duration::from_secs(2), || !pid_alive(pid));
    assert!(died, "child PID {} still alive after toggle off", pid);
}

// ---- 4. PWA manifest + service worker + root HTML --------------------

/// Build a minimal `WebState` with no email selected so `serve_email`
/// falls through to the welcome HTML — that's the shell that
/// `<link rel="manifest">` belongs to. The body-loader channel is
/// disconnected (rx dropped) on purpose: the welcome path never
/// dispatches a body parse.
fn webstate_with_no_selection(maildir: PathBuf) -> WebState {
    let store = EmailStore::new(maildir);
    let (tx, _rx) = std::sync::mpsc::channel::<PathBuf>();
    WebState {
        email_store: Arc::new(Mutex::new(store)),
        focused_pane: Arc::new(AtomicU8::new(ActivePane::Messages.to_u8())),
        body_request_tx: tx,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn pwa_manifest_sw_and_root_html_link_install_hooks() {
    // GET /manifest.json — Chrome/Edge install heuristic prerequisites.
    let manifest = serve_manifest().await;
    assert_eq!(manifest.status(), StatusCode::OK);
    let content_type = manifest
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        content_type.starts_with("application/manifest+json"),
        "manifest content-type: {:?}",
        content_type,
    );
    let body = to_bytes(manifest.into_body(), 64 * 1024).await.unwrap();
    let body = std::str::from_utf8(&body).unwrap();
    for field in ["\"name\"", "\"start_url\"", "\"display\"", "\"icons\""] {
        assert!(body.contains(field), "manifest missing {}", field);
    }

    // GET /sw.js — service worker must register an install handler
    // that pre-caches the shell so Chrome accepts the registration.
    let sw = serve_service_worker().await;
    assert_eq!(sw.status(), StatusCode::OK);
    let sw_ct = sw
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(sw_ct.contains("javascript"), "sw content-type: {:?}", sw_ct);
    let sw_body = to_bytes(sw.into_body(), 16 * 1024).await.unwrap();
    let sw_body = std::str::from_utf8(&sw_body).unwrap();
    assert!(
        sw_body.contains("addEventListener('install'"),
        "service worker missing install handler",
    );
    assert!(
        sw_body.contains("caches.open"),
        "service worker must pre-cache the shell",
    );

    // GET / — `serve_email` with no selection returns the welcome
    // shell, which must link the manifest and register the worker.
    let tmp = TempDir::new().unwrap();
    let state = webstate_with_no_selection(tmp.path().to_path_buf());
    let root_resp = serve_email(State(state)).await;
    assert_eq!(root_resp.status(), StatusCode::OK);
    let root_body = to_bytes(root_resp.into_body(), 64 * 1024).await.unwrap();
    let root_body = std::str::from_utf8(&root_body).unwrap();
    let head_end = root_body
        .find("</head>")
        .expect("welcome HTML must have a head");
    let head = &root_body[..head_end];
    assert!(
        head.contains(r#"<link rel="manifest" href="/manifest.json">"#),
        "welcome <head> missing manifest link",
    );
    assert!(
        head.contains("navigator.serviceWorker.register('/sw.js')"),
        "welcome <head> missing service-worker registration",
    );
}
