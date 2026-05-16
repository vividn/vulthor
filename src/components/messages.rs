// `MessagesComponent` — Phase 0.2.3 (vu-3yj).
//
// Owns the message-pane selection (`email_index`), the cross-pane
// hand-off slot (`remembered_email_index`), the attachment-pane
// selection (`attachment_index`), and the visible-rows hint
// (`message_pane_visible_rows`, set by render based on terminal
// height). Renders the message list. Translates Messages-pane
// keys (`j`/`k`/`Up`/`Down`) into `Msg::MessageMove`.
//
// **Sole writer of those fields.** AppRoot mirrors them into
// `App.selection` after each dispatch so the legacy `input.rs` paths
// (Enter, Backspace, Tab, etc.) and ui.rs's surrounding readers keep
// working until Step 5 retires `App`.
//
// **`remembered_email_index` hand-off.** Owned here so that the
// h/l view transition (handled in `AppRoot::apply_root` via the
// existing `App::next_view`/`prev_view` helpers) stashes/restores
// through this component, not directly through `App.selection`.
// AppRoot mirrors the value both ways: component → app before the
// transition, app → component after, so the hand-off is observable
// from the component.
//
// **`RefCell<ListState>`** — same workaround as `FoldersComponent`:
// `render_stateful_widget` needs `&mut ListState`, but `Component::
// render` takes `&self`. Documented in DESIGN-COMPONENTS.md
// § "Risks & open questions".
//
// **`Cell<usize>` for `message_pane_visible_rows`.** Render observes
// the terminal area height and writes it into the cell so the
// dispatch path can use it when sizing initial loads.

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

use crate::app::View;
use crate::email::{Email, Folder};
use crate::theme::VulthorTheme;

use super::{Component, Ctx, Dir, Msg};

pub struct MessagesComponent {
    pub email_index: usize,
    pub remembered_email_index: Option<usize>,
    pub attachment_index: usize,
    /// Visible rows in the message pane (terminal area height minus
    /// borders). Written by `render`, read by `AppRoot` when sizing
    /// initial loads. Default 20 mirrors the legacy `App` default.
    pub message_pane_visible_rows: Cell<usize>,
    list_state: RefCell<ListState>,
    attachment_list_state: RefCell<ListState>,
}

impl Default for MessagesComponent {
    fn default() -> Self {
        Self::new()
    }
}

impl MessagesComponent {
    pub fn new() -> Self {
        Self {
            email_index: 0,
            remembered_email_index: None,
            attachment_index: 0,
            message_pane_visible_rows: Cell::new(20),
            list_state: RefCell::new(ListState::default()),
            attachment_list_state: RefCell::new(ListState::default()),
        }
    }

    /// Pick the folder whose emails are listed in the messages pane.
    /// `FolderMessages` (the leftmost two-pane view) shows the
    /// emails of the folder *highlighted* in the Folders pane;
    /// every other view shows the *current* folder the user
    /// entered. Mirrors the existing `ui.rs` selection logic so the
    /// extraction is behaviorally identical.
    fn folder_to_display<'a>(ctx: &'a Ctx) -> &'a Folder {
        match ctx.view {
            View::FolderMessages => {
                let path = crate::input::get_folder_path_from_display_index(
                    &ctx.store.root_folder,
                    ctx.folder_index,
                );
                match path.and_then(|p| ctx.store.get_folder_at_path(&p)) {
                    Some(f) => f,
                    None => ctx.store.get_current_folder(),
                }
            }
            _ => ctx.store.get_current_folder(),
        }
    }
}

impl Component for MessagesComponent {
    fn handle_msg(&mut self, msg: &Msg, ctx: &Ctx) -> Vec<Msg> {
        match msg {
            Msg::MessageMove(Dir::Down) => {
                let folder = Self::folder_to_display(ctx);
                let total = folder.emails.len();
                if self.email_index + 1 < total {
                    self.email_index += 1;
                }
            }
            Msg::MessageMove(Dir::Up) => {
                if self.email_index > 0 {
                    self.email_index -= 1;
                }
            }
            _ => {}
        }
        Vec::new()
    }

    fn render(&self, f: &mut Frame, area: Rect, focused: bool, ctx: &Ctx) {
        let visible_rows = area.height.saturating_sub(2) as usize;
        self.message_pane_visible_rows.set(visible_rows);

        let folder = Self::folder_to_display(ctx);
        let is_sent_folder = folder.name == "Sent" || folder.name.to_lowercase().contains("sent");

        let email_items = build_email_list_with_truncation(
            &folder.emails,
            area.width.saturating_sub(2) as usize,
            is_sent_folder,
        );

        let style = if focused {
            Style::default().fg(VulthorTheme::CYAN)
        } else {
            Style::default()
        };
        let border_style = style;

        let folder_path = match ctx.view {
            View::FolderMessages => {
                if let Some(path_indices) = crate::input::get_folder_path_from_display_index(
                    &ctx.store.root_folder,
                    ctx.folder_index,
                ) {
                    ctx.store.get_folder_path_for_indices(&path_indices)
                } else {
                    ctx.store.get_folder_path()
                }
            }
            _ => ctx.store.get_folder_path(),
        };

        let title = if folder.is_loaded {
            format!("Emails - {} ({})", folder_path, folder.emails.len())
        } else {
            format!("Emails - {} ({}/...)", folder_path, folder.emails.len())
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .style(border_style)
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

    fn on_key(&mut self, key: KeyEvent, _ctx: &Ctx) -> Option<Msg> {
        if !key.modifiers.is_empty() && !matches!(key.code, KeyCode::Up | KeyCode::Down) {
            return None;
        }
        match key.code {
            KeyCode::Char('j') => Some(Msg::MessageMove(Dir::Down)),
            KeyCode::Char('k') => Some(Msg::MessageMove(Dir::Up)),
            KeyCode::Down => Some(Msg::MessageMove(Dir::Down)),
            KeyCode::Up => Some(Msg::MessageMove(Dir::Up)),
            _ => None,
        }
    }
}

/// Render attachments pane. Owned by `MessagesComponent` because
/// `attachment_index` lives here (per the Phase 0.2.3 bead). The
/// attachments pane is only shown in `View::MessagesAttachments`
/// (content-pane-hidden mode); ui.rs invokes this explicitly when
/// that view is active.
impl MessagesComponent {
    pub fn render_attachments(&self, f: &mut Frame, area: Rect, focused: bool, ctx: &Ctx) {
        use ratatui::widgets::Paragraph;

        let border_style = if focused {
            Style::default().fg(VulthorTheme::ACCENT_LIGHT)
        } else {
            Style::default()
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .style(border_style)
            .title("Attachments");

        let Some(email) = ctx.store.get_selected_email() else {
            let paragraph = Paragraph::new("Select an email to view attachments")
                .block(block)
                .style(Style::default().fg(VulthorTheme::GRAY_DARK));
            f.render_widget(paragraph, area);
            return;
        };

        if email.attachments.is_empty() {
            let text = match email.load_state {
                crate::email::EmailLoadState::HeadersOnly => "Loading attachments…",
                crate::email::EmailLoadState::FullyLoaded => "No attachments in this email",
            };
            let paragraph = Paragraph::new(text)
                .block(block)
                .style(Style::default().fg(VulthorTheme::GRAY_DARK));
            f.render_widget(paragraph, area);
            return;
        }

        let items: Vec<ListItem> = email
            .attachments
            .iter()
            .enumerate()
            .map(|(i, attachment)| {
                let size_str = if attachment.size < 1024 {
                    format!("{} B", attachment.size)
                } else if attachment.size < 1024 * 1024 {
                    format!("{:.1} KB", attachment.size as f64 / 1024.0)
                } else {
                    format!("{:.1} MB", attachment.size as f64 / (1024.0 * 1024.0))
                };
                let content = format!(
                    "{:2}. {} ({}) - {}",
                    i + 1,
                    attachment.filename,
                    attachment.content_type,
                    size_str
                );
                ListItem::new(content)
            })
            .collect();

        let list = List::new(items).block(block).highlight_style(
            Style::default()
                .bg(VulthorTheme::SELECTION_BG)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        );

        let mut state = self.attachment_list_state.borrow_mut();
        state.select(Some(self.attachment_index));
        f.render_stateful_widget(list, area, &mut *state);
    }
}

// Email-list row composition. Lifted verbatim from `ui.rs` so the
// extraction is behaviorally identical; the original copy will be
// removed once ui.rs delegates entirely.

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
) -> Vec<ListItem<'_>> {
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
                extract_email_address(&email.headers.to)
            } else {
                extract_email_address(&email.headers.from)
            };
            let truncated_sender = truncate_with_ellipsis(&sender, from_width);
            let padded_sender = pad_to_width(&truncated_sender, from_width);
            spans.push(Span::styled(padded_sender, style));
            spans.push(Span::raw("  "));

            let subject = if email.headers.subject.is_empty() {
                "(No Subject)"
            } else {
                &email.headers.subject
            };
            let truncated_subject = truncate_with_ellipsis(subject, subject_width);
            let padded_subject = pad_to_width(&truncated_subject, subject_width);
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

            let date_str = format_email_date(&email.headers.date);
            spans.push(Span::styled(date_str, style));

            ListItem::new(Line::from(spans))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::email::{Email, EmailStore, Folder};
    use crossterm::event::KeyModifiers;
    use std::path::PathBuf;

    fn store_with_inbox_emails(n: usize) -> EmailStore {
        let mut store = EmailStore::new(PathBuf::from("/tmp"));
        let mut inbox = Folder::new("INBOX".to_string(), PathBuf::from("/tmp/INBOX"));
        for i in 0..n {
            inbox.add_email(Email::new(PathBuf::from(format!("/tmp/INBOX/m{}", i))));
        }
        inbox.is_loaded = true;
        store.root_folder.add_subfolder(inbox);
        store.enter_folder_by_path(&[0]);
        store
    }

    fn ctx<'a>(
        theme: &'a VulthorTheme,
        config: &'a Config,
        store: &'a EmailStore,
        view: View,
        folder_index: usize,
    ) -> Ctx<'a> {
        Ctx {
            theme,
            config,
            store,
            view,
            folder_index,
        }
    }

    #[test]
    fn message_move_down_clamps_at_end() {
        let store = store_with_inbox_emails(3);
        let (theme, config) = (VulthorTheme, Config::default());
        // FolderMessages view: emails resolve via folder_index (the
        // INBOX is the only folder; index 0).
        let c = ctx(&theme, &config, &store, View::FolderMessages, 0);

        let mut comp = MessagesComponent::new();
        comp.handle_msg(&Msg::MessageMove(Dir::Down), &c);
        comp.handle_msg(&Msg::MessageMove(Dir::Down), &c);
        comp.handle_msg(&Msg::MessageMove(Dir::Down), &c);
        // At end — further Down is a no-op.
        comp.handle_msg(&Msg::MessageMove(Dir::Down), &c);
        assert_eq!(comp.email_index, 2);
    }

    #[test]
    fn message_move_up_clamps_at_zero() {
        let store = store_with_inbox_emails(3);
        let (theme, config) = (VulthorTheme, Config::default());
        let c = ctx(&theme, &config, &store, View::FolderMessages, 0);

        let mut comp = MessagesComponent::new();
        comp.handle_msg(&Msg::MessageMove(Dir::Up), &c);
        assert_eq!(comp.email_index, 0);
    }

    #[test]
    fn on_key_maps_jk_to_message_move() {
        let store = store_with_inbox_emails(2);
        let (theme, config) = (VulthorTheme, Config::default());
        let c = ctx(&theme, &config, &store, View::MessagesContent, 0);
        let mut comp = MessagesComponent::new();

        let j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(comp.on_key(j, &c), Some(Msg::MessageMove(Dir::Down)));
        let k = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(comp.on_key(k, &c), Some(Msg::MessageMove(Dir::Up)));
    }
}
