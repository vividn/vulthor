// `DraftComponent` — render-only scaffold.
//
// Locks in the Draft pane's slot in the layout and message addressing
// before compose / reply lands. It owns no state and reacts to no
// messages yet; `handle_msg` is a no-op and `render` paints a single-
// line placeholder. The reply-draft editor lifecycle will handle
// `Msg::DraftStart` / `Msg::DraftEditorExited` / `Msg::DraftSend`
// (the variants are already defined in `msg.rs`).

use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::theme::VulthorTheme;

use super::{Component, Ctx, Msg};

/// Pane title — shared with the placeholder body so tests can grep for
/// either string.
pub const DRAFT_TITLE: &str = "Draft";

/// Placeholder body until Phase 2 wires compose/reply in. Tests assert
/// this exact string renders when the pane is active.
pub const DRAFT_PLACEHOLDER: &str = "Coming in Phase 2";

/// Draft pane scaffold. Currently a zero-state placeholder that
/// reserves the pane's slot in the layout and the message addressing.
/// Real state arrives in Phase 2 (compose / reply flow).
#[derive(Default)]
pub struct DraftComponent;

impl DraftComponent {
    /// Build the stateless component. Equivalent to
    /// `DraftComponent::default()`.
    pub fn new() -> Self {
        Self
    }
}

impl Component for DraftComponent {
    fn handle_msg(&mut self, _msg: &Msg, _ctx: &Ctx) -> Vec<Msg> {
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
            .title(DRAFT_TITLE);

        let body = Paragraph::new(DRAFT_PLACEHOLDER)
            .block(block)
            .style(Style::default().fg(VulthorTheme::GRAY_DARK))
            .wrap(Wrap { trim: true });

        f.render_widget(body, area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{Dir, MessageId, ReplyKind};
    use crate::config::Config;
    use crate::email::EmailStore;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::path::PathBuf;

    fn fixtures() -> (VulthorTheme, Config, EmailStore) {
        (
            VulthorTheme,
            Config::default(),
            EmailStore::new(PathBuf::from("/tmp")),
        )
    }

    fn ctx<'a>(theme: &'a VulthorTheme, config: &'a Config, store: &'a EmailStore) -> Ctx<'a> {
        Ctx {
            theme,
            config,
            store,
        }
    }

    fn render_to_string(c: &DraftComponent, focused: bool) -> String {
        let backend = TestBackend::new(40, 5);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let (theme, config, store) = fixtures();
        let ctx = ctx(&theme, &config, &store);
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
    fn handle_msg_is_no_op() {
        let (theme, config, store) = fixtures();
        let ctx = ctx(&theme, &config, &store);
        let mut c = DraftComponent::new();
        // A spread of variants the component will eventually subscribe
        // to — until then they must all produce zero follow-ups.
        assert!(c.handle_msg(&Msg::Quit, &ctx).is_empty());
        assert!(
            c.handle_msg(
                &Msg::DraftStart(ReplyKind::Reply, MessageId::from("msg-1")),
                &ctx
            )
            .is_empty()
        );
        assert!(c.handle_msg(&Msg::DraftEditorExited, &ctx).is_empty());
        assert!(c.handle_msg(&Msg::DraftSend, &ctx).is_empty());
        assert!(c.handle_msg(&Msg::FolderMove(Dir::Down), &ctx).is_empty());
    }

    #[test]
    fn render_paints_placeholder_text() {
        let c = DraftComponent::new();
        let rendered = render_to_string(&c, true);
        assert!(
            rendered.contains(DRAFT_PLACEHOLDER),
            "expected placeholder text in render output, got:\n{}",
            rendered
        );
    }

    #[test]
    fn render_paints_pane_title() {
        let c = DraftComponent::new();
        let rendered = render_to_string(&c, false);
        assert!(
            rendered.contains(DRAFT_TITLE),
            "expected '{}' title in render output, got:\n{}",
            DRAFT_TITLE,
            rendered
        );
    }
}
