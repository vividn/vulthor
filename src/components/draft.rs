// `DraftComponent` — pre-send pane (Phase 2.b, vu-0gj).
//
// Holds the in-flight `Compose` plus a small status machine. When
// `state.is_some()` the pane renders headers / body against the live
// draft; otherwise it renders a tombstone so the view progression still
// has a pane to land on.
//
// Scope (minimal, iteration 1):
//   - State: original_message_id + Compose + DraftStatus
//   - Messages: DraftStart (begin), DraftEditorExited (parsed back),
//     DraftSend (handed to msmtp)
//   - Render: To / Cc / Subject header strip, scrollable body
//
// Out of scope (follow-ups tracked as separate beads):
//   - Per-pane key handling in AppRoot (e/a/t/c/b/S/D/q binding)
//   - Editor relaunch + msmtp pipeline execution from this pane
//   - Attachment list rendering and add/remove
//   - Side-by-side original-message preview when width allows

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout as RatatuiLayout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::compose::Compose;
use crate::theme::VulthorTheme;

use super::{Component, Ctx, MessageId, Msg, ReplyKind};

/// Pane title — tests grep for this exact string.
pub const DRAFT_TITLE: &str = "Draft";

/// Tombstone body when no draft exists. Replaces the old
/// "Coming in Phase 2" placeholder per vu-0gj acceptance.
pub const DRAFT_PLACEHOLDER: &str = "No draft — press r on a message to start one.";

/// Lifecycle of an in-flight draft. Drives the status footer and
/// (eventually) which action keys are legal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DraftStatus {
    /// `$EDITOR` currently has the terminal. Reserved for the brief
    /// window AppRoot is suspended around the editor invocation — the
    /// pane should not actually render in this state, but the variant
    /// makes the state machine total.
    Editing,
    /// Editor exited, draft parsed, awaiting `S` to send.
    ReadyToSend,
    /// `msmtp` pipe is open. Send keys disabled until it resolves.
    Sending,
    /// Send failed. Carries the error message for the footer.
    Failed(String),
}

/// In-memory state when a draft is active. `original_message_id` is
/// the email the draft is a reply/forward of — `None` for fresh
/// compositions, which is reserved for a later phase (current message
/// flow only emits `DraftStart` from a selected message).
#[derive(Debug, Clone)]
pub struct DraftState {
    pub original_message_id: MessageId,
    pub reply_kind: ReplyKind,
    pub compose: Compose,
    pub status: DraftStatus,
}

/// Draft pane. Stateless when no draft is in flight; otherwise owns
/// the compose buffer until send-or-discard.
#[derive(Default)]
pub struct DraftComponent {
    state: Option<DraftState>,
}

impl DraftComponent {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether an in-flight draft exists. AppRoot reads this to decide
    /// view progression (Content → Draft is gated on it).
    pub fn has_draft(&self) -> bool {
        self.state.is_some()
    }

    /// Borrow the current draft state. `None` between sessions or
    /// after discard.
    pub fn state(&self) -> Option<&DraftState> {
        self.state.as_ref()
    }

    /// Replace the live draft's [`Compose`] payload. AppRoot uses this
    /// after building a reply / forward template (Phase 2.d) so the
    /// editor sees a populated `To` / `Subject` / quoted body, and
    /// again after the editor exits to install the parsed result.
    /// No-op when there is no draft in flight.
    pub fn set_compose(&mut self, compose: Compose) {
        if let Some(state) = self.state.as_mut() {
            state.compose = compose;
        }
    }

    /// Force a particular status on the current draft. AppRoot uses
    /// this to flip a ReplyLater draft straight to `ReadyToSend` (it
    /// skips the editor) and to surface send failures via
    /// `DraftStatus::Failed`. No-op when there is no draft in flight.
    pub fn set_status(&mut self, status: DraftStatus) {
        if let Some(state) = self.state.as_mut() {
            state.status = status;
        }
    }

    /// Discard the in-flight draft. Used when the editor launch fails
    /// before any body could be captured — leaving a phantom Editing
    /// state would lock the user out of starting a new reply.
    pub fn clear(&mut self) {
        self.state = None;
    }
}

impl Component for DraftComponent {
    fn handle_msg(&mut self, msg: &Msg, _ctx: &Ctx) -> Vec<Msg> {
        match msg {
            Msg::DraftStart(kind, message_id) => {
                // Begin a draft. The compose itself stays empty here
                // — AppRoot populates it after the editor returns via
                // `DraftEditorExited`. We mark `Editing` because the
                // editor is conceptually next, even though this
                // component is not the one that launches it.
                self.state = Some(DraftState {
                    original_message_id: message_id.clone(),
                    reply_kind: *kind,
                    compose: Compose::new(),
                    status: DraftStatus::Editing,
                });
                Vec::new()
            }
            Msg::DraftEditorExited => {
                // The editor returned cleanly; AppRoot has already
                // refreshed `compose` via direct mutation (it owns the
                // suspend/restore around `$EDITOR`). All we do is
                // advance the state machine.
                if let Some(state) = self.state.as_mut() {
                    state.status = DraftStatus::ReadyToSend;
                }
                Vec::new()
            }
            Msg::DraftSend => {
                // `S` was pressed. AppRoot pipes `compose` to msmtp;
                // we just reflect that in the footer.
                if let Some(state) = self.state.as_mut() {
                    state.status = DraftStatus::Sending;
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
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

        let Some(state) = self.state.as_ref() else {
            // No draft — render the tombstone inside the bordered block
            // so the pane geometry matches the populated case.
            let body = Paragraph::new(DRAFT_PLACEHOLDER)
                .block(block)
                .style(Style::default().fg(VulthorTheme::GRAY_DARK))
                .wrap(Wrap { trim: true });
            f.render_widget(body, area);
            return;
        };

        // Carve the bordered area into header strip / body / status footer.
        let inner = block.inner(area);
        f.render_widget(block, area);

        let chunks = RatatuiLayout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(header_lines(&state.compose) as u16),
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .split(inner);

        f.render_widget(header_paragraph(&state.compose), chunks[0]);
        f.render_widget(
            Paragraph::new(state.compose.body.clone()).wrap(Wrap { trim: false }),
            chunks[1],
        );
        f.render_widget(status_paragraph(&state.status), chunks[2]);
    }
}

/// Number of header rows the strip will paint. Cc/Bcc are conditional
/// so the strip stays compact for typical drafts.
fn header_lines(c: &Compose) -> usize {
    // To + Subject always; Cc and Bcc only when non-empty.
    let mut n = 2;
    if !c.cc.is_empty() {
        n += 1;
    }
    if !c.bcc.is_empty() {
        n += 1;
    }
    n
}

fn header_paragraph(c: &Compose) -> Paragraph<'_> {
    let label_style = Style::default()
        .fg(VulthorTheme::CYAN_LIGHT)
        .add_modifier(Modifier::BOLD);

    let mut lines = Vec::with_capacity(4);
    lines.push(Line::from(vec![
        Span::styled("To:      ", label_style),
        Span::raw(c.to.clone()),
    ]));
    if !c.cc.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("Cc:      ", label_style),
            Span::raw(c.cc.clone()),
        ]));
    }
    if !c.bcc.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("Bcc:     ", label_style),
            Span::raw(c.bcc.clone()),
        ]));
    }
    lines.push(Line::from(vec![
        Span::styled("Subject: ", label_style),
        Span::raw(c.subject.clone()),
    ]));
    Paragraph::new(lines).wrap(Wrap { trim: false })
}

fn status_paragraph(status: &DraftStatus) -> Paragraph<'_> {
    let (label, style) = match status {
        DraftStatus::Editing => (
            "● editing".to_string(),
            Style::default().fg(VulthorTheme::GRAY_DARK),
        ),
        DraftStatus::ReadyToSend => (
            "● ready  (S to send · e to edit · q to discard)".to_string(),
            Style::default().fg(VulthorTheme::CYAN_LIGHT),
        ),
        DraftStatus::Sending => (
            "● sending…".to_string(),
            Style::default()
                .fg(VulthorTheme::CYAN_LIGHT)
                .add_modifier(Modifier::BOLD),
        ),
        DraftStatus::Failed(reason) => (
            format!("● send failed: {}", reason),
            Style::default()
                .fg(VulthorTheme::CYAN_LIGHT)
                .add_modifier(Modifier::REVERSED),
        ),
    };
    Paragraph::new(label).style(style)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{Dir, MessageId};
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

    fn render_to_string(c: &DraftComponent, focused: bool, w: u16, h: u16) -> String {
        let backend = TestBackend::new(w, h);
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
    fn new_has_no_draft() {
        let c = DraftComponent::new();
        assert!(!c.has_draft());
        assert!(c.state().is_none());
    }

    #[test]
    fn draft_start_initializes_state_in_editing() {
        let mut c = DraftComponent::new();
        let (theme, config, store) = fixtures();
        let ctx = ctx(&theme, &config, &store);
        let follow = c.handle_msg(
            &Msg::DraftStart(ReplyKind::Reply, MessageId::from("msg-42")),
            &ctx,
        );
        assert!(follow.is_empty(), "DraftStart should not emit follow-ups");
        let state = c.state().expect("state after DraftStart");
        assert_eq!(state.original_message_id, "msg-42");
        assert_eq!(state.reply_kind, ReplyKind::Reply);
        assert_eq!(state.status, DraftStatus::Editing);
        assert_eq!(state.compose, Compose::new());
    }

    #[test]
    fn editor_exited_advances_status_to_ready() {
        let mut c = DraftComponent::new();
        let (theme, config, store) = fixtures();
        let ctx = ctx(&theme, &config, &store);
        c.handle_msg(
            &Msg::DraftStart(ReplyKind::Reply, MessageId::from("m")),
            &ctx,
        );
        c.handle_msg(&Msg::DraftEditorExited, &ctx);
        assert_eq!(c.state().unwrap().status, DraftStatus::ReadyToSend);
    }

    #[test]
    fn editor_exited_without_draft_is_noop() {
        let mut c = DraftComponent::new();
        let (theme, config, store) = fixtures();
        let ctx = ctx(&theme, &config, &store);
        let out = c.handle_msg(&Msg::DraftEditorExited, &ctx);
        assert!(out.is_empty());
        assert!(c.state().is_none());
    }

    #[test]
    fn send_transitions_status_to_sending() {
        let mut c = DraftComponent::new();
        let (theme, config, store) = fixtures();
        let ctx = ctx(&theme, &config, &store);
        c.handle_msg(
            &Msg::DraftStart(ReplyKind::ReplyAll, MessageId::from("m")),
            &ctx,
        );
        c.handle_msg(&Msg::DraftEditorExited, &ctx);
        c.handle_msg(&Msg::DraftSend, &ctx);
        assert_eq!(c.state().unwrap().status, DraftStatus::Sending);
    }

    #[test]
    fn send_without_draft_is_noop() {
        let mut c = DraftComponent::new();
        let (theme, config, store) = fixtures();
        let ctx = ctx(&theme, &config, &store);
        let out = c.handle_msg(&Msg::DraftSend, &ctx);
        assert!(out.is_empty());
        assert!(c.state().is_none());
    }

    #[test]
    fn unrelated_messages_are_ignored() {
        let mut c = DraftComponent::new();
        let (theme, config, store) = fixtures();
        let ctx = ctx(&theme, &config, &store);
        assert!(c.handle_msg(&Msg::Quit, &ctx).is_empty());
        assert!(c.handle_msg(&Msg::FolderMove(Dir::Down), &ctx).is_empty());
        assert!(c.state().is_none());
    }

    #[test]
    fn render_without_state_paints_tombstone() {
        let c = DraftComponent::new();
        let rendered = render_to_string(&c, true, 60, 5);
        assert!(
            rendered.contains(DRAFT_PLACEHOLDER),
            "expected tombstone in render output, got:\n{}",
            rendered
        );
        assert!(rendered.contains(DRAFT_TITLE));
    }

    #[test]
    fn render_without_state_does_not_show_old_phase2_text() {
        // vu-0gj acceptance: no longer a "Coming in Phase 2" placeholder.
        let c = DraftComponent::new();
        let rendered = render_to_string(&c, false, 60, 5);
        assert!(
            !rendered.contains("Coming in Phase 2"),
            "old placeholder leaked into output:\n{}",
            rendered
        );
    }

    #[test]
    fn render_with_state_shows_headers_and_body() {
        let mut c = DraftComponent::new();
        let (theme, config, store) = fixtures();
        let ctx = ctx(&theme, &config, &store);
        c.handle_msg(
            &Msg::DraftStart(ReplyKind::Reply, MessageId::from("m")),
            &ctx,
        );
        c.state.as_mut().unwrap().compose = Compose {
            from: "me@example.com".into(),
            to: "alice@example.com".into(),
            subject: "Re: lunch".into(),
            body: "Sounds good.\n".into(),
            ..Compose::new()
        };
        c.handle_msg(&Msg::DraftEditorExited, &ctx);
        let rendered = render_to_string(&c, true, 60, 10);
        assert!(rendered.contains("To:"), "missing To: in:\n{}", rendered);
        assert!(rendered.contains("alice@example.com"));
        assert!(rendered.contains("Subject:"));
        assert!(rendered.contains("Re: lunch"));
        assert!(rendered.contains("Sounds good"));
        assert!(rendered.contains("ready"));
    }

    #[test]
    fn render_with_cc_includes_cc_row() {
        let mut c = DraftComponent::new();
        let (theme, config, store) = fixtures();
        let ctx = ctx(&theme, &config, &store);
        c.handle_msg(
            &Msg::DraftStart(ReplyKind::ReplyAll, MessageId::from("m")),
            &ctx,
        );
        c.state.as_mut().unwrap().compose = Compose {
            to: "alice@example.com".into(),
            cc: "bob@example.com".into(),
            subject: "hi".into(),
            body: "ok".into(),
            ..Compose::new()
        };
        let rendered = render_to_string(&c, true, 60, 10);
        assert!(rendered.contains("Cc:"));
        assert!(rendered.contains("bob@example.com"));
    }

    #[test]
    fn render_without_cc_omits_cc_row() {
        let mut c = DraftComponent::new();
        let (theme, config, store) = fixtures();
        let ctx = ctx(&theme, &config, &store);
        c.handle_msg(
            &Msg::DraftStart(ReplyKind::Reply, MessageId::from("m")),
            &ctx,
        );
        c.state.as_mut().unwrap().compose = Compose {
            to: "alice@example.com".into(),
            subject: "hi".into(),
            body: "ok".into(),
            ..Compose::new()
        };
        let rendered = render_to_string(&c, true, 60, 10);
        assert!(!rendered.contains("Cc:"));
    }

    #[test]
    fn render_status_reflects_sending() {
        let mut c = DraftComponent::new();
        let (theme, config, store) = fixtures();
        let ctx = ctx(&theme, &config, &store);
        c.handle_msg(
            &Msg::DraftStart(ReplyKind::Reply, MessageId::from("m")),
            &ctx,
        );
        c.handle_msg(&Msg::DraftEditorExited, &ctx);
        c.handle_msg(&Msg::DraftSend, &ctx);
        let rendered = render_to_string(&c, true, 60, 10);
        assert!(rendered.contains("sending"));
    }

    #[test]
    fn render_status_reflects_failure() {
        let mut c = DraftComponent::new();
        let (theme, config, store) = fixtures();
        let ctx = ctx(&theme, &config, &store);
        c.handle_msg(
            &Msg::DraftStart(ReplyKind::Reply, MessageId::from("m")),
            &ctx,
        );
        c.state.as_mut().unwrap().status = DraftStatus::Failed("smtp 550".to_string());
        let rendered = render_to_string(&c, true, 60, 10);
        assert!(rendered.contains("send failed"));
        assert!(rendered.contains("smtp 550"));
    }
}
