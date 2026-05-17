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
    style::Style,
    widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap},
};

use crate::email::EmailLoadState;
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
    scrollbar_state: RefCell<ScrollbarState>,
}

impl ContentComponent {
    /// Build a fresh content pane with scroll at the top of the body.
    pub fn new() -> Self {
        Self {
            scroll_offset: 0,
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
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(6), Constraint::Min(0)])
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
                    ctx.store.get_selected_email_markdown().unwrap_or_default()
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
