// `SearchComponent` — notmuch query input modal.
//
// A thin bottom-of-screen input strip. While `visible == true`,
// AppRoot routes every key event here first; the modal absorbs all
// keys until it emits `Msg::SearchExecute` (Enter) or
// `Msg::SearchCancel` (Esc).
//
// State:
//   - `visible`        modal currently shown
//   - `query`          typed query text
//
// The modal renders via `render_modal` (called by `ui::UI::draw` only
// when `visible`). The bare-trait `render` is a no-op because the
// modal overlays the existing pane layout.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    widgets::{Block, Borders, Clear, Paragraph},
};

use crate::theme::Theme;

use super::{Component, Ctx, Msg};

/// Search input modal state. Absorbs every key event while
/// `visible == true`.
pub struct SearchComponent {
    /// True while the modal is shown.
    pub visible: bool,
    /// Query text typed by the user. Submitted verbatim to `notmuch`.
    pub query: String,
}

impl SearchComponent {
    /// Build a closed modal. Stays invisible until [`Self::open`] is
    /// called via `Msg::OpenSearchInput`.
    pub fn new() -> Self {
        Self {
            visible: false,
            query: String::new(),
        }
    }

    /// Show the modal with a fresh, empty query.
    pub fn open(&mut self) {
        self.visible = true;
        self.query.clear();
    }

    /// Hide the modal and drop the typed query.
    pub fn close(&mut self) {
        self.visible = false;
        self.query.clear();
    }

    /// Draw the bottom-of-screen modal overlay. No-op when
    /// `!self.visible`.
    pub fn render_modal(&self, f: &mut Frame, screen: Rect, theme: &Theme) {
        if !self.visible {
            return;
        }
        // Single-line input strip just above the status bar. Three rows
        // total: top + bottom border + one row of text.
        let height: u16 = 3;
        let y = screen.y + screen.height.saturating_sub(height + 1);
        let area = Rect {
            x: screen.x,
            y,
            width: screen.width,
            height,
        };
        f.render_widget(Clear, area);
        let block = Block::default()
            .borders(Borders::ALL)
            .title("Search (notmuch) — Enter to run, Esc to cancel")
            .style(Style::default().fg(theme.cyan));
        let para = Paragraph::new(format!("/{}", self.query)).block(block);
        f.render_widget(para, area);
    }
}

impl Default for SearchComponent {
    fn default() -> Self {
        Self::new()
    }
}

impl Component for SearchComponent {
    fn handle_msg(&mut self, msg: &Msg, _ctx: &Ctx) -> Vec<Msg> {
        match msg {
            Msg::OpenSearchInput => {
                self.open();
            }
            Msg::SearchExecute(_) | Msg::SearchCancel => {
                self.close();
            }
            _ => {}
        }
        Vec::new()
    }

    fn render(&self, _f: &mut Frame, _area: Rect, _focused: bool, _ctx: &Ctx) {
        // Modal renders via `render_modal` from `ui::UI::draw`. The
        // bare-trait impl is a no-op so this component doesn't fight
        // with the pane layout.
    }

    fn on_key(&mut self, key: KeyEvent, _ctx: &Ctx) -> Option<Msg> {
        if !self.visible {
            return None;
        }
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => Some(Msg::SearchCancel),
            (KeyCode::Enter, _) => {
                let q = self.query.trim().to_string();
                if q.is_empty() {
                    Some(Msg::SearchCancel)
                } else {
                    Some(Msg::SearchExecute(q))
                }
            }
            (KeyCode::Backspace, _) => {
                self.query.pop();
                None
            }
            (KeyCode::Char(c), m) if m.is_empty() || m == KeyModifiers::SHIFT => {
                self.query.push(c);
                None
            }
            _ => None,
        }
    }
}

/// Parse stdout from `notmuch search --output=files <query>`. One
/// path per line; blank lines and trailing whitespace are skipped.
/// Pulled out as a free function so the parse can be unit-tested
/// against canned output without invoking `notmuch`.
pub fn parse_notmuch_files_output(stdout: &str) -> Vec<std::path::PathBuf> {
    stdout
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(std::path::PathBuf::from)
        .collect()
}

/// True when the `notmuch` binary is reachable on `PATH`. Implemented
/// by running `notmuch --version` and treating an `io::ErrorKind::
/// NotFound` spawn failure as "missing". A non-zero exit with the
/// binary present still counts as "available" — the diagnostic is
/// surfaced later by `SearchExecute`.
pub fn notmuch_available() -> bool {
    match std::process::Command::new("notmuch")
        .arg("--version")
        .output()
    {
        Ok(_) => true,
        Err(e) => e.kind() != std::io::ErrorKind::NotFound,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::email::EmailStore;
    use crate::theme::Theme;
    use std::path::PathBuf;

    fn ctx_fixture() -> (Config, EmailStore, Theme) {
        (
            Config::default(),
            EmailStore::new(PathBuf::from("/tmp")),
            Theme::default(),
        )
    }

    #[test]
    fn open_starts_with_empty_query() {
        let mut s = SearchComponent::new();
        assert!(!s.visible);
        s.open();
        assert!(s.visible);
        assert_eq!(s.query, "");
    }

    #[test]
    fn close_clears_state() {
        let mut s = SearchComponent::new();
        s.open();
        s.query = "tag:inbox".into();
        s.close();
        assert!(!s.visible);
        assert!(s.query.is_empty());
    }

    #[test]
    fn handle_msg_open_shows_modal() {
        let (config, store, theme) = ctx_fixture();
        let ctx = Ctx {
            theme: &theme,
            config: &config,
            store: &store,
        };
        let mut s = SearchComponent::new();
        s.handle_msg(&Msg::OpenSearchInput, &ctx);
        assert!(s.visible);
    }

    #[test]
    fn handle_msg_execute_closes_modal() {
        let (config, store, theme) = ctx_fixture();
        let ctx = Ctx {
            theme: &theme,
            config: &config,
            store: &store,
        };
        let mut s = SearchComponent::new();
        s.handle_msg(&Msg::OpenSearchInput, &ctx);
        s.handle_msg(&Msg::SearchExecute("tag:inbox".into()), &ctx);
        assert!(!s.visible);
    }

    #[test]
    fn handle_msg_cancel_closes_modal() {
        let (config, store, theme) = ctx_fixture();
        let ctx = Ctx {
            theme: &theme,
            config: &config,
            store: &store,
        };
        let mut s = SearchComponent::new();
        s.handle_msg(&Msg::OpenSearchInput, &ctx);
        s.handle_msg(&Msg::SearchCancel, &ctx);
        assert!(!s.visible);
    }

    #[test]
    fn on_key_typing_appends_to_query() {
        let (config, store, theme) = ctx_fixture();
        let ctx = Ctx {
            theme: &theme,
            config: &config,
            store: &store,
        };
        let mut s = SearchComponent::new();
        s.open();
        for c in "tag:inbox".chars() {
            let key = KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
            assert!(s.on_key(key, &ctx).is_none());
        }
        assert_eq!(s.query, "tag:inbox");
    }

    #[test]
    fn backspace_pops_query_char() {
        let (config, store, theme) = ctx_fixture();
        let ctx = Ctx {
            theme: &theme,
            config: &config,
            store: &store,
        };
        let mut s = SearchComponent::new();
        s.open();
        s.query = "abc".into();
        let bksp = KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE);
        s.on_key(bksp, &ctx);
        assert_eq!(s.query, "ab");
    }

    #[test]
    fn enter_emits_search_execute_with_trimmed_query() {
        let (config, store, theme) = ctx_fixture();
        let ctx = Ctx {
            theme: &theme,
            config: &config,
            store: &store,
        };
        let mut s = SearchComponent::new();
        s.open();
        s.query = "  tag:inbox  ".into();
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let msg = s.on_key(enter, &ctx);
        assert_eq!(msg, Some(Msg::SearchExecute("tag:inbox".into())));
    }

    #[test]
    fn enter_with_empty_query_cancels() {
        let (config, store, theme) = ctx_fixture();
        let ctx = Ctx {
            theme: &theme,
            config: &config,
            store: &store,
        };
        let mut s = SearchComponent::new();
        s.open();
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let msg = s.on_key(enter, &ctx);
        assert_eq!(msg, Some(Msg::SearchCancel));
    }

    #[test]
    fn esc_emits_search_cancel() {
        let (config, store, theme) = ctx_fixture();
        let ctx = Ctx {
            theme: &theme,
            config: &config,
            store: &store,
        };
        let mut s = SearchComponent::new();
        s.open();
        s.query = "foo".into();
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        let msg = s.on_key(esc, &ctx);
        assert_eq!(msg, Some(Msg::SearchCancel));
    }

    #[test]
    fn on_key_returns_none_when_invisible() {
        let (config, store, theme) = ctx_fixture();
        let ctx = Ctx {
            theme: &theme,
            config: &config,
            store: &store,
        };
        let mut s = SearchComponent::new();
        // Not visible: every key falls through.
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert!(s.on_key(enter, &ctx).is_none());
        let c = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        assert!(s.on_key(c, &ctx).is_none());
    }

    #[test]
    fn parse_notmuch_output_extracts_paths() {
        let stdout = "/home/u/Mail/INBOX/cur/1.eml\n/home/u/Mail/Sent/cur/2.eml\n";
        let got = parse_notmuch_files_output(stdout);
        assert_eq!(
            got,
            vec![
                PathBuf::from("/home/u/Mail/INBOX/cur/1.eml"),
                PathBuf::from("/home/u/Mail/Sent/cur/2.eml"),
            ]
        );
    }

    #[test]
    fn parse_notmuch_output_skips_blank_lines() {
        let stdout = "\n  /tmp/a.eml\n\n/tmp/b.eml\n";
        let got = parse_notmuch_files_output(stdout);
        assert_eq!(
            got,
            vec![PathBuf::from("/tmp/a.eml"), PathBuf::from("/tmp/b.eml")]
        );
    }

    #[test]
    fn parse_notmuch_output_handles_empty() {
        assert!(parse_notmuch_files_output("").is_empty());
        assert!(parse_notmuch_files_output("\n\n   \n").is_empty());
    }

    #[test]
    fn notmuch_available_reports_missing_binary_as_false() {
        // The not-installed fallback path: when the binary is missing,
        // we report `false` so callers can show the "notmuch not found"
        // status. We can't poison PATH for this process, so the test
        // instead exercises the same logic against a guaranteed-missing
        // command name to confirm the NotFound branch.
        fn probe(bin: &str) -> bool {
            match std::process::Command::new(bin).arg("--version").output() {
                Ok(_) => true,
                Err(e) => e.kind() != std::io::ErrorKind::NotFound,
            }
        }
        assert!(!probe("vulthor-notmuch-not-a-real-binary-zzzz"));
    }
}
