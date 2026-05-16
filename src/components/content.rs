// `ContentComponent` — Phase 0.2.3 (vu-3yj).
//
// Owns the content-pane scroll offset. Renders the headers + body of
// the currently selected email; produces `Msg::ContentScroll` from
// j/k/Up/Down/PageUp/PageDown when the content pane is focused.
//
// **Sole writer of `scroll_offset`.** AppRoot mirrors this value into
// `app.selection.scroll_offset` after each dispatch so legacy readers
// (`ui.rs` until it migrates fully, the legacy `input.rs` Backspace
// path that resets scroll) keep working until Step 5 retires `App`.
//
// The body text source still lives on `Email` (mutated off-thread by
// `BodyLoader`); the component reads it via `Ctx::store`. Render is
// pure: no I/O, no `Cell`s — just paragraph composition.

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

#[derive(Default)]
pub struct ContentComponent {
    pub scroll_offset: usize,
}

impl ContentComponent {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Component for ContentComponent {
    fn handle_msg(&mut self, msg: &Msg, _ctx: &Ctx) -> Vec<Msg> {
        match msg {
            Msg::ContentScroll(Dir::Down, amount) => {
                self.scroll_offset = self.scroll_offset.saturating_add(*amount);
            }
            Msg::ContentScroll(Dir::Up, amount) => {
                self.scroll_offset = self.scroll_offset.saturating_sub(*amount);
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

            let mut body_title = "Content".to_string();
            if email.has_attachments() {
                body_title = format!("Content ({} attachments)", email.attachment_count());
            }

            let body_block = Block::default()
                .borders(Borders::ALL)
                .style(border_style)
                .title(body_title);

            // Non-blocking read: the body loader parses bodies off-thread.
            // Until it lands a result, `body_text` is empty and `load_state`
            // is `HeadersOnly` — show a placeholder so the user knows
            // selection succeeded.
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

                let mut scrollbar_state = ScrollbarState::default()
                    .content_length(body_text.lines().count())
                    .position(self.scroll_offset);

                f.render_stateful_widget(
                    scrollbar,
                    chunks[1].inner(Margin {
                        vertical: 1,
                        horizontal: 1,
                    }),
                    &mut scrollbar_state,
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
        if !key.modifiers.is_empty() && !matches!(key.code, KeyCode::Up | KeyCode::Down) {
            return None;
        }
        match key.code {
            KeyCode::Char('j') => Some(Msg::ContentScroll(Dir::Down, 1)),
            KeyCode::Char('k') => Some(Msg::ContentScroll(Dir::Up, 1)),
            KeyCode::Down => Some(Msg::ContentScroll(Dir::Down, 1)),
            KeyCode::Up => Some(Msg::ContentScroll(Dir::Up, 1)),
            KeyCode::PageDown => Some(Msg::ContentScroll(Dir::Down, 10)),
            KeyCode::PageUp => Some(Msg::ContentScroll(Dir::Up, 10)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::View;
    use crate::config::Config;
    use crate::email::EmailStore;
    use crate::theme::VulthorTheme;
    use crossterm::event::KeyModifiers;
    use std::path::PathBuf;

    fn ctx_fixture() -> (VulthorTheme, Config) {
        (VulthorTheme, Config::default())
    }

    fn make_ctx<'a>(theme: &'a VulthorTheme, config: &'a Config, store: &'a EmailStore) -> Ctx<'a> {
        Ctx {
            theme,
            config,
            store,
            view: View::MessagesContent,
            folder_index: 0,
        }
    }

    #[test]
    fn scroll_down_increments_offset() {
        let store = EmailStore::new(PathBuf::from("/tmp"));
        let (theme, config) = ctx_fixture();
        let ctx = make_ctx(&theme, &config, &store);

        let mut c = ContentComponent::new();
        assert_eq!(c.scroll_offset, 0);
        c.handle_msg(&Msg::ContentScroll(Dir::Down, 5), &ctx);
        assert_eq!(c.scroll_offset, 5);
        c.handle_msg(&Msg::ContentScroll(Dir::Down, 3), &ctx);
        assert_eq!(c.scroll_offset, 8);
    }

    #[test]
    fn scroll_up_saturates_at_zero() {
        let store = EmailStore::new(PathBuf::from("/tmp"));
        let (theme, config) = ctx_fixture();
        let ctx = make_ctx(&theme, &config, &store);

        let mut c = ContentComponent { scroll_offset: 3 };
        c.handle_msg(&Msg::ContentScroll(Dir::Up, 10), &ctx);
        assert_eq!(
            c.scroll_offset, 0,
            "scroll up below zero must clamp, not panic",
        );
    }

    #[test]
    fn on_key_maps_jk_to_scroll() {
        let store = EmailStore::new(PathBuf::from("/tmp"));
        let (theme, config) = ctx_fixture();
        let ctx = make_ctx(&theme, &config, &store);
        let mut c = ContentComponent::new();

        let j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(c.on_key(j, &ctx), Some(Msg::ContentScroll(Dir::Down, 1)));
        let k = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(c.on_key(k, &ctx), Some(Msg::ContentScroll(Dir::Up, 1)));
    }

    #[test]
    fn on_key_page_keys_scroll_by_ten() {
        let store = EmailStore::new(PathBuf::from("/tmp"));
        let (theme, config) = ctx_fixture();
        let ctx = make_ctx(&theme, &config, &store);
        let mut c = ContentComponent::new();

        let pd = KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE);
        assert_eq!(c.on_key(pd, &ctx), Some(Msg::ContentScroll(Dir::Down, 10)));
        let pu = KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE);
        assert_eq!(c.on_key(pu, &ctx), Some(Msg::ContentScroll(Dir::Up, 10)));
    }

    #[test]
    fn on_key_ignores_modified_keys() {
        let store = EmailStore::new(PathBuf::from("/tmp"));
        let (theme, config) = ctx_fixture();
        let ctx = make_ctx(&theme, &config, &store);
        let mut c = ContentComponent::new();

        let alt_j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::ALT);
        assert!(c.on_key(alt_j, &ctx).is_none());
    }
}
