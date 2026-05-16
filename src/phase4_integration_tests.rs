//! Phase 4 integration tests (vu-tpv).
//!
//! End-to-end coverage for the four Phase 4 features once 4.a-4.d
//! landed:
//!
//!   * 4.a — `[web].port` / `[web].bind` round-trip from `vulthor.toml`
//!     into a live `WebServer` that actually binds.
//!   * 4.b — `[keybindings]` overrides flow through `Config::load` and
//!     `AppRoot::with_config` into the resolved `Keymap` AppRoot
//!     dispatches keys against. Conflicts are rejected at load time.
//!   * 4.c — user theme files at `<themes-dir>/<name>.toml` plus
//!     `[theme].overrides` resolve into the final `Theme` via
//!     `theme::build_theme_with`.
//!   * 4.d — inotify watching: a file create under `INBOX/cur/`
//!     produces a `Msg::MailDirChanged` that invalidates the cached
//!     folder headers within one second of the write.
//!
//! Per-test scope (mirrors vu-tpv acceptance):
//!
//! 1. `web_block_round_trips_through_config_and_binds_listener`
//!    — `[web] port=<free> bind="127.0.0.1"` in TOML reaches the live
//!    `WebServer::start`; `/health` returns 200. A second case asserts
//!    `bind = "0.0.0.0"` survives validation (the bind we don't actually
//!    open in CI).
//! 2. `keybindings_override_archive_to_e_propagates_to_apphroot_keymap`
//!    — `[keybindings] archive = "e"` + `draft_edit = "X"` in TOML
//!    flows through `Config::load` and `AppRoot::with_config` so
//!    `root.keymap().lookup_single('e') == Some(Action::Archive)` and
//!    the default `a` no longer maps to `Archive`.
//! 3. `theme_user_file_and_overrides_resolve_via_config`
//!    — A user theme file `<tmp>/themes/dark.toml` is loaded by
//!    `theme::build_theme_with` (with an explicit loader pointed at
//!    the tmpdir) for a config carrying `[theme] name = "dark"`. The
//!    resolved `Theme.primary` matches the file's hex.
//! 4. `inotify_create_under_cur_invalidates_folder_within_one_second`
//!    — A real watcher spawned via `AppRoot::init_maildir_watcher`
//!    observes a `cur/` create within 1 s; the resulting
//!    `Msg::MailDirChanged` clears `INBOX.is_loaded` and empties the
//!    cached headers.
//! 5. `keybindings_conflict_is_rejected_at_config_load`
//!    — `[keybindings] archive = "e"` without freeing the default `e`
//!    (DraftEdit) makes `Config::load` return a
//!    `VulthorError::KeybindingConflict` naming both action keys.

#![cfg(test)]

use std::path::PathBuf;
use std::sync::atomic::AtomicU8;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ratatui::style::Color;
use tempfile::TempDir;
use tokio::net::TcpListener;

use crate::components::AppRoot;
use crate::components::ReplyKind;
use crate::components::draft::DraftStatus;
use crate::config::Config;
use crate::email::{Email, EmailStore, Folder};
use crate::error::VulthorError;
use crate::keymap::Action;
use crate::layout::ActivePane;
use crate::maildir::MaildirScanner;
use crate::theme::{VulthorTheme, build_theme_with, load_user_theme_from_path};
use crate::web::WebServer;

// ---- shared helpers ---------------------------------------------------

/// Build a minimal MailDir tree at `<root>/INBOX/{cur,new,tmp}` and
/// return the INBOX path. Mirrors the layout `mbsync` lays down so the
/// inotify integration test exercises a realistic create-under-cur path.
fn make_inbox_tree(root: &std::path::Path) -> PathBuf {
    let inbox = root.join("INBOX");
    for sub in ["cur", "new", "tmp"] {
        std::fs::create_dir_all(inbox.join(sub)).unwrap();
    }
    inbox
}

/// `AppRoot` wired to a freshly seeded `EmailStore` whose root folder
/// is `maildir`. Adds a single INBOX subfolder with one placeholder
/// email so cache-invalidation can be observed after a watcher event.
/// Mirrors the in-tree `account_select_repoints_maildir_watcher_at_new_root`
/// setup without dragging in its assertions.
fn root_with_seeded_inbox(maildir: PathBuf, inbox: &std::path::Path) -> AppRoot {
    let store = EmailStore::new(maildir.clone());
    let scanner = MaildirScanner::new(maildir);
    let root = AppRoot::new(Arc::new(Mutex::new(store)), scanner);
    {
        let handle = root.email_store_handle();
        let mut store = handle.lock().unwrap();
        let mut f = Folder::new("INBOX".to_string(), inbox.to_path_buf());
        f.add_email(Email::new(inbox.join("cur").join("seed.eml")));
        f.is_loaded = true;
        f.total_count = 1;
        store.root_folder.subfolders.clear();
        store.root_folder.add_subfolder(f);
    }
    root
}

/// Reserve a port by binding `127.0.0.1:0`, then closing the socket so
/// the kernel-assigned port is free for the integration's real bind.
/// Race window is tiny — Phase 4.a tests already use the same trick.
async fn pick_free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

/// Poll `cond` every 25 ms until it returns true or `timeout` elapses.
/// Returns whether the condition was met before the deadline.
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

/// Write `contents` to `<dir>/vulthor.toml` and return its path. Keeps
/// the per-test temp dirs from leaking the literal "vulthor.toml" name
/// past the test boundary.
fn write_toml(dir: &std::path::Path, contents: &str) -> PathBuf {
    let path = dir.join("vulthor.toml");
    std::fs::write(&path, contents).unwrap();
    path
}

// ---- 1. [web] block round-trip ---------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn web_block_round_trips_through_config_and_binds_listener() {
    // Pick a free loopback port and write a config with the matching
    // `[web]` block. Loading via `Config::load_from_file` (the same
    // entry point `main.rs` uses) must hand us back the configured
    // bind/port verbatim.
    let port = pick_free_port().await;
    let tmp = TempDir::new().unwrap();
    let toml = format!(
        r#"
maildir_path = "{}"

[web]
port = {}
bind = "127.0.0.1"
"#,
        tmp.path().join("Mail").display(),
        port,
    );
    let config_path = write_toml(tmp.path(), &toml);
    let cfg = Config::load(Some(config_path)).await.expect("config loads");
    assert_eq!(cfg.web.port, port);
    assert_eq!(cfg.web.bind, "127.0.0.1");

    // Forward the resolved bind/port into `WebServer::new` (same call
    // shape as `main.rs`) and spawn `start`. The /health endpoint is a
    // single async handler — once it returns 200, we've proven the
    // listener accepted a real TCP connection on the configured port.
    let store = Arc::new(Mutex::new(EmailStore::new(tmp.path().to_path_buf())));
    let focused = Arc::new(AtomicU8::new(ActivePane::Messages.to_u8()));
    let (tx, _rx) = std::sync::mpsc::channel::<PathBuf>();
    let server = WebServer::new(cfg.web.bind.clone(), cfg.web.port, store, focused, tx);
    let server_task = tokio::spawn(async move {
        let _ = server.start().await;
    });

    // Poll /health by hand over a raw TcpStream — `reqwest` is not in
    // the dep graph, and the handler returns a plain `"OK"` body that
    // a one-shot GET is sufficient to verify. Once we see `HTTP/1.1 200`
    // we've proven the listener accepted the connection on the
    // configured port. Bounded at 2 s to keep CI flake-tolerant.
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let addr = format!("127.0.0.1:{}", port);
    let request =
        format!("GET /health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    let mut last_outcome: Option<String> = None;
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        match tokio::net::TcpStream::connect(&addr).await {
            Ok(mut stream) => {
                if stream.write_all(request.as_bytes()).await.is_err() {
                    last_outcome = Some("write failed".into());
                    continue;
                }
                let mut buf = Vec::new();
                if stream.read_to_end(&mut buf).await.is_err() {
                    last_outcome = Some("read failed".into());
                    continue;
                }
                let response = String::from_utf8_lossy(&buf);
                if response.starts_with("HTTP/1.1 200") && response.ends_with("OK") {
                    server_task.abort();
                    return;
                }
                last_outcome = Some(response.into_owned());
            }
            Err(e) => last_outcome = Some(format!("connect refused: {e}")),
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    server_task.abort();
    panic!(
        "web server on configured port {} never returned 200 from /health (last outcome: {:?})",
        port, last_outcome,
    );
}

#[tokio::test(flavor = "current_thread")]
async fn web_bind_zero_zero_zero_zero_passes_validation_via_config_load() {
    // `bind = "0.0.0.0"` is the literal pattern called out in vu-tpv —
    // we don't actually bind it (CI sandboxes may block 0.0.0.0) but
    // the load + validate path must accept it. Bind hostnames remain
    // rejected via the `malformed_bind_rejects_via_load` config test.
    let tmp = TempDir::new().unwrap();
    let toml = format!(
        r#"
maildir_path = "{}"

[web]
port = 9999
bind = "0.0.0.0"
"#,
        tmp.path().join("Mail").display(),
    );
    let path = write_toml(tmp.path(), &toml);
    let cfg = Config::load(Some(path)).await.expect("0.0.0.0 validates");
    assert_eq!(cfg.web.port, 9999);
    assert_eq!(cfg.web.bind, "0.0.0.0");
}

// ---- 2. [keybindings] override propagation ---------------------------

#[tokio::test(flavor = "current_thread")]
async fn keybindings_override_archive_to_e_propagates_to_apphroot_keymap() {
    // `[keybindings] archive = "e"` without also moving the default
    // `e`-bound `draft_edit` is a conflict (see test #5). The
    // VISION.md scenario is to move both: archive takes `e`, draft_edit
    // takes `E`. Confirm Config::load → AppRoot::with_config wires the
    // resolved table all the way through to the dispatch keymap.
    let tmp = TempDir::new().unwrap();
    let toml = format!(
        r#"
maildir_path = "{}"

[keybindings]
archive = "e"
draft_edit = "X"
"#,
        tmp.path().join("Mail").display(),
    );
    let cfg = Config::load(Some(write_toml(tmp.path(), &toml)))
        .await
        .expect("validated config");

    let store = EmailStore::new(tmp.path().to_path_buf());
    let scanner = MaildirScanner::new(tmp.path().to_path_buf());
    let root = AppRoot::with_config(Arc::new(Mutex::new(store)), scanner, cfg);

    let map = root.keymap();
    let e_event = crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('e'),
        crossterm::event::KeyModifiers::NONE,
    );
    assert_eq!(
        map.lookup_single(e_event),
        Some(Action::Archive),
        "override should rebind 'e' to Archive end-to-end",
    );

    // The default `a` no longer triggers Archive — the override
    // displaced it. Mirrors the assertion in keymap::tests but at the
    // Config → AppRoot integration layer.
    let a_event = crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('a'),
        crossterm::event::KeyModifiers::NONE,
    );
    assert_eq!(
        map.lookup_single(a_event),
        None,
        "default 'a' must be unbound after archive moves to 'e'",
    );

    // Capital `X` (draft_edit's new home) resolves correctly too.
    let big_x = crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('X'),
        crossterm::event::KeyModifiers::NONE,
    );
    assert_eq!(map.lookup_single(big_x), Some(Action::DraftEdit));
}

// ---- 3. [theme] file + overrides -------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn theme_user_file_and_overrides_resolve_via_config() {
    // Write a user theme file at `<tmp>/themes/dark.toml`. The
    // production loader resolves names against
    // `~/.config/vulthor/themes/<name>.toml`; the test uses
    // `build_theme_with`'s loader-injection seam to point at the
    // tmpdir without mutating `$HOME` (which other tests rely on).
    let tmp = TempDir::new().unwrap();
    let themes_dir = tmp.path().join("themes");
    std::fs::create_dir_all(&themes_dir).unwrap();
    let theme_path = themes_dir.join("dark.toml");
    std::fs::write(
        &theme_path,
        r##"primary = "#FF0000"
accent  = "#00FF00"
"##,
    )
    .unwrap();

    let toml = format!(
        r##"
maildir_path = "{}"

[theme]
name = "dark"

[theme.overrides]
accent = "#0000FF"
"##,
        tmp.path().join("Mail").display(),
    );
    let cfg = Config::load(Some(write_toml(tmp.path(), &toml)))
        .await
        .expect("config loads");
    assert_eq!(cfg.theme.name.as_deref(), Some("dark"));

    let resolved = build_theme_with(&cfg, |name| {
        assert_eq!(name, "dark", "loader sees the configured name");
        load_user_theme_from_path(&themes_dir.join(format!("{name}.toml")))
    })
    .expect("theme builds");

    // File value comes through where override is silent.
    assert_eq!(
        resolved.primary,
        Color::Rgb(0xFF, 0x00, 0x00),
        "user theme file's primary must reach Theme.primary",
    );
    // `[theme].overrides` wins over the file (resolution order:
    // built-in → file → overrides). Mirrors the unit test
    // `build_theme_layers_file_then_overrides`.
    assert_eq!(
        resolved.accent,
        Color::Rgb(0x00, 0x00, 0xFF),
        "overrides take priority over the user theme file",
    );
    // Untouched roles keep their built-in palette.
    assert_eq!(resolved.cyan, VulthorTheme::CYAN);

    // `AppRoot::set_theme` plumbs the resolved palette into AppRoot.
    // Tests in main.rs cover that main wires set_theme in; here we
    // confirm the test seam reflects whatever we installed.
    let scanner = MaildirScanner::new(tmp.path().to_path_buf());
    let store = EmailStore::new(tmp.path().to_path_buf());
    let mut root = AppRoot::with_config(Arc::new(Mutex::new(store)), scanner, cfg);
    root.set_theme(resolved.clone());
    assert_eq!(root.theme().primary, Color::Rgb(0xFF, 0x00, 0x00));
    assert_eq!(root.theme(), &resolved);
}

// ---- 4. inotify MailDir watch ----------------------------------------

#[test]
fn inotify_create_under_cur_invalidates_folder_within_one_second() {
    let tmp = TempDir::new().unwrap();
    let maildir = tmp.path().to_path_buf();
    let inbox = make_inbox_tree(&maildir);

    let mut root = root_with_seeded_inbox(maildir.clone(), &inbox);

    // Spawn the real watcher (DEFAULT_DEBOUNCE = 250 ms). We don't
    // touch `process_event`; the pump_maildir_watcher seam mirrors the
    // drain `tick`/`render` would run.
    root.init_maildir_watcher();

    // Give the inotify backend a beat to register the watch before we
    // touch the tree. `notify`'s inotify backend can drop events that
    // race the watch attachment.
    std::thread::sleep(Duration::from_millis(100));

    // Create the file `mbsync` would land — under `cur/`, with a
    // maildir-style name. The watcher should fire a `MailDirChanged`
    // for the parent (INBOX) once the debounce window closes.
    let target = inbox.join("cur").join("1700000000.M0P0Q0.host:2,S");
    std::fs::write(&target, b"From: a@b\r\n\r\nbody").unwrap();

    let store_handle = root.email_store_handle();
    let observed = wait_until(Duration::from_secs(1), || {
        root.pump_maildir_watcher();
        let store = store_handle.lock().unwrap();
        let f = &store.root_folder.subfolders[0];
        f.emails.is_empty() && !f.is_loaded
    });
    assert!(
        observed,
        "inotify MailDirChanged must invalidate INBOX within 1s of the cur/ create",
    );
}

// ---- 2b. [keybindings] dispatch-through-override end-to-end ----------
//
// These tests close the gap that vu-otn fixes: an override resolved into
// the keymap must actually reach the runtime when the user presses the
// new key. We seed a real INBOX file on disk, rebind the action to a
// non-default key, drive that key through `process_event`, and observe
// the side effect (file moved, draft started, undo stack popped).

/// Seed a Messages-pane AppRoot at `<root>/INBOX/cur/<filename>` with
/// the cursor on the one email. Shared by every override-dispatch test
/// below so the setup boilerplate stays in one place. Mirrors the
/// in-tree `make_root_with_disk_inbox` helper used by `root.rs` tests.
fn override_root(
    maildir: &std::path::Path,
    filename: &str,
    overrides: &[(&str, &str)],
) -> (AppRoot, PathBuf) {
    let inbox_cur = maildir.join("INBOX").join("cur");
    std::fs::create_dir_all(&inbox_cur).unwrap();
    let src = inbox_cur.join(filename);
    // Real headers so reply-template tests have something to work with.
    std::fs::write(
        &src,
        "From: Alice <alice@example.com>\r\n\
         To: Tester <tester@example.com>\r\n\
         Subject: Lunch tomorrow?\r\n\
         Message-ID: <orig-1@example.com>\r\n\
         Date: Sat, 16 May 2026 12:00:00 +0000\r\n\
         \r\n\
         hi\r\n",
    )
    .unwrap();

    let mut store = EmailStore::new(maildir.to_path_buf());
    let mut inbox = Folder::new("INBOX".into(), maildir.join("INBOX"));
    let mut email = Email::new(src.clone());
    let _ = email.parse_headers_only();
    inbox.add_email(email);
    inbox.is_loaded = true;
    store.root_folder.add_subfolder(inbox);
    store.enter_folder_by_path(&[0]);
    store.select_email(0);

    let mut cfg = Config {
        maildir_path: maildir.to_path_buf(),
        ..Config::default()
    };
    for (action, key) in overrides {
        cfg.keybindings
            .inner
            .insert((*action).to_string(), (*key).to_string());
    }

    let scanner = MaildirScanner::new(maildir.to_path_buf());
    let mut root = AppRoot::with_config(Arc::new(Mutex::new(store)), scanner, cfg);
    root.set_active_pane_for_test(ActivePane::Messages);
    (root, src)
}

/// Convenience: build a `KeyEvent` from a single ASCII char with no
/// modifiers. Capital letters work too — `Keymap::normalize` strips
/// SHIFT at lookup time.
fn key(c: char) -> crossterm::event::Event {
    crossterm::event::Event::Key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char(c),
        crossterm::event::KeyModifiers::NONE,
    ))
}

#[test]
fn keybindings_override_archive_drives_through_process_event_to_disk_move() {
    // Override `archive` to `x` (and free `e`'s prior owner `draft_edit`
    // — the conflict path is asserted in test #5). Pressing `x` in the
    // Messages pane must walk:
    //   process_event → keymap.lookup_single('x') → Action::Archive
    //   → action_to_msg → Msg::Archive → apply_move_action(Archive).
    // The on-disk file lands in `<maildir>/Archive/cur/`.
    let tmp = TempDir::new().unwrap();
    let (mut root, src) = override_root(tmp.path(), "msg-arch", &[("archive", "x")]);

    root.process_event(key('x')).unwrap();

    let archive = tmp.path().join("Archive").join("cur").join("msg-arch");
    assert!(
        archive.exists(),
        "override 'x' must move file to Archive/cur"
    );
    assert!(!src.exists(), "original INBOX/cur entry must be gone");
    assert_eq!(root.undo_stack_len(), 1);

    // The default `a` must no longer trigger Archive — the override
    // displaced it. Same regression the unit test catches at the keymap
    // layer, asserted here at the dispatch layer.
    let (mut root2, src2) = override_root(tmp.path(), "msg-arch2", &[("archive", "x")]);
    root2.process_event(key('a')).unwrap();
    assert!(
        src2.exists(),
        "default 'a' must be inert after archive is rebound",
    );
    assert_eq!(root2.undo_stack_len(), 0);
}

#[test]
fn keybindings_override_star_drives_through_process_event_to_filename_flag() {
    // Star toggles the maildir `F` flag on the cursor email's filename.
    // Override `star` to `y`; pressing `y` must rename the file from
    // `:2,S` to `:2,FS`.
    let tmp = TempDir::new().unwrap();
    let (mut root, src) = override_root(tmp.path(), "msg-star:2,S", &[("star", "y")]);

    root.process_event(key('y')).unwrap();

    let starred = tmp.path().join("INBOX").join("cur").join("msg-star:2,FS");
    assert!(starred.exists(), "override 'y' must add the F flag");
    assert!(!src.exists());
    let store = root.email_store_handle();
    let store = store.lock().unwrap();
    assert!(store.root_folder.subfolders[0].emails[0].is_flagged);
}

#[test]
fn keybindings_override_delete_drives_through_process_event_to_trash() {
    // Delete moves the file to `<maildir>/Trash/cur/`. Override `delete`
    // to `z`; pressing `z` must land the file under Trash and push an
    // undo entry.
    let tmp = TempDir::new().unwrap();
    let (mut root, src) = override_root(tmp.path(), "msg-del", &[("delete", "z")]);

    root.process_event(key('z')).unwrap();

    let trash = tmp.path().join("Trash").join("cur").join("msg-del");
    assert!(trash.exists(), "override 'z' must move file to Trash/cur");
    assert!(!src.exists());
    assert_eq!(root.undo_stack_len(), 1);
}

#[test]
fn keybindings_override_reply_all_drives_through_process_event_to_draft_start() {
    // ReplyAll defaults to `r`. Rebind to `w` (free in the default
    // keymap) and verify the runtime starts the draft with reply-all
    // semantics — the original To recipient must appear on the new To
    // line, distinguishing from sender-only reply.
    let tmp = TempDir::new().unwrap();
    let (mut root, _src) = override_root(tmp.path(), "msg-r", &[("reply_all", "w")]);

    root.process_event(key('w')).unwrap();

    assert!(
        root.has_pending_editor(),
        "override 'w' must park an editor"
    );
    let state = root.draft().state().expect("draft started");
    assert_eq!(state.reply_kind, ReplyKind::ReplyAll);
    assert!(
        state.compose.to.contains("Tester <tester@example.com>"),
        "reply-all must include the original To recipient, got {:?}",
        state.compose.to,
    );
}

#[test]
fn keybindings_override_forward_drives_through_process_event_to_draft_start() {
    // Forward defaults to `f`. Rebind to `i` and verify the runtime
    // builds a forward template (empty To, `Fwd:` subject prefix).
    let tmp = TempDir::new().unwrap();
    let (mut root, _src) = override_root(tmp.path(), "msg-fwd", &[("forward", "i")]);

    root.process_event(key('i')).unwrap();

    assert!(root.has_pending_editor());
    let state = root.draft().state().expect("draft started");
    assert_eq!(state.reply_kind, ReplyKind::Forward);
    assert_eq!(state.compose.to, "");
    assert_eq!(state.compose.subject, "Fwd: Lunch tomorrow?");
}

#[test]
fn keybindings_override_reply_later_drives_through_process_event_to_draft_ready() {
    // ReplyLater (default `R`) writes an empty-body draft straight to
    // `<maildir>/Drafts/cur/` without launching an editor. Rebind to
    // `t` and verify the draft surfaces in `ReadyToSend`.
    let tmp = TempDir::new().unwrap();
    let (mut root, _src) = override_root(tmp.path(), "msg-rl", &[("reply_later", "t")]);

    root.process_event(key('t')).unwrap();

    assert!(
        !root.has_pending_editor(),
        "reply-later must not park an editor",
    );
    let state = root.draft().state().expect("draft started");
    assert_eq!(state.reply_kind, ReplyKind::ReplyLater);
    assert_eq!(state.status, DraftStatus::ReadyToSend);
    assert!(
        tmp.path()
            .join("Drafts")
            .join("cur")
            .read_dir()
            .map(|it| it.count() == 1)
            .unwrap_or(false),
        "exactly one reply-later draft file must land in Drafts/cur/",
    );
}

#[test]
fn keybindings_override_undo_drives_through_process_event_to_stack_pop() {
    // Undo defaults to `u`. Rebind to `b`; first archive to push a
    // mutation, then pressing `b` must restore the file to INBOX/cur.
    let tmp = TempDir::new().unwrap();
    // Rebind BOTH archive (so we can push a mutation we control without
    // its default `a`-key) and undo. Archive on `c` keeps the test self
    // contained.
    let (mut root, src) = override_root(tmp.path(), "msg-undo", &[("archive", "c"), ("undo", "b")]);

    root.process_event(key('c')).unwrap();
    let archive = tmp.path().join("Archive").join("cur").join("msg-undo");
    assert!(archive.exists() && !src.exists());
    assert_eq!(root.undo_stack_len(), 1);

    root.process_event(key('b')).unwrap();
    assert!(src.exists(), "override 'b' must restore the file to INBOX");
    assert!(!archive.exists());
    assert_eq!(root.undo_stack_len(), 0);
}

#[test]
fn default_gr_sequence_still_emits_reply_after_dispatch_centralisation() {
    // Regression: vu-otn introduces a pending-sequence pre-empt that
    // routes the second key of a `g`-prefix sequence into the component
    // BEFORE the central keymap dispatch. If the pre-empt is wrong,
    // the second key (`r`) would hit the keymap as a single-key
    // ReplyAll and the sequence (`gr` → Reply) would silently break.
    //
    // No overrides — exercises the default keymap end-to-end.
    let tmp = TempDir::new().unwrap();
    let (mut root, _src) = override_root(tmp.path(), "msg-gr", &[]);

    root.process_event(key('g')).unwrap();
    assert!(
        !root.has_pending_editor(),
        "lone 'g' must arm the prefix, not launch anything",
    );
    assert!(root.draft().state().is_none());

    root.process_event(key('r')).unwrap();

    let state = root.draft().state().expect("gr must start a Reply draft");
    assert_eq!(
        state.reply_kind,
        ReplyKind::Reply,
        "gr must dispatch reply-sender, not reply-all",
    );
}

// ---- 5. [keybindings] conflict rejected at load -----------------------

#[tokio::test(flavor = "current_thread")]
async fn keybindings_conflict_is_rejected_at_config_load() {
    // Rebinding `archive` to `e` without also moving the default `e`
    // (DraftEdit) collides. `Config::validate` (driven from every
    // loader path) must surface the structured `KeybindingConflict`
    // error so the user sees both colliding action names at startup —
    // not at the first keypress.
    let tmp = TempDir::new().unwrap();
    let toml = format!(
        r#"
maildir_path = "{}"

[keybindings]
archive = "e"
"#,
        tmp.path().join("Mail").display(),
    );
    let err = Config::load(Some(write_toml(tmp.path(), &toml)))
        .await
        .expect_err("conflict must be rejected at load time");
    match err {
        VulthorError::KeybindingConflict {
            key,
            action_a,
            action_b,
        } => {
            assert_eq!(key, "e");
            let mut names = [action_a, action_b];
            names.sort();
            assert_eq!(names, ["archive".to_string(), "draft_edit".to_string()]);
        }
        other => panic!("expected KeybindingConflict, got {other:?}"),
    }
}
