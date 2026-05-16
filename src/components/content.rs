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
use crate::theme::VulthorTheme;

use super::{Component, Ctx, Dir, Msg};

/// How many lines PageUp/PageDown moves through the body. Matches the
/// legacy `input::handle_main_view_input` constant of 10.
const PAGE_SCROLL_STEP: usize = 10;

pub struct ContentComponent {
    pub scroll_offset: usize,
    scrollbar_state: RefCell<ScrollbarState>,
}

impl ContentComponent {
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
            Style::default().fg(VulthorTheme::CYAN_LIGHT)
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
                .style(Style::default().fg(VulthorTheme::GRAY_DARK));
            f.render_widget(paragraph, area);
        }
    }

    fn on_key(&mut self, key: KeyEvent, _ctx: &Ctx) -> Option<Msg> {
        // Allow arrow + paging keys with any modifier (terminals
        // sometimes report Shift+Arrow with a modifier), but reject
        // modified letter keys (Alt+j etc.) so global shortcuts don't
        // accidentally scroll.
        if !key.modifiers.is_empty()
            && !matches!(
                key.code,
                KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown
            )
        {
            return None;
        }
        match key.code {
            KeyCode::Char('j') => Some(Msg::ContentScroll(Dir::Down, 1)),
            KeyCode::Char('k') => Some(Msg::ContentScroll(Dir::Up, 1)),
            KeyCode::Down => Some(Msg::ContentScroll(Dir::Down, 1)),
            KeyCode::Up => Some(Msg::ContentScroll(Dir::Up, 1)),
            KeyCode::PageDown => Some(Msg::ContentScroll(Dir::Down, PAGE_SCROLL_STEP)),
            KeyCode::PageUp => Some(Msg::ContentScroll(Dir::Up, PAGE_SCROLL_STEP)),
            // Reply from Content. Mirrors the Messages-pane binding so
            // the user can press 'r' anywhere a message is in focus.
            // vu-l1y will extend with gr/f/R.
            KeyCode::Char('r') => Some(Msg::DraftStart(
                crate::components::ReplyKind::Reply,
                String::new(),
            )),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::email::EmailStore;
    use crossterm::event::KeyModifiers;
    use std::path::PathBuf;

    fn ctx<'a>(theme: &'a VulthorTheme, config: &'a Config, store: &'a EmailStore) -> Ctx<'a> {
        Ctx {
            theme,
            config,
            store,
        }
    }

    fn fixtures() -> (VulthorTheme, Config, EmailStore) {
        (
            VulthorTheme,
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

    #[test]
    fn on_key_jk_maps_to_content_scroll() {
        let (theme, config, store) = fixtures();
        let ctx = ctx(&theme, &config, &store);
        let mut c = ContentComponent::new();
        let j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(c.on_key(j, &ctx), Some(Msg::ContentScroll(Dir::Down, 1)));
        let k = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(c.on_key(k, &ctx), Some(Msg::ContentScroll(Dir::Up, 1)));
    }

    #[test]
    fn on_key_arrows_map_to_single_line_scroll() {
        let (theme, config, store) = fixtures();
        let ctx = ctx(&theme, &config, &store);
        let mut c = ContentComponent::new();
        let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(c.on_key(down, &ctx), Some(Msg::ContentScroll(Dir::Down, 1)));
        let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(c.on_key(up, &ctx), Some(Msg::ContentScroll(Dir::Up, 1)));
    }

    #[test]
    fn on_key_pageup_pagedown_step_by_ten() {
        let (theme, config, store) = fixtures();
        let ctx = ctx(&theme, &config, &store);
        let mut c = ContentComponent::new();
        let pd = KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE);
        assert_eq!(
            c.on_key(pd, &ctx),
            Some(Msg::ContentScroll(Dir::Down, PAGE_SCROLL_STEP)),
        );
        let pu = KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE);
        assert_eq!(
            c.on_key(pu, &ctx),
            Some(Msg::ContentScroll(Dir::Up, PAGE_SCROLL_STEP)),
        );
    }

    #[test]
    fn on_key_ignores_modified_letter_keys() {
        let (theme, config, store) = fixtures();
        let ctx = ctx(&theme, &config, &store);
        let mut c = ContentComponent::new();
        let alt_j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::ALT);
        assert_eq!(c.on_key(alt_j, &ctx), None);
    }
}
