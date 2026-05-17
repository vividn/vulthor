//! Ratatui snapshot tests for the major panes (vu-ba9).
//!
//! Each test builds a fixture state, renders one component into a
//! `TestBackend`, and asserts the symbol-grid output matches a saved
//! snapshot under `tests/snapshots/`. Style (fg/bg/modifier) is not
//! captured — these are layout / text regressions only.
//!
//! On first run insta writes `.snap.new` files and the test fails;
//! accept with `cargo insta accept` (or set `INSTA_UPDATE=always` to
//! auto-write). Subsequent runs assert byte-equality.

use std::collections::HashMap;
use std::path::PathBuf;

use insta::assert_snapshot;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use vulthor::components::{
    Component, ContentComponent, Ctx, DraftComponent, FoldersComponent, MessageId,
    MessagesComponent, Msg, ReplyKind,
};
use vulthor::compose::Compose;
use vulthor::config::Config;
use vulthor::email::{Email, EmailLoadState, EmailStore, Folder};
use vulthor::sanitizer::sanitize_email_html;
use vulthor::theme::Theme;

/// Draw `f` into a (w × h) `TestBackend` and return the symbol grid
/// as a newline-joined string. Mirrors the helper used inside
/// `components/draft.rs` tests so the snapshot output matches what a
/// developer running the live TUI at this size would see.
fn render_to_string(w: u16, h: u16, f: impl FnOnce(&mut ratatui::Frame)) -> String {
    let backend = TestBackend::new(w, h);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal.draw(f).expect("draw");
    let buf = terminal.backend().buffer();
    let mut out = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

/// Build an `EmailStore` whose root carries the named subfolders, each
/// flagged `is_loaded` so the folder pane does not paint the
/// "Scanning folders…" splash.
fn store_with_folders(names: &[&str]) -> EmailStore {
    let mut store = EmailStore::new(PathBuf::from("/snapshot/maildir"));
    for name in names {
        let mut folder = Folder::new(
            (*name).to_string(),
            PathBuf::from(format!("/snapshot/maildir/{}", name)),
        );
        folder.is_loaded = true;
        store.root_folder.add_subfolder(folder);
    }
    store
}

/// Construct an `Email` populated with deterministic headers so the
/// row formatting in `MessagesComponent` is stable across hosts (no
/// `Local::now()` dependency — RFC-3339 with explicit offset).
fn fixture_email(file: &str, from: &str, subject: &str, date_rfc3339: &str, unread: bool) -> Email {
    let mut email = Email::new(PathBuf::from(file));
    email.headers.from = from.to_string();
    email.headers.to = "user@example.com".to_string();
    email.headers.subject = subject.to_string();
    email.headers.date = date_rfc3339.to_string();
    email.headers.message_id = format!("<{}@snapshot.test>", subject.replace(' ', "-"));
    email.is_unread = unread;
    email
}

#[test]
fn render_folders_snapshot() {
    let store = store_with_folders(&["INBOX", "Sent", "Drafts"]);
    let theme = Theme::default();
    let config = Config::default();
    let comp = FoldersComponent::with_index(0);

    let rendered = render_to_string(40, 10, |frame| {
        let ctx = Ctx {
            theme: &theme,
            config: &config,
            store: &store,
        };
        comp.render(frame, frame.area(), true, &ctx);
    });
    assert_snapshot!(rendered);
}

#[test]
fn render_messages_snapshot() {
    let mut folder = Folder::new("INBOX".to_string(), PathBuf::from("/snapshot/INBOX"));
    folder.is_loaded = true;
    // Fixed dates (year 2024) so the day-vs-time branch in
    // `MessagesComponent::format_email_date` always renders the date
    // form regardless of the host's current clock.
    let rows = [
        (
            "alice@example.com",
            "Welcome aboard",
            "2024-01-02T09:15:00+00:00",
            true,
        ),
        (
            "bob@example.com",
            "Re: project status",
            "2024-01-03T10:20:00+00:00",
            false,
        ),
        (
            "carol@example.com",
            "Lunch tomorrow?",
            "2024-01-04T11:25:00+00:00",
            true,
        ),
        (
            "dave@example.com",
            "Q1 roadmap draft",
            "2024-01-05T12:30:00+00:00",
            false,
        ),
        (
            "eve@example.com",
            "Weekly digest",
            "2024-01-06T13:35:00+00:00",
            false,
        ),
    ];
    for (i, (from, subject, date, unread)) in rows.iter().enumerate() {
        let path = format!("/snapshot/INBOX/cur/email{}", i + 1);
        folder.add_email(fixture_email(&path, from, subject, date, *unread));
    }

    let drafts: HashMap<String, vulthor::email::DraftInfo> = HashMap::new();
    let theme = Theme::default();
    let mut comp = MessagesComponent::new();
    // Focused row = 2nd (index 1) per acceptance criteria.
    comp.email_index = 1;

    let rendered = render_to_string(80, 10, |frame| {
        comp.render_with_folder(
            frame,
            frame.area(),
            true,
            &folder,
            "Mail > INBOX",
            &drafts,
            &theme,
        );
    });
    assert_snapshot!(rendered);
}

#[test]
fn render_content_snapshot() {
    // Build an HTML email and run the body through the real sanitizer so
    // the snapshot pins both the rendered pane and the blocked-image
    // pipeline. Remote `<img src>` is rewritten to a 1×1 placeholder by
    // `sanitize_email_html`; the Content pane shows the plain-text
    // alternative, but we still want the sanitizer invocation in the
    // snapshot's setup as a regression anchor.
    let raw_html = r#"<p>Hello <b>world</b>.</p><img src="http://tracker.example.com/pixel.gif"/>"#;
    let sanitized = sanitize_email_html(raw_html);
    assert!(
        sanitized.contains("data:image/gif;base64,"),
        "sanitizer must replace remote <img src> with inline placeholder: {sanitized}"
    );

    let mut email = fixture_email(
        "/snapshot/INBOX/cur/html_email",
        "Newsletter <news@example.com>",
        "Your weekly update",
        "2024-01-07T08:00:00+00:00",
        true,
    );
    email.body_text = "Hello world.\n\nClick here for details.".to_string();
    email.body_html = Some(sanitized);
    email.load_state = EmailLoadState::FullyLoaded;

    let mut folder = Folder::new("INBOX".to_string(), PathBuf::from("/snapshot/INBOX"));
    folder.is_loaded = true;
    folder.add_email(email);

    let mut store = EmailStore::new(PathBuf::from("/snapshot/maildir"));
    store.root_folder.add_subfolder(folder);
    store.current_folder = vec![0];
    store.select_email(0);

    let theme = Theme::default();
    let config = Config::default();
    let comp = ContentComponent::new();

    let rendered = render_to_string(60, 14, |frame| {
        let ctx = Ctx {
            theme: &theme,
            config: &config,
            store: &store,
        };
        comp.render(frame, frame.area(), true, &ctx);
    });
    assert_snapshot!(rendered);
}

#[test]
fn render_draft_snapshot() {
    let store = EmailStore::new(PathBuf::from("/snapshot/maildir"));
    let theme = Theme::default();
    let config = Config::default();
    let ctx = Ctx {
        theme: &theme,
        config: &config,
        store: &store,
    };

    let mut comp = DraftComponent::new();
    comp.handle_msg(
        &Msg::DraftStart(ReplyKind::Reply, MessageId::from("orig-msg-1")),
        &ctx,
    );
    comp.set_compose(Compose {
        from: "me@example.com".into(),
        to: "alice@example.com".into(),
        subject: "Re: project status".into(),
        body: "Thanks for the update — looks good.\n\nMore comments inline.\n".into(),
        ..Compose::new()
    });
    comp.handle_msg(&Msg::DraftEditorExited, &ctx);

    let rendered = render_to_string(60, 10, |frame| {
        let ctx = Ctx {
            theme: &theme,
            config: &config,
            store: &store,
        };
        comp.render(frame, frame.area(), true, &ctx);
    });
    assert_snapshot!(rendered);
}
