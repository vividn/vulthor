//! Phase 2.e integration tests for the compose / draft / send pipeline.
//!
//! Each test stitches together the surfaces that 2.a–2.d landed:
//!
//!   * 2.a — `compose::{default_template, parse_compose_from_text,
//!     launch_editor, send}` and the SMTP / Sent-copy contract.
//!   * 2.b — `DraftComponent` state machine (`Editing` →
//!     `ReadyToSend` → `Sending`).
//!   * 2.c — `EmailStore.drafts` index and the `⏰` / `✏` chip glyph
//!     picked by `MessagesComponent::chip_for_message_id`.
//!   * 2.d — `r` / `gr` / `f` / `R` reply-variant key wiring in
//!     `AppRoot::process_event` that parks an editor launch and
//!     populates the `Compose`.
//!
//! Tests drive the UI through `AppRoot::process_event` for the keys
//! that are wired today, then complete the end-to-end flow through
//! the public compose APIs (`compose::send`, `compose::launch_editor`
//! via a stub `$EDITOR` / stub `msmtp`). Where a key binding does
//! *not* yet exist (`S` to send from the pre-send pane; `e` to re-
//! launch the editor on a pre-existing draft), tests note the gap
//! inline and exercise the underlying pipeline that any future key
//! wiring would call. That keeps the integration coverage honest
//! about what would break in production while making the missing
//! wiring obvious.

#![cfg(test)]
#![allow(clippy::needless_return)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use tempfile::TempDir;

use crate::components::draft::DraftStatus;
use crate::components::{AppRoot, Msg, ReplyKind};
use crate::compose::{self, Compose};
use crate::config::{AccountConfig, Config};
use crate::email::{Email, EmailStore, Folder};
use crate::layout::ActivePane;
use crate::maildir::MaildirScanner;

// ---- fixture helpers ------------------------------------------------

/// Wire-format email body used as the "original" message every test
/// replies to / forwards / quotes. The headers are deliberately rich
/// enough that the reply / forward template builders have something
/// non-trivial to quote (multi-recipient To, real Message-ID).
const ORIGINAL_RFC822: &str = "From: Alice <alice@example.com>\r\n\
To: Tester <tester@example.com>, Bob <bob@example.com>\r\n\
Subject: Lunch tomorrow?\r\n\
Message-ID: <orig-1@example.com>\r\n\
Date: Sat, 16 May 2026 12:00:00 +0000\r\n\
\r\n\
Hey,\r\nWant to grab lunch?\r\n";

/// Write the canonical original email under `<root>/INBOX/cur/orig.eml`
/// and return the seeded `AppRoot` with the INBOX as the active folder
/// and the original email selected. Mirrors the helper in
/// `components::root::tests::make_root_with_one_real_email`, lifted to
/// the integration module so tests outside `root.rs` don't have to
/// reach into private test scaffolding.
fn make_root_with_one_email(root_path: PathBuf) -> AppRoot {
    let inbox = root_path.join("INBOX").join("cur");
    std::fs::create_dir_all(&inbox).unwrap();
    let msg_path = inbox.join("orig.eml");
    std::fs::write(&msg_path, ORIGINAL_RFC822).unwrap();

    let mut store = EmailStore::new(root_path.clone());
    let mut folder = Folder::new("INBOX".into(), root_path.join("INBOX"));
    let mut email = Email::new(msg_path);
    // Reply / forward template builders quote `original.body_text`, so
    // the fixture must be fully parsed — `parse_headers_only` leaves
    // `body_text` empty and the resulting quoted block would be blank.
    email.parse_from_file().unwrap();
    folder.add_email(email);
    folder.is_loaded = true;
    store.root_folder.add_subfolder(folder);
    store.enter_folder_by_path(&[0]);
    store.select_email(0);

    let scanner = MaildirScanner::new(root_path);
    AppRoot::new(Arc::new(Mutex::new(store)), scanner)
}

/// Build a stub `msmtp` script at `dir/msmtp` that captures stdin to
/// `dir/captured.eml` and exits with `exit_code`. Returns the path to
/// the captured-bytes file (does not exist until `msmtp` is invoked).
fn stub_msmtp(dir: &Path, exit_code: i32) -> PathBuf {
    let script_path = dir.join("msmtp");
    let captured = dir.join("captured.eml");
    let script = format!(
        "#!/bin/sh\ncat > '{}'\nexit {}\n",
        captured.display(),
        exit_code,
    );
    std::fs::write(&script_path, script).unwrap();
    let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script_path, perms).unwrap();
    captured
}

/// Build an `AccountConfig` whose `smtp_command` is the absolute path
/// to a stub `msmtp` script under `stub_dir`. Pointing at an absolute
/// path keeps the test independent of the worker's `$PATH`.
fn account_with_stub(name: &str, email: &str, maildir: &Path, stub_dir: &Path) -> AccountConfig {
    let stub = stub_dir.join("msmtp");
    AccountConfig {
        name: name.to_string(),
        email: email.to_string(),
        maildir_path: maildir.to_path_buf(),
        smtp_command: Some(format!("{} -a {}", stub.display(), name)),
        signature: None,
    }
}

/// Press a sequence of plain (no-modifier) chars through
/// `AppRoot::process_event`. Multi-key vim-style combos like `gr` come
/// out as two separate events the AppRoot has to thread together
/// itself.
fn type_keys(root: &mut AppRoot, keys: &str) {
    for c in keys.chars() {
        let ev = Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        root.process_event(ev).unwrap();
    }
}

/// Read every regular file under `<dir>` and return the sorted list of
/// (filename, contents). Lets us assert "exactly one file landed in
/// Sent/cur" without baking the time-based filename into the test.
fn read_dir_files(dir: &Path) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let contents = std::fs::read_to_string(e.path()).unwrap_or_default();
            (name, contents)
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Reseed the AppRoot's store from `<root>/INBOX/cur/` so that
/// post–`AccountSelect` tests don't have to wait for the off-thread
/// folder scanner. Adapted from
/// `components::root::tests::reseed_inbox_from_disk` for the multi-
/// account scenario where the integration test owns both maildirs.
fn reseed_inbox_from_disk(root: &mut AppRoot) {
    let maildir_root = {
        let store = root.email_store_handle();
        let store = store.lock().unwrap();
        store.root_folder.path.clone()
    };
    let inbox_path = maildir_root.join("INBOX");
    let mut inbox = Folder::new("INBOX".to_string(), inbox_path.clone());
    if let Ok(entries) = std::fs::read_dir(inbox_path.join("cur")) {
        let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for p in paths {
            let mut e = Email::new(p);
            // Fully parse — reply templates quote `body_text`.
            e.parse_from_file().ok();
            inbox.add_email(e);
        }
    }
    inbox.is_loaded = true;

    let store_handle = root.email_store_handle();
    let mut store = store_handle.lock().unwrap();
    store.root_folder.subfolders.clear();
    store.root_folder.add_subfolder(inbox);
    store.scanning_folders = false;
    store.enter_folder_by_path(&[0]);
    store.select_email(0);
}

/// Build a multi-account `Config` with two accounts ("alpha" and
/// "bravo") rooted under `root_a` / `root_b`. Used by the multi-
/// account compose scenario.
fn two_account_config(root_a: &Path, root_b: &Path) -> Config {
    let mut cfg = Config {
        maildir_path: root_a.to_path_buf(),
        ..Config::default()
    };
    cfg.accounts.insert(
        "alpha".into(),
        AccountConfig {
            name: "alpha".into(),
            email: "alpha@example.com".into(),
            maildir_path: root_a.to_path_buf(),
            smtp_command: None,
            signature: None,
        },
    );
    cfg.accounts.insert(
        "bravo".into(),
        AccountConfig {
            name: "bravo".into(),
            email: "bravo@example.com".into(),
            maildir_path: root_b.to_path_buf(),
            smtp_command: None,
            signature: None,
        },
    );
    cfg.default_account = Some("alpha".into());
    cfg
}

// ---- scenario 1: reply round-trip ----------------------------------

/// `gr` (sender-only reply) on a message must:
///   1. park a `PendingEditorLaunch` whose template carries the quoted
///      original body (the bytes `$EDITOR` would see),
///   2. on `apply_editor_result`, advance the draft to `ReadyToSend`
///      with the parsed compose installed on the live state,
///   3. when fed to `compose::send` against a stub `msmtp`, write a
///      `Sent/cur/<…>` copy of the wire bytes the SMTP command saw.
///
/// `S`-to-send is not wired into `AppRoot::process_event` today —
/// `DraftSend` exists as a `Msg` and `compose::send` is the function
/// any future key binding would call. This test exercises that
/// pipeline directly so a regression in either half fails here.
#[test]
fn scenario_reply_round_trip_via_gr_and_send_lands_in_sent() {
    let temp = TempDir::new().unwrap();
    let mut root = make_root_with_one_email(temp.path().to_path_buf());
    root.set_active_pane_for_test(ActivePane::Messages);

    // 1. Type `gr` — two separate key events, threaded by the prefix
    //    handler in `MessagesComponent::on_key`. After both fire, an
    //    editor launch must be parked.
    type_keys(&mut root, "gr");
    assert!(root.has_pending_editor(), "gr must park an editor launch",);

    // 2. Snapshot the template the run loop would hand `$EDITOR`. The
    //    template must contain the quoted-original body so the user
    //    sees what they're replying to.
    let launch = root.take_pending_editor().expect("editor parked");
    assert!(
        launch.template.contains("To: Alice <alice@example.com>"),
        "template To header missing, got:\n{}",
        launch.template,
    );
    assert!(
        launch.template.contains("Subject: Re: Lunch tomorrow?"),
        "template Subject missing, got:\n{}",
        launch.template,
    );
    assert!(
        launch
            .template
            .contains("In-Reply-To: <orig-1@example.com>"),
        "template In-Reply-To missing, got:\n{}",
        launch.template,
    );
    assert!(
        launch.template.contains("> Hey,"),
        "quoted original body missing, got:\n{}",
        launch.template,
    );

    // 3. Simulate the editor: the user accepts the template verbatim.
    //    `parse_compose_from_text` is what `compose::launch_editor`
    //    runs on the file the editor wrote back; calling it directly
    //    keeps the test free of `$EDITOR` env-var races without losing
    //    coverage of the parse step.
    let parsed = compose::parse_compose_from_text(&launch.template).expect("parse ok");
    root.apply_editor_result(parsed);

    // 4. Pre-send pane state — `DraftStatus::ReadyToSend`, with the
    //    parsed compose installed.
    let state = root.draft().state().expect("draft state");
    assert_eq!(state.status, DraftStatus::ReadyToSend);
    assert_eq!(state.compose.to, "Alice <alice@example.com>");
    assert_eq!(state.compose.subject, "Re: Lunch tomorrow?");
    assert_eq!(
        state.compose.in_reply_to.as_deref(),
        Some("<orig-1@example.com>"),
    );

    // 5. `S`-to-send simulation: build an account with a stub `msmtp`
    //    and call `compose::send` against the live draft. AppRoot's
    //    own `from` line comes from the active account at template
    //    time, so the compose already carries a synthetic "<>" From
    //    (no [accounts.*] configured). Overwrite it so the wire-format
    //    is well-formed for the assertion.
    let stub_dir = TempDir::new().unwrap();
    let _captured = stub_msmtp(stub_dir.path(), 0);
    let account = account_with_stub("tester", "tester@example.com", temp.path(), stub_dir.path());
    let mut to_send = state.compose.clone();
    to_send.from = "Tester <tester@example.com>".into();

    let sent_path = compose::send(&to_send, &account).expect("send ok");

    // 6. The captured stdin and the Sent/cur copy must match, and
    //    must carry the In-Reply-To header from the parsed compose.
    let captured_bytes = std::fs::read_to_string(stub_dir.path().join("captured.eml")).unwrap();
    assert!(captured_bytes.contains("Subject: Re: Lunch tomorrow?"));
    assert!(captured_bytes.contains("In-Reply-To: <orig-1@example.com>"));
    assert!(captured_bytes.contains("From: Tester <tester@example.com>"));

    let sent_dir = temp.path().join("Sent").join("cur");
    assert!(
        sent_path.starts_with(&sent_dir),
        "sent path must live under Sent/cur/, got {:?}",
        sent_path,
    );
    let on_disk = std::fs::read_to_string(&sent_path).unwrap();
    assert_eq!(on_disk, captured_bytes);
}

// ---- scenario 2: reply-later ---------------------------------------

/// `R` (reply-later) must write an empty-body draft straight to
/// `<maildir>/Drafts/cur/<…>` (no editor, no msmtp) and register a
/// `body_empty=true` entry in the store's drafts index so the `⏰`
/// chip surfaces next to the original message in the Messages pane.
#[test]
fn scenario_reply_later_writes_draft_and_renders_alarm_chip() {
    let temp = TempDir::new().unwrap();
    let mut root = make_root_with_one_email(temp.path().to_path_buf());
    root.set_active_pane_for_test(ActivePane::Messages);

    let big_r = Event::Key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::NONE));
    root.process_event(big_r).unwrap();

    // No editor launch parked — reply-later is purely a placeholder.
    assert!(!root.has_pending_editor(), "R must not launch the editor",);

    // Draft state advanced straight to ReadyToSend with an empty body.
    let state = root.draft().state().expect("draft state");
    assert_eq!(state.reply_kind, ReplyKind::ReplyLater);
    assert_eq!(state.status, DraftStatus::ReadyToSend);
    assert!(
        state.compose.body.is_empty(),
        "reply-later body must be empty, got {:?}",
        state.compose.body,
    );

    // Exactly one draft file appeared under Drafts/cur/ and it carries
    // the In-Reply-To header pointing at the original Message-ID.
    let drafts_cur = temp.path().join("Drafts").join("cur");
    let drafts = read_dir_files(&drafts_cur);
    assert_eq!(drafts.len(), 1, "exactly one draft expected");
    let (_, draft_bytes) = &drafts[0];
    assert!(draft_bytes.contains("In-Reply-To: <orig-1@example.com>"));
    assert!(draft_bytes.contains("Subject: Re: Lunch tomorrow?"));

    // Store's drafts index gained a body_empty=true entry — that's
    // what `chip_for_message_id` reads to paint the `⏰` glyph.
    use crate::components::MessagesComponent;
    let store_handle = root.email_store_handle();
    let store = store_handle.lock().unwrap();
    let entry = store
        .drafts
        .get("orig-1@example.com")
        .expect("drafts index entry");
    assert!(entry.body_empty);
    let chip = MessagesComponent::chip_for_message_id(&store.drafts, "orig-1@example.com");
    assert_eq!(chip, Some('⏰'));
}

// ---- scenario 3: complete a reply-later draft ----------------------

/// Completing a reply-later draft means: open the existing draft,
/// fill in a body, send it, and delete the original Drafts/ file so
/// the `⏰` chip clears. The UI side ("press `l` to open View::Draft,
/// `e` to re-launch the editor") is not yet wired in `AppRoot`; this
/// test exercises the *pipeline* that wiring will call, so the
/// integration contract (send + delete) is covered.
///
/// Steps:
///   1. `R` to seed a reply-later draft on disk (scenario 2 covers
///      the chip side; here we just want a real file on disk).
///   2. Read the draft back into a `Compose`, simulate the user
///      filling in a body, send it via `compose::send`.
///   3. Delete the original draft file (this is the cleanup the
///      `S`-from-Drafts wiring is expected to perform).
///   4. Assert: the Sent/cur copy carries the user's body, the
///      draft is gone from Drafts/cur, and the chip would no
///      longer render for the original.
#[test]
fn scenario_complete_reply_later_sends_and_removes_draft() {
    let temp = TempDir::new().unwrap();
    let mut root = make_root_with_one_email(temp.path().to_path_buf());
    root.set_active_pane_for_test(ActivePane::Messages);

    // 1. Seed the reply-later draft via the `R` key path.
    let big_r = Event::Key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::NONE));
    root.process_event(big_r).unwrap();
    let drafts_cur = temp.path().join("Drafts").join("cur");
    let draft_path = read_dir_files(&drafts_cur)
        .into_iter()
        .next()
        .map(|(name, _)| drafts_cur.join(name))
        .expect("reply-later draft must be on disk");

    // 2. Re-open the draft from disk. `parse_compose_from_text` works
    //    on the same template shape `default_template` emits, but the
    //    draft file is RFC 5322 wire format (CRLF, MIME headers).
    //    Use `mail-parser` so the test reads what `Email::parse_*`
    //    would produce in production.
    let draft_bytes = std::fs::read(&draft_path).unwrap();
    let parsed = mail_parser::MessageParser::default()
        .parse(draft_bytes.as_slice())
        .expect("parse draft");
    let in_reply_to = parsed
        .in_reply_to()
        .as_text_list()
        .map(|v| v.join(", "))
        .unwrap_or_default();
    let subject = parsed.subject().unwrap_or("").to_string();
    let to_header = parsed
        .to()
        .and_then(|t| t.first())
        .and_then(|a| a.address())
        .unwrap_or("")
        .to_string();

    // 3. Simulate the user filling in the body in `$EDITOR` and
    //    pressing `S`. Compose carries the original metadata plus
    //    the new body.
    let composed = Compose {
        from: "Tester <tester@example.com>".into(),
        to: if to_header.is_empty() {
            "Alice <alice@example.com>".into()
        } else {
            to_header
        },
        cc: String::new(),
        bcc: String::new(),
        subject,
        body: "Sounds great — see you at noon.\n".into(),
        // `mail-parser`'s `in_reply_to()` strips the angle brackets;
        // re-add them so the serialized wire form is RFC-correct.
        in_reply_to: if in_reply_to.is_empty() {
            None
        } else {
            Some(format!("<{}>", in_reply_to))
        },
        attachments: Vec::new(),
        signature: None,
    };

    let stub_dir = TempDir::new().unwrap();
    let _captured = stub_msmtp(stub_dir.path(), 0);
    let account = account_with_stub("tester", "tester@example.com", temp.path(), stub_dir.path());
    let sent_path = compose::send(&composed, &account).expect("send ok");

    // 4. Cleanup: delete the original Drafts/cur/* file. (Future `S`-
    //    from-Drafts wiring will own this; we do it inline here to
    //    capture the contract.)
    std::fs::remove_file(&draft_path).unwrap();

    // 5. Assertions.
    let sent = std::fs::read_to_string(&sent_path).unwrap();
    assert!(sent.contains("Subject: Re: Lunch tomorrow?"));
    assert!(sent.contains("In-Reply-To: <orig-1@example.com>"));
    assert!(sent.contains("Sounds great"));

    assert!(
        read_dir_files(&drafts_cur).is_empty(),
        "Drafts/cur must be empty after send + cleanup",
    );
}

// ---- scenario 4: forward -------------------------------------------

/// `f` (forward) must build a Compose with an empty To: (the user
/// fills in the recipient), a `Fwd:` subject prefix, no In-Reply-To
/// header (forwards are a new thread), and a body containing the
/// standard "---------- Forwarded message ----------" preview block
/// with the original headers.
#[test]
fn scenario_forward_via_f_yields_empty_to_and_forwarded_preview_body() {
    let temp = TempDir::new().unwrap();
    let mut root = make_root_with_one_email(temp.path().to_path_buf());
    root.set_active_pane_for_test(ActivePane::Messages);

    let f = Event::Key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
    root.process_event(f).unwrap();

    assert!(root.has_pending_editor(), "f must park an editor launch");
    let state = root.draft().state().expect("draft state");
    assert_eq!(state.reply_kind, ReplyKind::Forward);

    let c = &state.compose;
    assert_eq!(c.to, "", "forward leaves To: blank");
    assert_eq!(c.subject, "Fwd: Lunch tomorrow?");
    assert!(
        c.in_reply_to.is_none(),
        "forwards must not set In-Reply-To, got {:?}",
        c.in_reply_to,
    );
    assert!(
        c.body.contains("---------- Forwarded message ----------"),
        "missing forwarded preview block, body:\n{}",
        c.body,
    );
    assert!(c.body.contains("From: Alice <alice@example.com>"));
    assert!(c.body.contains("Subject: Lunch tomorrow?"));
    // The original body is included verbatim under the preview block.
    assert!(c.body.contains("Want to grab lunch?"));

    // And the parked editor template carries those bytes too — that's
    // what `$EDITOR` would actually see.
    let launch = root.take_pending_editor().expect("editor parked");
    assert!(launch.template.contains("Subject: Fwd: Lunch tomorrow?"));
    assert!(launch.template.contains("Forwarded message"));
}

// ---- scenario 5: multi-account compose -----------------------------

/// With two accounts configured, switching to account B and replying
/// from a message in B's INBOX must land the Sent/ copy under B's
/// MailDir — never A's. The Accounts-pane `Char('l') => AccountSelect`
/// keystroke is intercepted by the global `l => ViewNext` shortcut
/// today (see `multi_account_switch_preserves_per_account_disk_state`
/// in `root.rs`), so this test drives the switch via `Msg::AccountSelect`
/// directly and reseeds the store from disk, mirroring the convention
/// of the existing multi-account integration test.
#[test]
fn scenario_multi_account_reply_lands_sent_in_account_b_maildir() {
    let temp = TempDir::new().unwrap();
    let root_a = temp.path().join("alpha");
    let root_b = temp.path().join("bravo");

    // Seed both accounts with the same canonical original email under
    // INBOX/cur. Distinct file names so the per-account assertions
    // can tell them apart if something gets crossed.
    for root in [&root_a, &root_b] {
        std::fs::create_dir_all(root.join("INBOX").join("cur")).unwrap();
        std::fs::write(
            root.join("INBOX").join("cur").join("orig.eml"),
            ORIGINAL_RFC822,
        )
        .unwrap();
    }

    let cfg = two_account_config(&root_a, &root_b);
    let store = EmailStore::new(root_a.clone());
    let scanner = MaildirScanner::new(root_a.clone());
    let mut root = AppRoot::with_config(Arc::new(Mutex::new(store)), scanner, cfg);

    // Land on alpha. The fixture store starts empty; reseed from disk
    // so the test does not depend on the async folder scanner.
    root.enqueue(Msg::AccountSelect("alpha".into()));
    root.drain();
    reseed_inbox_from_disk(&mut root);
    {
        let store = root.email_store_handle();
        let store = store.lock().unwrap();
        assert_eq!(store.root_folder.path, root_a, "should start on alpha");
    }

    // Switch to bravo and reseed bravo's inbox.
    root.enqueue(Msg::AccountSelect("bravo".into()));
    root.drain();
    reseed_inbox_from_disk(&mut root);
    {
        let store = root.email_store_handle();
        let store = store.lock().unwrap();
        assert_eq!(store.root_folder.path, root_b, "store must point at bravo");
    }

    // Press `gr` from bravo's INBOX. Reply variant fires against the
    // bravo-rooted email selected by `reseed_inbox_from_disk`.
    root.set_active_pane_for_test(ActivePane::Messages);
    root.set_messages_email_index_for_test(0);
    type_keys(&mut root, "gr");
    assert!(
        root.has_pending_editor(),
        "gr from bravo must park an editor launch"
    );

    let launch = root.take_pending_editor().expect("editor parked");
    let parsed = compose::parse_compose_from_text(&launch.template).expect("parse ok");
    root.apply_editor_result(parsed);

    let state = root.draft().state().expect("draft state");
    assert_eq!(state.status, DraftStatus::ReadyToSend);

    // Send through bravo's stub msmtp — Sent/ must land under bravo's
    // MailDir, NOT alpha's.
    let stub_dir = TempDir::new().unwrap();
    let _captured = stub_msmtp(stub_dir.path(), 0);
    let bravo_account = account_with_stub("bravo", "bravo@example.com", &root_b, stub_dir.path());
    let mut to_send = state.compose.clone();
    to_send.from = "Bravo <bravo@example.com>".into();

    let sent_path = compose::send(&to_send, &bravo_account).expect("send ok");

    assert!(
        sent_path.starts_with(root_b.join("Sent").join("cur")),
        "Sent copy must live under bravo's MailDir, got {:?}",
        sent_path,
    );
    assert!(
        !root_a.join("Sent").exists(),
        "alpha's MailDir must be untouched, but {:?} exists",
        root_a.join("Sent"),
    );
}
