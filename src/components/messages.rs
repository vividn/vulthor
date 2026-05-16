// `MessagesComponent` — message-list pane.
//
// Owns the message-pane email cursor (`email_index`), the
// remembered-cursor hand-off slot (`remembered_email_index`), and the
// last-rendered visible-row count (`visible_rows`). Translates Messages-
// pane keys into messages, restores/remembers selection across
// Folders↔Messages focus changes, and renders the email list.
//
// **`Cell<usize>` visible_rows / `RefCell<ListState>`.** Ratatui's
// `render_stateful_widget` requires `&mut ListState`, but
// `Component::render` takes `&self`. The component owns both as
// interior-mutable cells. `visible_rows` is read by `handle_msg` when
// `MessageMove(Down)` checks whether to emit `Msg::StoreLoadMore`; it
// is written exclusively by `render` from the live pane area.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

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

use crate::email::{DraftInfo, Email, Folder};
use crate::theme::VulthorTheme;

use super::{Component, Ctx, Dir, Msg};

/// How many rows past the visible tail we look ahead before asking the
/// store for more headers. Matches the legacy `index + 5 >= len` test
/// in `EmailStore::load_more_messages_if_needed`.
const SCROLL_LOOKAHEAD: usize = 5;

/// Messages pane state. Owns the email cursor, the
/// remembered-cursor handoff slot used across pane focus changes,
/// and a `Cell` mirroring the last-rendered row count so handle_msg
/// can size lookahead loads.
pub struct MessagesComponent {
    /// Cursor into the current folder's `emails`. Reset to 0 on
    /// folder enter / exit.
    pub email_index: usize,
    /// Cursor snapshotted when focus last left the Messages pane.
    /// Restored on `FoldersBlur`; `None` if focus has never settled
    /// here yet.
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
    /// Build a fresh component: cursor at 0, no remembered index,
    /// `visible_rows` seeded to 20 so pre-render `handle_msg` calls
    /// still get a sensible answer.
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
        drafts: &HashMap<String, DraftInfo>,
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
            drafts,
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

    /// Pick the chip character for an original message based on the
    /// drafts index (Phase 2.c). `Some('✏')` for an in-progress
    /// reply, `Some('⏰')` for an empty reply-later placeholder, `None`
    /// when there's no draft for this id. Pulled out of the renderer so
    /// it can be tested without a full Frame.
    pub fn chip_for_message_id(
        drafts: &HashMap<String, DraftInfo>,
        message_id: &str,
    ) -> Option<char> {
        if message_id.is_empty() {
            return None;
        }
        let info = drafts.get(message_id)?;
        Some(if info.body_empty { '⏰' } else { '✏' })
    }

    fn build_email_list_with_truncation(
        emails: &[Email],
        available_width: usize,
        is_sent_folder: bool,
        drafts: &HashMap<String, DraftInfo>,
    ) -> Vec<ListItem<'static>> {
        emails
            .iter()
            .map(|email| {
                ListItem::new(Line::from(Self::build_email_row_spans(
                    email,
                    available_width,
                    is_sent_folder,
                    drafts,
                )))
            })
            .collect()
    }

    /// Build the row's spans for one email. Extracted from
    /// `build_email_list_with_truncation` so the test suite can inspect
    /// the rendered glyphs and column widths without going through the
    /// private `ListItem.content` field.
    fn build_email_row_spans(
        email: &Email,
        available_width: usize,
        is_sent_folder: bool,
        drafts: &HashMap<String, DraftInfo>,
    ) -> Vec<Span<'static>> {
        const UNREAD_WIDTH: usize = 2;
        // `✏`/`⏰` plus trailing space — reserved even when no chip
        // present so the From column stays vertically aligned.
        const CHIP_WIDTH: usize = 2;
        const DATE_WIDTH: usize = 10;
        const ATTACHMENT_WIDTH: usize = 3;
        const SEPARATORS: usize = 8;

        let min_from_width = 15;
        let max_from_width = (available_width * 30) / 100;
        let from_width = min_from_width.max(max_from_width).min(25);

        let mut style = Style::default();
        if email.is_unread {
            style = style.add_modifier(Modifier::BOLD);
        }

        let subject_width = available_width
            .saturating_sub(UNREAD_WIDTH)
            .saturating_sub(CHIP_WIDTH)
            .saturating_sub(from_width)
            .saturating_sub(DATE_WIDTH)
            .saturating_sub(ATTACHMENT_WIDTH)
            .saturating_sub(SEPARATORS);

        let mut spans = vec![];
        spans.push(Span::styled(if email.is_unread { "•" } else { " " }, style));
        spans.push(Span::raw(" "));

        // Chip slot. Always emits CHIP_WIDTH wide so absent chips don't
        // shift the From column off by one.
        let chip_text = match Self::chip_for_message_id(drafts, &email.headers.message_id) {
            Some(c) => Self::pad_to_width(&c.to_string(), CHIP_WIDTH),
            None => " ".repeat(CHIP_WIDTH),
        };
        spans.push(Span::styled(chip_text, style));

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

        spans
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
                // mark-read (VISION.md "Enter (auto mark-read)"). AppRoot
                // derives the actual message id from `email_index` — we
                // pass an empty sentinel the same way `on_key(Enter)`
                // does for `MessageOpen` itself.
                return vec![Msg::MessageMarkRead(String::new())];
            }
            Msg::MessagesBlur => {
                // Focus just moved Messages → Folders. Remember where
                // the cursor was so the next `FoldersBlur` can restore
                // it. Always remember; if the cursor pointed at nothing
                // meaningful, `FoldersBlur` clamps to 0 on restore anyway.
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
        // SHIFT is allowed (capital-letter bindings like `F`/`U`); other
        // modifiers (ALT, CTRL) still bail out so Alt-pane shortcuts and
        // friends don't double-fire. Arrow keys keep their legacy
        // any-modifier pass-through.
        use crossterm::event::KeyModifiers;
        let mods_ok = key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT;
        if !mods_ok && !matches!(key.code, KeyCode::Up | KeyCode::Down) {
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
            // Same empty-id sentinel — AppRoot resolves the target
            // email from the cursor.
            KeyCode::Char('a') => Some(Msg::Archive(String::new())),
            // `F` is a capital-letter alias for `s`. VISION.md lists
            // both; treating them as the same action keeps the
            // rebinding story simple.
            KeyCode::Char('s') | KeyCode::Char('F') => Some(Msg::ToggleStar(String::new())),
            KeyCode::Char('d') => Some(Msg::Delete(String::new())),
            // 'm' surfaces the folder picker; the picker dispatches
            // `Msg::MoveTo` on Enter.
            KeyCode::Char('m') => Some(Msg::OpenFolderPicker),
            // Mark the cursor email unread.
            KeyCode::Char('U') => Some(Msg::MarkUnread(String::new())),
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

    /// The open path pairs with an auto mark-read. MessagesComponent
    /// emits `MessageMarkRead` as a follow-up to `MessageOpen` so a
    /// single Enter triggers both.
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
        // Direct action keys map to mutation messages carrying an
        // empty MessageId sentinel.
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
        // 'm' surfaces the modal folder picker.
        let store = store_with_one_folder(3);
        let (theme, config) = (VulthorTheme, Config::default());
        let ctx = ctx(&theme, &config, &store);
        let mut m = MessagesComponent::new();
        let key = KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE);
        assert_eq!(m.on_key(key, &ctx), Some(Msg::OpenFolderPicker));
    }

    #[test]
    fn on_key_capital_f_emits_toggle_star_alias() {
        // VISION.md lists `F` as a capital-letter alias for `s`. Both
        // forms — bare and SHIFT-modified, since terminals vary — must
        // produce ToggleStar.
        let store = store_with_one_folder(3);
        let (theme, config) = (VulthorTheme, Config::default());
        let ctx = ctx(&theme, &config, &store);
        let mut m = MessagesComponent::new();

        let f_bare = KeyEvent::new(KeyCode::Char('F'), KeyModifiers::NONE);
        assert!(matches!(m.on_key(f_bare, &ctx), Some(Msg::ToggleStar(_))));
        let f_shift = KeyEvent::new(KeyCode::Char('F'), KeyModifiers::SHIFT);
        assert!(matches!(m.on_key(f_shift, &ctx), Some(Msg::ToggleStar(_))));
    }

    #[test]
    fn on_key_capital_u_emits_mark_unread() {
        // `U` marks the cursor email unread.
        let store = store_with_one_folder(3);
        let (theme, config) = (VulthorTheme, Config::default());
        let ctx = ctx(&theme, &config, &store);
        let mut m = MessagesComponent::new();

        let u_bare = KeyEvent::new(KeyCode::Char('U'), KeyModifiers::NONE);
        assert!(matches!(m.on_key(u_bare, &ctx), Some(Msg::MarkUnread(_))));
        let u_shift = KeyEvent::new(KeyCode::Char('U'), KeyModifiers::SHIFT);
        assert!(matches!(m.on_key(u_shift, &ctx), Some(Msg::MarkUnread(_))));
    }

    #[test]
    fn on_key_lowercase_u_does_not_emit_mark_unread() {
        // Capital-letter bindings must not collide with lowercase
        // global keys (`u` is the session-undo key in `AppRoot`). The
        // Messages component returns None for plain `u` so the global
        // handler can claim it.
        let store = store_with_one_folder(3);
        let (theme, config) = (VulthorTheme, Config::default());
        let ctx = ctx(&theme, &config, &store);
        let mut m = MessagesComponent::new();

        let u = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE);
        assert_eq!(m.on_key(u, &ctx), None);
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

    // --- Rendering helper tests. ---
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
        let drafts = HashMap::new();

        assert_eq!(
            MessagesComponent::build_email_list_with_truncation(&emails, 80, false, &drafts).len(),
            1
        );
        assert_eq!(
            MessagesComponent::build_email_list_with_truncation(&emails, 80, true, &drafts).len(),
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
        let drafts = HashMap::new();
        assert_eq!(
            MessagesComponent::build_email_list_with_truncation(&emails, 60, false, &drafts).len(),
            1
        );
    }

    // --- Phase 2.c: chip selection contract. ---

    fn draft(body_empty: bool) -> DraftInfo {
        DraftInfo {
            path: PathBuf::from("/tmp/drafts/cur/x"),
            body_empty,
        }
    }

    /// In-progress draft (non-empty body) maps to the `✏` chip;
    /// reply-later marker (empty body) maps to `⏰`. Lookup by the
    /// original message's `message_id`.
    #[test]
    fn chip_for_message_id_returns_pencil_for_in_progress_drafts() {
        let mut drafts = HashMap::new();
        drafts.insert("orig-1@example.com".to_string(), draft(false));
        assert_eq!(
            MessagesComponent::chip_for_message_id(&drafts, "orig-1@example.com"),
            Some('✏'),
        );
    }

    #[test]
    fn chip_for_message_id_returns_alarm_for_reply_later_drafts() {
        let mut drafts = HashMap::new();
        drafts.insert("orig-2@example.com".to_string(), draft(true));
        assert_eq!(
            MessagesComponent::chip_for_message_id(&drafts, "orig-2@example.com"),
            Some('⏰'),
        );
    }

    /// Without a matching draft, no chip is rendered — and an empty
    /// message-id must never match (Phase 1.x action-key handlers pass
    /// an empty-id sentinel before resolving cursor position).
    #[test]
    fn chip_for_message_id_returns_none_when_no_draft_or_empty_id() {
        let mut drafts = HashMap::new();
        drafts.insert("orig-3@example.com".to_string(), draft(false));
        assert_eq!(
            MessagesComponent::chip_for_message_id(&drafts, "nobody@example.com"),
            None,
        );
        assert_eq!(MessagesComponent::chip_for_message_id(&drafts, ""), None);
    }

    /// Render-path integration: the chip slot is reserved-width, so an
    /// email *without* a draft and an email *with* a draft must produce
    /// list rows of the same total visible width. (Same available width
    /// in, same row width out — alignment guarantee.)
    #[test]
    fn build_email_list_reserves_chip_width_even_without_chip() {
        fn email_for(id: &str) -> Email {
            let mut e = Email::new(PathBuf::from(format!("/tmp/{}", id)));
            e.headers = EmailHeaders {
                from: "a@b.test".to_string(),
                to: "c@d.test".to_string(),
                subject: "subject line".to_string(),
                date: "2024-01-15T10:30:00+00:00".to_string(),
                message_id: id.to_string(),
            };
            e
        }
        let with_match = email_for("orig-1@x");
        let without = email_for("orig-2@x");

        let mut drafts = HashMap::new();
        drafts.insert("orig-1@x".to_string(), draft(false));

        let with_spans = MessagesComponent::build_email_row_spans(&with_match, 80, false, &drafts);
        let without_spans = MessagesComponent::build_email_row_spans(&without, 80, false, &drafts);

        let width = |spans: &[Span<'static>]| -> usize {
            spans.iter().map(|s| s.content.as_ref().width()).sum()
        };
        assert_eq!(
            width(&with_spans),
            width(&without_spans),
            "chip-present and chip-absent rows must render at the same width \
             so the From column stays aligned",
        );
    }

    /// The chip character actually shows up in the rendered spans for
    /// emails that have a matching draft, and the right glyph is used
    /// per body_empty.
    #[test]
    fn build_email_list_emits_pencil_and_alarm_glyphs_per_draft_kind() {
        fn email_for(id: &str) -> Email {
            let mut e = Email::new(PathBuf::from(format!("/tmp/{}", id)));
            e.headers = EmailHeaders {
                from: "a@b.test".to_string(),
                to: "c@d.test".to_string(),
                subject: "s".to_string(),
                date: "2024-01-15T10:30:00+00:00".to_string(),
                message_id: id.to_string(),
            };
            e
        }
        let in_progress = email_for("orig-1@x");
        let later = email_for("orig-2@x");

        let mut drafts = HashMap::new();
        drafts.insert("orig-1@x".to_string(), draft(false));
        drafts.insert("orig-2@x".to_string(), draft(true));

        let row_text = |email: &Email| -> String {
            MessagesComponent::build_email_row_spans(email, 80, false, &drafts)
                .into_iter()
                .map(|s| s.content.into_owned())
                .collect::<String>()
        };
        let in_progress_row = row_text(&in_progress);
        let later_row = row_text(&later);
        assert!(
            in_progress_row.contains('✏'),
            "in-progress draft must render ✏ chip, row was {:?}",
            in_progress_row,
        );
        assert!(
            later_row.contains('⏰'),
            "reply-later draft must render ⏰ chip, row was {:?}",
            later_row,
        );
    }
}
