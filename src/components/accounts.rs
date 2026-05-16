// `AccountsComponent` — render-only scaffold (Phase 0.2.4, vu-501).
//
// Per DESIGN-COMPONENTS.md § "Migration order" step 4, this component
// locks in the Accounts pane's slot in the layout and message addressing
// before Phase 1 (multi-account support) lands. It owns no state and
// reacts to no messages yet; `handle_msg` is a no-op and `render` paints
// a single-line "Coming in Phase 1" placeholder.
//
// Phase 1 will populate it with account selection, per-account unread
// counts, and `Msg::AccountSelect` handling (the `Msg` variant is
// already defined in `msg.rs`).

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
pub const ACCOUNTS_TITLE: &str = "Accounts";

/// Placeholder body until Phase 1 wires multi-account in. Tests assert
/// this exact string renders when the pane is active.
pub const ACCOUNTS_PLACEHOLDER: &str = "Coming in Phase 1";

#[derive(Default)]
pub struct AccountsComponent;

impl AccountsComponent {
    pub fn new() -> Self {
        Self
    }
}

impl Component for AccountsComponent {
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
            .title(ACCOUNTS_TITLE);

        let body = Paragraph::new(ACCOUNTS_PLACEHOLDER)
            .block(block)
            .style(Style::default().fg(VulthorTheme::GRAY_DARK))
            .wrap(Wrap { trim: true });

        f.render_widget(body, area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{AccountId, Dir};
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

    /// Render the component into a TestBackend and flatten the visible
    /// cell contents into a single string. Used to assert placeholder
    /// text reaches the screen.
    fn render_to_string(c: &AccountsComponent, focused: bool) -> String {
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
        let mut c = AccountsComponent::new();
        // A spread of variants the component will eventually subscribe
        // to — until then they must all produce zero follow-ups.
        assert!(c.handle_msg(&Msg::Quit, &ctx).is_empty());
        assert!(
            c.handle_msg(&Msg::AccountSelect(AccountId::from("default")), &ctx)
                .is_empty()
        );
        assert!(c.handle_msg(&Msg::FolderMove(Dir::Down), &ctx).is_empty());
    }

    #[test]
    fn render_paints_placeholder_text() {
        let c = AccountsComponent::new();
        let rendered = render_to_string(&c, true);
        assert!(
            rendered.contains(ACCOUNTS_PLACEHOLDER),
            "expected placeholder text in render output, got:\n{}",
            rendered
        );
    }

    #[test]
    fn render_paints_pane_title() {
        let c = AccountsComponent::new();
        let rendered = render_to_string(&c, false);
        assert!(
            rendered.contains(ACCOUNTS_TITLE),
            "expected '{}' title in render output, got:\n{}",
            ACCOUNTS_TITLE,
            rendered
        );
    }
}
