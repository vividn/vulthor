// `AccountsComponent` — multi-account pane.
//
// Owns the Accounts pane's cursor (`selected_index`), a snapshot of
// the `[accounts.*]` table loaded from `vulthor.toml`, and per-account
// unread counts. Translates Accounts-pane keys into messages.
//
// **State source.** The accounts list is seeded once at construction
// from `Config::ordered_accounts()`. The component does not read
// `Ctx::config` during render — that would let a stale config drift
// the cursor against the rendered list. Re-seeding (e.g. when a
// future config-reload feature lands) is an explicit caller action.
//
// **Why a `Vec` and not a `BTreeMap`.** Selection-by-index dominates
// the UX surface (j/k, render highlight, `current_account_id`).
// `Config` keeps the map; the component flattens it once.
//
// **Single-account installs.** When the config has 0 or 1 accounts,
// VISION.md § "Multi-Account" requires the pane to be hidden. That
// policy is enforced by `AppRoot`'s `ViewPrev` handler refusing to
// surface `View::AccountsFolders`; this component happily renders a
// single row when asked.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};
use std::cell::RefCell;

use crate::config::{AccountConfig, Config};
use crate::theme::VulthorTheme;

use super::{AccountId, Component, Ctx, Dir, Msg};

/// Title rendered on the Accounts pane's bordered block. Tests grep
/// for this exact string; runtime renders it too.
pub const ACCOUNTS_TITLE: &str = "Accounts";

/// Body rendered when no `[accounts.*]` sections are configured —
/// e.g. an old single-account config that still uses the top-level
/// `maildir_path`. Tests grep for this exact string.
pub const ACCOUNTS_EMPTY_BODY: &str = "No accounts configured";

#[derive(Debug, Clone)]
struct AccountRow {
    id: AccountId,
    account: AccountConfig,
    /// Reserved for a later loader; today every row reports 0 because
    /// the store only knows the active account's folders. Rendered
    /// as `Name (n)` only when non-zero so non-active accounts read
    /// plainly.
    unread: usize,
}

/// Accounts pane state. Owns its own cursor and a snapshot of the
/// configured accounts taken at construction. See module docs for the
/// "no live `Ctx::config` reads in render" invariant.
pub struct AccountsComponent {
    accounts: Vec<AccountRow>,
    selected_index: usize,
    list_state: RefCell<ListState>,
}

impl Default for AccountsComponent {
    fn default() -> Self {
        Self::new()
    }
}

impl AccountsComponent {
    /// Construct an empty component. The runtime calls
    /// [`with_config`](Self::with_config) instead, but tests that
    /// don't care about accounts use this.
    pub fn new() -> Self {
        Self {
            accounts: Vec::new(),
            selected_index: 0,
            list_state: RefCell::new(ListState::default()),
        }
    }

    /// Build a component seeded from `Config`. The initial cursor
    /// follows `Config::default_account_index()` so the highlighted
    /// row matches the account whose maildir is loaded into the store.
    pub fn with_config(config: &Config) -> Self {
        let accounts: Vec<AccountRow> = config
            .ordered_accounts()
            .into_iter()
            .map(|(id, account)| AccountRow {
                id,
                account,
                unread: 0,
            })
            .collect();
        let selected_index = config.default_account_index().unwrap_or(0);
        let mut state = ListState::default();
        if !accounts.is_empty() {
            state.select(Some(selected_index));
        }
        Self {
            accounts,
            selected_index,
            list_state: RefCell::new(state),
        }
    }

    /// Number of accounts seeded from config. AppRoot uses this to
    /// gate the multi-account-only `View::AccountsFolders` transition.
    pub fn account_count(&self) -> usize {
        self.accounts.len()
    }

    /// Current cursor position. Zero-based index into the seeded
    /// accounts list.
    pub fn selected_index(&self) -> usize {
        self.selected_index
    }

    /// Account id at the cursor — `None` only when no accounts are
    /// configured. Used by `on_key` to populate `Msg::AccountSelect`.
    pub fn current_account_id(&self) -> Option<AccountId> {
        self.accounts.get(self.selected_index).map(|r| r.id.clone())
    }

    /// Resolve an account id to its config payload. `AppRoot` uses
    /// this to look up the maildir_path when handling
    /// `Msg::AccountSelect`.
    pub fn account_by_id(&self, id: &str) -> Option<&AccountConfig> {
        self.accounts
            .iter()
            .find(|r| r.id == id)
            .map(|r| &r.account)
    }

    fn highlight_row(&self, row: &AccountRow) -> Line<'static> {
        let mut spans = vec![Span::raw(row.account.name.clone())];
        if row.unread > 0 {
            spans.push(Span::raw(format!(" ({})", row.unread)));
        }
        Line::from(spans)
    }
}

impl Component for AccountsComponent {
    fn handle_msg(&mut self, msg: &Msg, _ctx: &Ctx) -> Vec<Msg> {
        match msg {
            Msg::AccountMove(Dir::Down) => {
                if !self.accounts.is_empty() && self.selected_index + 1 < self.accounts.len() {
                    self.selected_index += 1;
                }
            }
            Msg::AccountMove(Dir::Up) => {
                if self.selected_index > 0 {
                    self.selected_index -= 1;
                }
            }
            // `AccountSelect` is observed by AppRoot (which rebuilds
            // the store); the component itself only needs to mirror
            // the cursor to the chosen id so the highlight matches.
            Msg::AccountSelect(id) => {
                if let Some(idx) = self.accounts.iter().position(|r| r.id == *id) {
                    self.selected_index = idx;
                }
            }
            _ => {}
        }
        Vec::new()
    }

    fn render(&self, f: &mut Frame, area: Rect, focused: bool, _ctx: &Ctx) {
        let border_style = if focused {
            Style::default().fg(VulthorTheme::CYAN_LIGHT)
        } else {
            Style::default()
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .style(border_style)
            .title(ACCOUNTS_TITLE);

        if self.accounts.is_empty() {
            let body = Paragraph::new(Text::from(ACCOUNTS_EMPTY_BODY))
                .block(block)
                .style(Style::default().fg(VulthorTheme::GRAY_DARK))
                .wrap(Wrap { trim: true });
            f.render_widget(body, area);
            return;
        }

        let items: Vec<ListItem<'static>> = self
            .accounts
            .iter()
            .map(|row| ListItem::new(self.highlight_row(row)))
            .collect();
        let list = List::new(items).block(block).highlight_style(
            Style::default()
                .bg(VulthorTheme::SELECTION_BG)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        );
        let mut state = self.list_state.borrow_mut();
        state.select(Some(self.selected_index));
        f.render_stateful_widget(list, area, &mut *state);
    }

    fn on_key(&mut self, _key: KeyEvent, _ctx: &Ctx) -> Option<Msg> {
        // Every Accounts-pane key (`j`/`k`/`l`/Enter/arrows) resolves
        // through the central `AppRoot::action_to_msg` keymap dispatch.
        // For `l` and Enter, that path produces `Msg::AccountSelect`
        // with an empty-id sentinel; `AppRoot::apply_root` resolves the
        // sentinel to the cursor account via
        // `AccountsComponent::current_account_id`. Keeping this trait
        // method as a no-op satisfies the Component contract.
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::email::EmailStore;
    use crossterm::event::KeyModifiers;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::path::PathBuf;

    fn account(name: &str, mail_path: &str) -> AccountConfig {
        AccountConfig {
            name: name.to_string(),
            email: format!("{}@x.test", name.to_lowercase()),
            maildir_path: PathBuf::from(mail_path),
            smtp_command: None,
            signature: None,
        }
    }

    fn config_with(pairs: &[(&str, AccountConfig)]) -> Config {
        let mut cfg = Config::default();
        for (key, account) in pairs {
            cfg.accounts.insert(key.to_string(), account.clone());
        }
        cfg
    }

    fn fixtures() -> (VulthorTheme, EmailStore) {
        (VulthorTheme, EmailStore::new(PathBuf::from("/tmp")))
    }

    fn ctx<'a>(theme: &'a VulthorTheme, config: &'a Config, store: &'a EmailStore) -> Ctx<'a> {
        Ctx {
            theme,
            config,
            store,
        }
    }

    fn render_to_string(c: &AccountsComponent, focused: bool, cfg: &Config) -> String {
        let backend = TestBackend::new(30, 6);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let (theme, store) = fixtures();
        let ctx = ctx(&theme, cfg, &store);
        terminal
            .draw(|f| c.render(f, f.area(), focused, &ctx))
            .expect("draw");
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

    #[test]
    fn with_config_seeds_accounts_in_btreemap_order() {
        let cfg = config_with(&[
            ("work", account("Work", "/Mail/work")),
            ("personal", account("Personal", "/Mail/personal")),
        ]);
        let c = AccountsComponent::with_config(&cfg);
        assert_eq!(c.account_count(), 2);
        // BTreeMap → alphabetical: personal (p) before work (w).
        assert_eq!(c.current_account_id().as_deref(), Some("personal"));
    }

    #[test]
    fn with_config_honors_default_account() {
        let mut cfg = config_with(&[
            ("work", account("Work", "/Mail/work")),
            ("personal", account("Personal", "/Mail/personal")),
        ]);
        cfg.default_account = Some("work".into());
        let c = AccountsComponent::with_config(&cfg);
        assert_eq!(c.current_account_id().as_deref(), Some("work"));
    }

    #[test]
    fn account_move_down_clamps_at_last_row() {
        let cfg = config_with(&[("a", account("A", "/a")), ("b", account("B", "/b"))]);
        let mut c = AccountsComponent::with_config(&cfg);
        let (theme, store) = fixtures();
        let ctx = ctx(&theme, &cfg, &store);
        // Starts at 0 → Down moves to 1 → another Down clamps at 1.
        c.handle_msg(&Msg::AccountMove(Dir::Down), &ctx);
        assert_eq!(c.selected_index(), 1);
        c.handle_msg(&Msg::AccountMove(Dir::Down), &ctx);
        assert_eq!(c.selected_index(), 1);
    }

    #[test]
    fn account_move_up_clamps_at_zero() {
        let cfg = config_with(&[("a", account("A", "/a")), ("b", account("B", "/b"))]);
        let mut c = AccountsComponent::with_config(&cfg);
        let (theme, store) = fixtures();
        let ctx = ctx(&theme, &cfg, &store);
        // Already at top — Up is a no-op.
        c.handle_msg(&Msg::AccountMove(Dir::Up), &ctx);
        assert_eq!(c.selected_index(), 0);
    }

    #[test]
    fn account_select_msg_moves_cursor_to_named_account() {
        let cfg = config_with(&[
            ("a", account("A", "/a")),
            ("b", account("B", "/b")),
            ("c", account("C", "/c")),
        ]);
        let mut c = AccountsComponent::with_config(&cfg);
        let (theme, store) = fixtures();
        let ctx = ctx(&theme, &cfg, &store);
        c.handle_msg(&Msg::AccountSelect("c".into()), &ctx);
        assert_eq!(c.selected_index(), 2);
        assert_eq!(c.current_account_id().as_deref(), Some("c"));
    }

    // `j`/`k`, arrow `Up`/`Down`, Enter, and `l` all resolve via
    // `AppRoot::action_to_msg` (centralised keymap dispatch). The
    // sentinel-resolution and end-to-end account-switch behaviour are
    // covered by
    // `components::root::tests::l_on_accounts_pane_switches_account_end_to_end`
    // and the new keymap-dispatch tests in `phase4_integration_tests`.

    #[test]
    fn render_lists_each_account_name() {
        let cfg = config_with(&[
            ("alpha", account("Alpha", "/a")),
            ("bravo", account("Bravo", "/b")),
        ]);
        let c = AccountsComponent::with_config(&cfg);
        let rendered = render_to_string(&c, true, &cfg);
        assert!(
            rendered.contains("Alpha"),
            "expected Alpha row, got:\n{}",
            rendered
        );
        assert!(
            rendered.contains("Bravo"),
            "expected Bravo row, got:\n{}",
            rendered
        );
    }

    #[test]
    fn render_paints_title() {
        let cfg = Config::default();
        let c = AccountsComponent::with_config(&cfg);
        let rendered = render_to_string(&c, false, &cfg);
        assert!(rendered.contains(ACCOUNTS_TITLE));
    }

    #[test]
    fn render_shows_empty_message_when_no_accounts() {
        // VISION.md: single-account installs hide the pane, but the
        // policy lives in AppRoot. If a caller does render this
        // component with zero accounts (e.g. a legacy maildir_path-
        // only config opened deliberately), it must not panic — show
        // a polite empty body.
        let cfg = Config::default();
        let c = AccountsComponent::with_config(&cfg);
        let rendered = render_to_string(&c, true, &cfg);
        assert!(
            rendered.contains(ACCOUNTS_EMPTY_BODY),
            "expected empty body, got:\n{}",
            rendered
        );
    }

    #[test]
    fn account_by_id_resolves_to_config_payload() {
        let cfg = config_with(&[
            ("work", account("Work", "/Mail/work")),
            ("personal", account("Personal", "/Mail/personal")),
        ]);
        let c = AccountsComponent::with_config(&cfg);
        let work = c.account_by_id("work").expect("work account");
        assert_eq!(work.maildir_path, PathBuf::from("/Mail/work"));
        assert!(c.account_by_id("missing").is_none());
    }
}
