// `MessagesComponent` — second pane migration (Phase 0.2.3a, vu-3ko).
//
// Owns the message-pane email cursor (`email_index`), the
// remembered-cursor hand-off slot (`remembered_email_index`), and the
// last-rendered visible-row count (`visible_rows`). Translates Messages-
// pane keys into messages, restores/remembers selection across
// Folders↔Messages focus changes, and renders the email list.
//
// **Sole writer of `email_index`.** `AppRoot::apply_root` mirrors
// `email_index` into `app.selection.email_index` after each dispatch
// step so legacy readers in `ui.rs` (status bar, scroll-offset) and
// the web server keep working until ContentComponent lands.
//
// **`Cell<usize>` visible_rows / `RefCell<ListState>`.** Ratatui's
// `render_stateful_widget` requires `&mut ListState`, but
// `Component::render` takes `&self`. The component owns both as
// interior-mutable cells. `visible_rows` is read by `handle_msg` when
// `MessageMove(Down)` checks whether to emit `Msg::StoreLoadMore`; it
// is written exclusively by `render` from the live pane area.

use std::cell::{Cell, RefCell};

use chrono::{DateTime, Local};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState},
};
use unicode_width::UnicodeWidthStr;

use crate::email::{Email, Folder};
use crate::theme::VulthorTheme;

use super::{Component, Ctx, Dir, Msg};

/// How many rows past the visible tail we look ahead before asking the
/// store for more headers. Matches the legacy `index + 5 >= len` test
/// in `EmailStore::load_more_messages_if_needed`.
const SCROLL_LOOKAHEAD: usize = 5;

pub struct MessagesComponent {
    pub email_index: usize,
    pub remembered_email_index: Option<usize>,
    /// Number of email rows the pane could display at its last render.
    /// Set by `render_with_folder` from the pane area; read by AppRoot
    /// to size the headers-load chunk. Seeded to 20 (same default as
    /// the pre-refactor `App` field) so a component that has never
    /// rendered still gives a sensible answer — important for tests
    /// that drive `handle_msg` directly.
    pub visible_rows: Cell<usize>,
    list_state: RefCell<ListState>,
}

impl MessagesComponent {
    pub fn new() -> Self {
        Self {
            email_index: 0,
            remembered_email_index: None,
            visible_rows: Cell::new(20),
            list_state: RefCell::new(ListState::default()),
        }
    }

    /// Render the pane against an explicit folder reference. `AppRoot`
    /// (and tests) pick the right folder based on the active view —
    /// see `MessagesRenderContext`. We render against the resolved
    /// `&Folder` rather than re-deriving it from `Ctx::store` so the
    /// caller stays in charge of the view-vs-store decision.
    pub fn render_with_folder(
        &self,
        f: &mut Frame,
        area: Rect,
        focused: bool,
        folder_to_display: &Folder,
        folder_path: &str,
    ) {
        // Track the actual visible row count so `handle_msg(MessageMove)`
        // can emit `StoreLoadMore` ahead of the user reaching the tail.
        let rows = (area.height.saturating_sub(2)) as usize;
        self.visible_rows.set(rows);

        let is_sent_folder = folder_to_display.name == "Sent"
            || folder_to_display.name.to_lowercase().contains("sent");

        let email_items = Self::build_email_list_with_truncation(
            &folder_to_display.emails,
            area.width.saturating_sub(2) as usize,
            is_sent_folder,
        );

        let style = if focused {
            Style::default().fg(VulthorTheme::CYAN)
        } else {
            Style::default()
        };

        let title = if folder_to_display.is_loaded {
            format!(
                "Emails - {} ({})",
                folder_path,
                folder_to_display.emails.len()
            )
        } else if folder_to_display.emails.is_empty() {
            format!("Emails - {} (loading)", folder_path)
        } else {
            format!(
                "Emails - {} ({}/...)",
                folder_path,
                folder_to_display.emails.len()
            )
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .style(style)
            .title(title);

        let list = List::new(email_items)
            .block(block)
            .style(style)
            .highlight_style(
                Style::default()
                    .bg(VulthorTheme::SELECTION_BG)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            );

        let mut state = self.list_state.borrow_mut();
        state.select(Some(self.email_index));
        f.render_stateful_widget(list, area, &mut *state);
    }

    // --- Email-row helpers (extracted from the pre-refactor ui.rs) ---

    fn format_email_date(date_str: &str) -> String {
        if let Ok(date_time) = DateTime::parse_from_rfc3339(date_str) {
            let local_time = date_time.with_timezone(&Local);
            let today = Local::now().date_naive();
            if local_time.date_naive() == today {
                local_time.format("%H:%M").to_string()
            } else {
                local_time.format("%Y-%m-%d").to_string()
            }
        } else {
            date_str.chars().take(10).collect()
        }
    }

    fn truncate_with_ellipsis(text: &str, max_width: usize) -> String {
        let text_width = text.width();
        if text_width <= max_width {
            text.to_string()
        } else if max_width > 3 {
            let mut current_width = 0;
            let mut truncation_point = 0;
            for (idx, ch) in text.char_indices() {
                let ch_str = &text[idx..idx + ch.len_utf8()];
                let ch_width = ch_str.width();
                if current_width + ch_width > max_width - 3 {
                    break;
                }
                current_width += ch_width;
                truncation_point = idx + ch.len_utf8();
            }
            format!("{}...", &text[..truncation_point])
        } else {
            let mut current_width = 0;
            let mut truncation_point = 0;
            for (idx, ch) in text.char_indices() {
                let ch_str = &text[idx..idx + ch.len_utf8()];
                let ch_width = ch_str.width();
                if current_width + ch_width > max_width {
                    break;
                }
                current_width += ch_width;
                truncation_point = idx + ch.len_utf8();
            }
            text[..truncation_point].to_string()
        }
    }

    fn pad_to_width(text: &str, target_width: usize) -> String {
        let text_width = text.width();
        if text_width >= target_width {
            text.to_string()
        } else {
            let padding = target_width - text_width;
            format!("{}{}", text, " ".repeat(padding))
        }
    }

    fn extract_email_address(from_field: &str) -> String {
        if let Some(name_end) = from_field.find(" <") {
            from_field[..name_end].to_string()
        } else if from_field.contains('@') {
            if let Some(at_pos) = from_field.find('@') {
                from_field[..at_pos].to_string()
            } else {
                from_field.to_string()
            }
        } else {
            from_field.to_string()
        }
    }

    fn build_email_list_with_truncation(
        emails: &[Email],
        available_width: usize,
        is_sent_folder: bool,
    ) -> Vec<ListItem<'static>> {
        const UNREAD_WIDTH: usize = 2;
        const DATE_WIDTH: usize = 10;
        const ATTACHMENT_WIDTH: usize = 3;
        const SEPARATORS: usize = 8;

        let min_from_width = 15;
        let max_from_width = (available_width * 30) / 100;
        let from_width = min_from_width.max(max_from_width).min(25);

        emails
            .iter()
            .map(|email| {
                let mut style = Style::default();
                if email.is_unread {
                    style = style.add_modifier(Modifier::BOLD);
                }

                let subject_width = available_width
                    .saturating_sub(UNREAD_WIDTH)
                    .saturating_sub(from_width)
                    .saturating_sub(DATE_WIDTH)
                    .saturating_sub(ATTACHMENT_WIDTH)
                    .saturating_sub(SEPARATORS);

                let mut spans = vec![];
                spans.push(Span::styled(if email.is_unread { "•" } else { " " }, style));
                spans.push(Span::raw(" "));

                let sender = if is_sent_folder {
                    Self::extract_email_address(&email.headers.to)
                } else {
                    Self::extract_email_address(&email.headers.from)
                };
                let truncated_sender = Self::truncate_with_ellipsis(&sender, from_width);
                let padded_sender = Self::pad_to_width(&truncated_sender, from_width);
                spans.push(Span::styled(padded_sender, style));
                spans.push(Span::raw("  "));

                let subject = if email.headers.subject.is_empty() {
                    "(No Subject)"
                } else {
                    &email.headers.subject
                };
                let truncated_subject = Self::truncate_with_ellipsis(subject, subject_width);
                let padded_subject = Self::pad_to_width(&truncated_subject, subject_width);
                spans.push(Span::styled(padded_subject, style));
                spans.push(Span::raw("  "));

                spans.push(Span::styled(
                    if email.has_attachments() {
                        "📎"
                    } else {
                        "  "
                    },
                    style,
                ));
                spans.push(Span::raw(" "));

                let date_str = Self::format_email_date(&email.headers.date);
                spans.push(Span::styled(date_str, style));

                ListItem::new(Line::from(spans))
            })
            .collect()
    }
}

impl Default for MessagesComponent {
    fn default() -> Self {
        Self::new()
    }
}

impl Component for MessagesComponent {
    fn handle_msg(&mut self, msg: &Msg, ctx: &Ctx) -> Vec<Msg> {
        match msg {
            Msg::MessageMove(Dir::Down) => {
                let total = ctx.store.get_current_folder().emails.len();
                if self.email_index + 1 < total {
                    self.email_index += 1;
                    // Look ahead: if the user is scrolling into the
                    // unloaded tail, ask the store for more headers.
                    if self.email_index + SCROLL_LOOKAHEAD >= total {
                        return vec![Msg::StoreLoadMore(self.email_index)];
                    }
                }
            }
            Msg::MessageMove(Dir::Up) => {
                if self.email_index > 0 {
                    self.email_index -= 1;
                }
            }
            Msg::FolderMove(_) | Msg::FolderEnter | Msg::FolderExitParent => {
                // New folder context: drop the cursor to the top and
                // clear the cross-pane remembered position. The legacy
                // `App::load_selected_folder_messages` and
                // `handle_back_navigation` paths did exactly this.
                self.email_index = 0;
                self.remembered_email_index = None;
            }
            Msg::FoldersBlur => {
                // Focus just moved Folders → Messages. Restore the
                // remembered cursor or, if none, pick the first email.
                let current = ctx.store.get_current_folder();
                let target = self
                    .remembered_email_index
                    .filter(|&idx| idx < current.emails.len())
                    .unwrap_or(0);
                if target < current.emails.len() {
                    self.email_index = target;
                }
            }
            Msg::MessageOpen(_) => {
                // Enter on a message: pair the open with an auto
                // mark-read (vu-rxi, Phase 1.b / VISION.md "Enter
                // (auto mark-read)"). AppRoot derives the actual
                // message id from `email_index` — we pass an empty
                // sentinel the same way `on_key(Enter)` does for
                // `MessageOpen` itself.
                return vec![Msg::MessageMarkRead(String::new())];
            }
            Msg::MessagesBlur => {
                // Focus just moved Messages → Folders. Remember where
                // the cursor was so the next `FoldersBlur` can restore it.
                // The legacy `app.switch_pane` checked
                // `email_store.selected_email.is_some()` here — but it
                // *also* deselected the email *before* emitting this
                // message, so the check would always fail. Always
                // remember; if the cursor pointed at nothing meaningful,
                // `FoldersBlur` clamps to 0 on restore anyway.
                let _ = ctx;
                self.remembered_email_index = Some(self.email_index);
            }
            _ => {}
        }
        Vec::new()
    }

    fn render(&self, _f: &mut Frame, _area: Rect, _focused: bool, _ctx: &Ctx) {
        // The Messages pane needs a view-aware folder pick that is owned
        // by AppRoot/ui.rs, so the actual draw happens through
        // `render_with_folder`. This bare-trait impl is a no-op; calling
        // it would render nothing rather than the wrong folder.
    }

    fn on_key(&mut self, key: KeyEvent, _ctx: &Ctx) -> Option<Msg> {
        if !key.modifiers.is_empty() && !matches!(key.code, KeyCode::Up | KeyCode::Down) {
            return None;
        }
        match key.code {
            KeyCode::Char('j') => Some(Msg::MessageMove(Dir::Down)),
            KeyCode::Char('k') => Some(Msg::MessageMove(Dir::Up)),
            KeyCode::Down => Some(Msg::MessageMove(Dir::Down)),
            KeyCode::Up => Some(Msg::MessageMove(Dir::Up)),
            // `MessageOpen` carries a `MessageId`. Until the store grows
            // a real index, the open semantics are "open whatever the
            // cursor is on" and the id is derived in `apply_root` from
            // `self.email_index`. We pass an empty id as a sentinel.
            KeyCode::Enter => Some(Msg::MessageOpen(String::new())),
            KeyCode::Backspace => Some(Msg::FolderExitParent),
            // Phase 1.c (vu-bti). Same empty-id sentinel — AppRoot
            // resolves the target email from the cursor.
            KeyCode::Char('a') => Some(Msg::Archive(String::new())),
            KeyCode::Char('s') => Some(Msg::ToggleStar(String::new())),
            KeyCode::Char('d') => Some(Msg::Delete(String::new())),
            // Phase 1.d (vu-rr6). 'm' surfaces the folder picker; the
            // picker dispatches `Msg::MoveTo` on Enter.
            KeyCode::Char('m') => Some(Msg::OpenFolderPicker),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::email::{Email, EmailStore, Folder};
    use crossterm::event::KeyModifiers;
    use std::path::PathBuf;

    fn store_with_one_folder(emails: usize) -> EmailStore {
        let mut store = EmailStore::new(PathBuf::from("/tmp"));
        let mut inbox = Folder::new("INBOX".to_string(), PathBuf::from("/tmp/INBOX"));
        for i in 0..emails {
            inbox.add_email(Email::new(PathBuf::from(format!("/tmp/INBOX/m{}", i))));
        }
        inbox.is_loaded = true;
        store.root_folder.add_subfolder(inbox);
        store.enter_folder_by_path(&[0]);
        if emails > 0 {
            store.select_email(0);
        }
        store
    }

    fn ctx<'a>(theme: &'a VulthorTheme, config: &'a Config, store: &'a EmailStore) -> Ctx<'a> {
        Ctx {
            theme,
            config,
            store,
        }
    }

    #[test]
    fn message_move_down_advances_and_clamps() {
        let store = store_with_one_folder(3);
        let (theme, config) = (VulthorTheme, Config::default());
        let ctx = ctx(&theme, &config, &store);

        let mut m = MessagesComponent::new();
        m.handle_msg(&Msg::MessageMove(Dir::Down), &ctx);
        assert_eq!(m.email_index, 1);
        m.handle_msg(&Msg::MessageMove(Dir::Down), &ctx);
        assert_eq!(m.email_index, 2);
        // At the last email — further Down is a no-op (clamp, not wrap).
        m.handle_msg(&Msg::MessageMove(Dir::Down), &ctx);
        assert_eq!(m.email_index, 2);
    }

    #[test]
    fn message_move_up_clamps_at_zero() {
        let store = store_with_one_folder(3);
        let (theme, config) = (VulthorTheme, Config::default());
        let ctx = ctx(&theme, &config, &store);

        let mut m = MessagesComponent::new();
        m.handle_msg(&Msg::MessageMove(Dir::Up), &ctx);
        assert_eq!(m.email_index, 0);
    }

    /// `MessageMove(Down)` near the tail must emit `StoreLoadMore` so the
    /// headers worker can keep ahead of the cursor. The lookahead matches
    /// `SCROLL_LOOKAHEAD` (= 5). With 6 emails and cursor at 0, moving
    /// down to 1 puts us at `1 + 5 = 6 >= 6` → load-more fires.
    #[test]
    fn message_move_emits_store_load_more_near_tail() {
        let store = store_with_one_folder(6);
        let (theme, config) = (VulthorTheme, Config::default());
        let ctx = ctx(&theme, &config, &store);

        let mut m = MessagesComponent::new();
        let followups = m.handle_msg(&Msg::MessageMove(Dir::Down), &ctx);
        assert!(
            followups.iter().any(|x| matches!(x, Msg::StoreLoadMore(_))),
            "near-tail scroll must request more headers, got {:?}",
            followups,
        );
    }

    /// Mid-list scrolls (well clear of the lookahead window) must NOT
    /// pile up `StoreLoadMore` requests — the legacy `index + 5 >= len`
    /// test in `load_more_messages_if_needed` is the contract.
    #[test]
    fn message_move_does_not_emit_store_load_more_mid_list() {
        let store = store_with_one_folder(100);
        let (theme, config) = (VulthorTheme, Config::default());
        let ctx = ctx(&theme, &config, &store);

        let mut m = MessagesComponent::new();
        let followups = m.handle_msg(&Msg::MessageMove(Dir::Down), &ctx);
        assert!(followups.is_empty(), "mid-list scroll should not fan out");
    }

    #[test]
    fn folder_move_resets_email_cursor_and_clears_remembered() {
        let store = store_with_one_folder(5);
        let (theme, config) = (VulthorTheme, Config::default());
        let ctx = ctx(&theme, &config, &store);

        let mut m = MessagesComponent::new();
        m.email_index = 3;
        m.remembered_email_index = Some(2);
        m.handle_msg(&Msg::FolderMove(Dir::Down), &ctx);
        assert_eq!(m.email_index, 0);
        assert!(m.remembered_email_index.is_none());
    }

    #[test]
    fn folder_enter_resets_email_cursor_and_clears_remembered() {
        let store = store_with_one_folder(5);
        let (theme, config) = (VulthorTheme, Config::default());
        let ctx = ctx(&theme, &config, &store);

        let mut m = MessagesComponent::new();
        m.email_index = 4;
        m.remembered_email_index = Some(3);
        m.handle_msg(&Msg::FolderEnter, &ctx);
        assert_eq!(m.email_index, 0);
        assert!(m.remembered_email_index.is_none());
    }

    #[test]
    fn folder_exit_parent_resets_email_cursor() {
        let store = store_with_one_folder(5);
        let (theme, config) = (VulthorTheme, Config::default());
        let ctx = ctx(&theme, &config, &store);

        let mut m = MessagesComponent::new();
        m.email_index = 2;
        m.handle_msg(&Msg::FolderExitParent, &ctx);
        assert_eq!(m.email_index, 0);
    }

    #[test]
    fn messages_blur_always_remembers_email_index() {
        // The legacy `app.switch_pane` deselects the email *before*
        // we get a chance to read it, so the component cannot gate
        // remembering on `store.selected_email.is_some()`. Always
        // remember; `FoldersBlur` does the clamp on restore.
        let store = store_with_one_folder(5);
        let (theme, config) = (VulthorTheme, Config::default());
        let ctx = ctx(&theme, &config, &store);

        let mut m = MessagesComponent::new();
        m.email_index = 3;
        m.handle_msg(&Msg::MessagesBlur, &ctx);
        assert_eq!(m.remembered_email_index, Some(3));
    }

    #[test]
    fn folders_blur_restores_remembered_email_index() {
        let store = store_with_one_folder(5);
        let (theme, config) = (VulthorTheme, Config::default());
        let ctx = ctx(&theme, &config, &store);

        let mut m = MessagesComponent::new();
        m.email_index = 0;
        m.remembered_email_index = Some(2);
        m.handle_msg(&Msg::FoldersBlur, &ctx);
        assert_eq!(m.email_index, 2);
    }

    #[test]
    fn folders_blur_picks_first_email_when_nothing_remembered() {
        let store = store_with_one_folder(5);
        let (theme, config) = (VulthorTheme, Config::default());
        let ctx = ctx(&theme, &config, &store);

        let mut m = MessagesComponent::new();
        m.email_index = 4;
        m.handle_msg(&Msg::FoldersBlur, &ctx);
        assert_eq!(m.email_index, 0);
    }

    #[test]
    fn folders_blur_clamps_to_emails_len() {
        // Remembered index points past end of (shorter) folder — must
        // fall back to 0, not panic on the OOB select.
        let store = store_with_one_folder(2);
        let (theme, config) = (VulthorTheme, Config::default());
        let ctx = ctx(&theme, &config, &store);

        let mut m = MessagesComponent::new();
        m.remembered_email_index = Some(99);
        m.handle_msg(&Msg::FoldersBlur, &ctx);
        assert_eq!(m.email_index, 0);
    }

    #[test]
    fn on_key_jk_maps_to_message_move() {
        let store = store_with_one_folder(3);
        let (theme, config) = (VulthorTheme, Config::default());
        let ctx = ctx(&theme, &config, &store);
        let mut m = MessagesComponent::new();

        let j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(m.on_key(j, &ctx), Some(Msg::MessageMove(Dir::Down)));
        let k = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(m.on_key(k, &ctx), Some(Msg::MessageMove(Dir::Up)));
        let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(m.on_key(down, &ctx), Some(Msg::MessageMove(Dir::Down)));
        let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(m.on_key(up, &ctx), Some(Msg::MessageMove(Dir::Up)));
    }

    /// vu-rxi (Phase 1.b): the open path pairs with an auto mark-read.
    /// MessagesComponent emits `MessageMarkRead` as a follow-up to
    /// `MessageOpen` so a single Enter triggers both.
    #[test]
    fn message_open_returns_mark_read_follow_up() {
        let store = store_with_one_folder(3);
        let (theme, config) = (VulthorTheme, Config::default());
        let ctx = ctx(&theme, &config, &store);
        let mut m = MessagesComponent::new();

        let followups = m.handle_msg(&Msg::MessageOpen(String::new()), &ctx);
        assert!(
            followups
                .iter()
                .any(|x| matches!(x, Msg::MessageMarkRead(_))),
            "Enter must dispatch MessageMarkRead alongside MessageOpen, got {:?}",
            followups,
        );
    }

    #[test]
    fn on_key_enter_emits_message_open() {
        let store = store_with_one_folder(3);
        let (theme, config) = (VulthorTheme, Config::default());
        let ctx = ctx(&theme, &config, &store);
        let mut m = MessagesComponent::new();

        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(m.on_key(enter, &ctx), Some(Msg::MessageOpen(_))));
    }

    #[test]
    fn on_key_a_s_d_emit_action_messages() {
        // Phase 1.c (vu-bti): direct action keys map to mutation
        // messages carrying an empty MessageId sentinel.
        let store = store_with_one_folder(3);
        let (theme, config) = (VulthorTheme, Config::default());
        let ctx = ctx(&theme, &config, &store);
        let mut m = MessagesComponent::new();

        let a = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        assert!(matches!(m.on_key(a, &ctx), Some(Msg::Archive(_))));
        let s = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE);
        assert!(matches!(m.on_key(s, &ctx), Some(Msg::ToggleStar(_))));
        let d = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE);
        assert!(matches!(m.on_key(d, &ctx), Some(Msg::Delete(_))));
    }

    #[test]
    fn on_key_m_emits_open_folder_picker() {
        // Phase 1.d (vu-rr6): 'm' surfaces the modal folder picker.
        let store = store_with_one_folder(3);
        let (theme, config) = (VulthorTheme, Config::default());
        let ctx = ctx(&theme, &config, &store);
        let mut m = MessagesComponent::new();
        let key = KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE);
        assert_eq!(m.on_key(key, &ctx), Some(Msg::OpenFolderPicker));
    }

    #[test]
    fn on_key_backspace_emits_folder_exit_parent() {
        let store = store_with_one_folder(3);
        let (theme, config) = (VulthorTheme, Config::default());
        let ctx = ctx(&theme, &config, &store);
        let mut m = MessagesComponent::new();

        let backspace = KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(m.on_key(backspace, &ctx), Some(Msg::FolderExitParent));
    }

    #[test]
    fn on_key_ignores_modified_keys() {
        let store = store_with_one_folder(3);
        let (theme, config) = (VulthorTheme, Config::default());
        let ctx = ctx(&theme, &config, &store);
        let mut m = MessagesComponent::new();

        let alt_j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::ALT);
        assert_eq!(m.on_key(alt_j, &ctx), None);
    }

    // --- Rendering helper tests (migrated from ui.rs in vu-3ko). ---
    // The helpers are private to `MessagesComponent`; these tests
    // exercise them via `super::*`.

    use crate::email::EmailHeaders;

    #[test]
    fn format_email_date_today_shows_hhmm() {
        let now = Local::now();
        let date_str = now.to_rfc3339();
        let formatted = MessagesComponent::format_email_date(&date_str);
        assert_eq!(formatted.len(), 5);
        assert!(formatted.contains(':'));
    }

    #[test]
    fn format_email_date_past_shows_iso_date() {
        let date_str = "2024-01-15T10:30:00+00:00";
        let formatted = MessagesComponent::format_email_date(date_str);
        assert_eq!(formatted, "2024-01-15");
    }

    #[test]
    fn format_email_date_invalid_falls_back_to_first_ten_chars() {
        let date_str = "invalid date";
        let formatted = MessagesComponent::format_email_date(date_str);
        assert_eq!(formatted, "invalid da");
    }

    #[test]
    fn truncate_with_ellipsis_handles_unicode_and_short_widths() {
        assert_eq!(
            MessagesComponent::truncate_with_ellipsis("Short text", 20),
            "Short text"
        );
        let long = "This is a very long subject line that needs truncation";
        assert_eq!(
            MessagesComponent::truncate_with_ellipsis(long, 20),
            "This is a very lo..."
        );
        assert_eq!(MessagesComponent::truncate_with_ellipsis(long, 3), "Thi");
        let emoji = "Hello 🌍 World 🚀 Test";
        assert_eq!(
            MessagesComponent::truncate_with_ellipsis(emoji, 15),
            "Hello 🌍 Wor..."
        );
        let emoji2 = "Test 🎉🎊🎈";
        assert_eq!(
            MessagesComponent::truncate_with_ellipsis(emoji2, 8),
            "Test ..."
        );
    }

    #[test]
    fn pad_to_width_handles_unicode_widths() {
        assert_eq!(MessagesComponent::pad_to_width("Hello", 10), "Hello     ");
        assert_eq!(MessagesComponent::pad_to_width("Hello", 10).width(), 10);
        let emoji = "Hi 🌍";
        assert_eq!(MessagesComponent::pad_to_width(emoji, 10).width(), 10);
        assert_eq!(MessagesComponent::pad_to_width(emoji, 3), emoji);
    }

    #[test]
    fn extract_email_address_parses_common_formats() {
        assert_eq!(
            MessagesComponent::extract_email_address("John Doe <john@example.com>"),
            "John Doe"
        );
        assert_eq!(
            MessagesComponent::extract_email_address("jane@example.com"),
            "jane"
        );
        assert_eq!(
            MessagesComponent::extract_email_address("Bob Smith"),
            "Bob Smith"
        );
    }

    #[test]
    fn build_email_list_with_truncation_renders_one_per_email() {
        let mut email = Email::new(PathBuf::from("/test/email"));
        email.headers = EmailHeaders {
            from: "Alice Johnson <alice@example.com>".to_string(),
            to: "Bob Smith <bob@example.com>".to_string(),
            subject: "This is a very long subject line that will need to be truncated for display"
                .to_string(),
            date: Local::now().to_rfc3339(),
            message_id: "123".to_string(),
        };
        email.is_unread = true;
        let emails = vec![email];

        assert_eq!(
            MessagesComponent::build_email_list_with_truncation(&emails, 80, false).len(),
            1
        );
        assert_eq!(
            MessagesComponent::build_email_list_with_truncation(&emails, 80, true).len(),
            1
        );
    }

    #[test]
    fn build_email_list_with_truncation_does_not_panic_on_emoji() {
        let mut email = Email::new(PathBuf::from("/test/email"));
        email.headers = EmailHeaders {
            from: "Alice 🦄 <alice@example.com>".to_string(),
            to: "Bob 🚀 <bob@example.com>".to_string(),
            subject: "Meeting tomorrow 📅 Important! 🔥🔥🔥".to_string(),
            date: Local::now().to_rfc3339(),
            message_id: "456".to_string(),
        };
        email.is_unread = false;
        let emails = vec![email];
        assert_eq!(
            MessagesComponent::build_email_list_with_truncation(&emails, 60, false).len(),
            1
        );
    }
}
