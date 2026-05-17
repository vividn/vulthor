// `ContentComponent` — content pane.
//
// Owns the content-pane scroll position (`scroll_offset`). Translates
// Content-pane keys (j/k/Up/Down/PageUp/PageDown) into messages, and
// renders the headers + body + scrollbar against the selected email.
//
// **`RefCell<ScrollbarState>`.** Ratatui's `render_stateful_widget`
// needs `&mut state`, but `Component::render` takes `&self`. The
// component owns the state in a cell, mirroring what `FoldersComponent`
// does for `ListState`.

use std::cell::RefCell;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Margin, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap},
};

use crate::email::{Attachment, EmailLoadState};
use crate::theme::Theme;

use super::{Component, Ctx, Dir, Msg};

/// How many lines PageUp/PageDown moves through the body. Matches the
/// legacy `input::handle_main_view_input` constant of 10.
pub(crate) const PAGE_SCROLL_STEP: usize = 10;

/// Content pane state. Holds the scroll offset for the body and the
/// scrollbar's ratatui state.
pub struct ContentComponent {
    /// Lines scrolled past the top of the body. `j`/`k` and arrows
    /// step by 1; PageUp/PageDown step by `PAGE_SCROLL_STEP`.
    pub scroll_offset: usize,
    /// Index of the focused row in the per-email attachment list shown
    /// below the body. AppRoot reads this when resolving the
    /// keymap-sentinel `Msg::AttachmentOpen(0)` to the actual
    /// attachment the user wants to open. Resets to 0 whenever email
    /// selection changes.
    pub attachment_focus: usize,
    /// Per-session "force plain text" toggle (vu-c1s). When true the
    /// content pane skips any HTML→text fallback: `body_plain` is shown
    /// verbatim, or the literal `"(no plain part)"` marker if missing.
    /// Default `false`; flipped by `Msg::TogglePlaintext` (Shift+P).
    pub prefer_plaintext: bool,
    scrollbar_state: RefCell<ScrollbarState>,
}

impl ContentComponent {
    /// Build a fresh content pane with scroll at the top of the body
    /// and HTML rendering enabled (the legacy default).
    pub fn new() -> Self {
        Self::with_prefer_plaintext(false)
    }

    /// Build a content pane seeding `prefer_plaintext` to `initial`.
    /// `AppRoot::with_config` passes `config.render.prefer_plaintext`
    /// so a static opt-in takes effect on the first frame.
    pub fn with_prefer_plaintext(initial: bool) -> Self {
        Self {
            scroll_offset: 0,
            attachment_focus: 0,
            prefer_plaintext: initial,
            scrollbar_state: RefCell::new(ScrollbarState::default()),
        }
    }
}

impl Default for ContentComponent {
    fn default() -> Self {
        Self::new()
    }
}

impl Component for ContentComponent {
    fn handle_msg(&mut self, msg: &Msg, _ctx: &Ctx) -> Vec<Msg> {
        match msg {
            Msg::ContentScroll(Dir::Down, n) => {
                // Saturating add mirrors the legacy `App::scroll`'s TODO
                // for bounds checking — no upper clamp yet because the
                // body line count isn't easily available off the render
                // path. Ratatui's `Paragraph::scroll` clamps the visible
                // offset on its own, so over-scrolling is a cosmetic
                // no-op rather than a panic.
                self.scroll_offset = self.scroll_offset.saturating_add(*n);
            }
            Msg::ContentScroll(Dir::Up, n) => {
                self.scroll_offset = self.scroll_offset.saturating_sub(*n);
            }
            // Any folder-level navigation invalidates the current scroll
            // position. Matches the resets in
            // `AppRoot::enter_selected_folder_async`,
            // `apply_root(FolderExitParent)`, and the legacy
            // `input::handle_back_navigation` /
            // `handle_folder_selection_and_switch_view` paths.
            Msg::FolderEnter | Msg::FolderExitParent | Msg::FolderMove(_) => {
                self.scroll_offset = 0;
                self.attachment_focus = 0;
            }
            // Selecting a different email invalidates the focused
            // attachment row from the prior email.
            Msg::MessageMove(_) | Msg::MessageOpen(_) => {
                self.attachment_focus = 0;
            }
            _ => {}
        }
        Vec::new()
    }

    fn render(&self, f: &mut Frame, area: Rect, focused: bool, ctx: &Ctx) {
        let border_style = if focused {
            Style::default().fg(ctx.theme.cyan_light)
        } else {
            Style::default()
        };

        let email_info = ctx.store.get_selected_email_headers();

        if let Some(email) = email_info {
            // Attachment strip sits below the body when the email has
            // any. Its height = list rows + 2 for the bordered block;
            // we cap at 8 rows to keep the body usable for messages
            // with long attachment lists.
            let attachment_rows = email.attachments.len();
            let attachment_strip = if attachment_rows == 0 {
                0
            } else {
                (attachment_rows.min(6) as u16) + 2
            };
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(6),
                    Constraint::Min(0),
                    Constraint::Length(attachment_strip),
                ])
                .split(area);

            let header_block = Block::default()
                .borders(Borders::ALL)
                .style(border_style)
                .title("Headers");
            let header_text = email.get_header_display();
            let header_paragraph = Paragraph::new(header_text.as_str())
                .block(header_block)
                .wrap(Wrap { trim: true });
            f.render_widget(header_paragraph, chunks[0]);

            let body_title = if email.has_attachments() {
                format!("Content ({} attachments)", email.attachment_count())
            } else {
                "Content".to_string()
            };

            let body_block = Block::default()
                .borders(Borders::ALL)
                .style(border_style)
                .title(body_title);

            // Non-blocking read: the body loader (`BodyLoader`) parses
            // bodies off-thread. Until it lands a result, `body_text`
            // is empty and `load_state` is `HeadersOnly`. Show a
            // placeholder so the user knows selection succeeded.
            let body_text = match email.load_state {
                EmailLoadState::HeadersOnly => "Loading body…".to_string(),
                EmailLoadState::FullyLoaded => {
                    ctx.store
                        .get_selected_email_markdown_with_pref(self.prefer_plaintext)
                        .unwrap_or_default()
                }
            };

            let body_paragraph = Paragraph::new(body_text.as_str())
                .block(body_block)
                .wrap(Wrap { trim: true })
                .scroll((self.scroll_offset as u16, 0));
            f.render_widget(body_paragraph, chunks[1]);

            if focused {
                let scrollbar = Scrollbar::default()
                    .orientation(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(Some("↑"))
                    .end_symbol(Some("↓"));

                let mut state = self.scrollbar_state.borrow_mut();
                *state = ScrollbarState::default()
                    .content_length(body_text.lines().count())
                    .position(self.scroll_offset);

                f.render_stateful_widget(
                    scrollbar,
                    chunks[1].inner(Margin {
                        vertical: 1,
                        horizontal: 1,
                    }),
                    &mut *state,
                );
            }

            if attachment_strip > 0 {
                render_attachment_strip(
                    f,
                    chunks[2],
                    &email.attachments,
                    self.attachment_focus,
                    border_style,
                    ctx.theme,
                );
            }
        } else {
            let block = Block::default()
                .borders(Borders::ALL)
                .style(border_style)
                .title("Content");

            let current_folder = ctx.store.get_current_folder();
            let text = if current_folder.emails.is_empty() {
                "No emails in this folder"
            } else {
                "Select an email to view its content"
            };

            let paragraph = Paragraph::new(text)
                .block(block)
                .style(Style::default().fg(ctx.theme.gray_dark));
            f.render_widget(paragraph, area);
        }
    }

    fn on_key(&mut self, _key: KeyEvent, _ctx: &Ctx) -> Option<Msg> {
        // Every Content-pane key (`j`/`k`/arrows/PageUp/PageDown)
        // resolves through the central `AppRoot::action_to_msg` keymap
        // dispatch. Keeping this trait method as a no-op satisfies the
        // Component contract and leaves a seam for future
        // Content-local sequence keys.
        None
    }
}

/// Format a byte count as `"123 B"`, `"4.5 KB"`, or `"12.3 MB"`.
/// Standalone so tests can exercise it directly.
pub(crate) fn format_attachment_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// Render the attachment strip block at `area` with `focus_index`
/// highlighted. Caller is responsible for sizing `area` to fit the
/// list (rows + 2 for the bordered block).
fn render_attachment_strip(
    f: &mut Frame,
    area: Rect,
    attachments: &[Attachment],
    focus_index: usize,
    border_style: Style,
    theme: &Theme,
) {
    let title = format!("Attachments ({})", attachments.len());
    let block = Block::default()
        .borders(Borders::ALL)
        .style(border_style)
        .title(title);

    let lines: Vec<Line> = attachments
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let label = format!(" {} ({})", a.filename, format_attachment_size(a.size));
            if i == focus_index {
                Line::from(Span::styled(
                    format!("▸{}", label),
                    Style::default()
                        .fg(theme.cyan_light)
                        .add_modifier(Modifier::BOLD),
                ))
            } else {
                Line::from(Span::raw(format!(" {}", label)))
            }
        })
        .collect();

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    f.render_widget(paragraph, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::email::EmailStore;
    use crossterm::event::KeyModifiers;
    use std::path::PathBuf;

    fn ctx<'a>(theme: &'a Theme, config: &'a Config, store: &'a EmailStore) -> Ctx<'a> {
        Ctx {
            theme,
            config,
            store,
        }
    }

    fn fixtures() -> (Theme, Config, EmailStore) {
        (
            Theme::default(),
            Config::default(),
            EmailStore::new(PathBuf::from("/tmp")),
        )
    }

    #[test]
    fn content_scroll_down_increments_offset() {
        let (theme, config, store) = fixtures();
        let ctx = ctx(&theme, &config, &store);
        let mut c = ContentComponent::new();
        c.handle_msg(&Msg::ContentScroll(Dir::Down, 1), &ctx);
        assert_eq!(c.scroll_offset, 1);
        c.handle_msg(&Msg::ContentScroll(Dir::Down, 10), &ctx);
        assert_eq!(c.scroll_offset, 11);
    }

    #[test]
    fn content_scroll_up_saturates_at_zero() {
        let (theme, config, store) = fixtures();
        let ctx = ctx(&theme, &config, &store);
        let mut c = ContentComponent::new();
        c.handle_msg(&Msg::ContentScroll(Dir::Up, 5), &ctx);
        assert_eq!(c.scroll_offset, 0, "Up from zero must not underflow");
        c.scroll_offset = 3;
        c.handle_msg(&Msg::ContentScroll(Dir::Up, 10), &ctx);
        assert_eq!(c.scroll_offset, 0, "Up past zero clamps");
    }

    #[test]
    fn content_scroll_up_decrements_within_range() {
        let (theme, config, store) = fixtures();
        let ctx = ctx(&theme, &config, &store);
        let mut c = ContentComponent::new();
        c.scroll_offset = 7;
        c.handle_msg(&Msg::ContentScroll(Dir::Up, 3), &ctx);
        assert_eq!(c.scroll_offset, 4);
    }

    #[test]
    fn folder_enter_resets_scroll_offset() {
        let (theme, config, store) = fixtures();
        let ctx = ctx(&theme, &config, &store);
        let mut c = ContentComponent::new();
        c.scroll_offset = 42;
        c.handle_msg(&Msg::FolderEnter, &ctx);
        assert_eq!(c.scroll_offset, 0);
    }

    #[test]
    fn folder_exit_parent_resets_scroll_offset() {
        let (theme, config, store) = fixtures();
        let ctx = ctx(&theme, &config, &store);
        let mut c = ContentComponent::new();
        c.scroll_offset = 42;
        c.handle_msg(&Msg::FolderExitParent, &ctx);
        assert_eq!(c.scroll_offset, 0);
    }

    #[test]
    fn folder_move_resets_scroll_offset() {
        let (theme, config, store) = fixtures();
        let ctx = ctx(&theme, &config, &store);
        let mut c = ContentComponent::new();
        c.scroll_offset = 17;
        c.handle_msg(&Msg::FolderMove(Dir::Down), &ctx);
        assert_eq!(c.scroll_offset, 0);
    }

    #[test]
    fn unrelated_messages_leave_offset_unchanged() {
        let (theme, config, store) = fixtures();
        let ctx = ctx(&theme, &config, &store);
        let mut c = ContentComponent::new();
        c.scroll_offset = 5;
        c.handle_msg(&Msg::Quit, &ctx);
        c.handle_msg(&Msg::ToggleHelp, &ctx);
        c.handle_msg(&Msg::MessageMove(Dir::Down), &ctx);
        assert_eq!(c.scroll_offset, 5);
    }

    // `j`/`k`, arrow `Up`/`Down`, and `PageUp`/`PageDown` all resolve
    // via `AppRoot::action_to_msg` (centralised keymap dispatch). This
    // component's `on_key` is a no-op; the dispatch test
    // `components::root::tests::key_pagedown_in_content_pane_scrolls_by_ten`
    // exercises the full process_event → keymap → ContentScroll path.
}
