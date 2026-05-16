// `DraftComponent` — pre-send pane (Phase 2.b, vu-0gj).
//
// Owns the in-flight `Compose` plus a tiny status machine. While
// `state.is_some()` the pane renders headers / body / attachments
// against the live draft; otherwise it falls back to a tombstone so the
// view progression test (`l` from Content with no draft) sees the same
// pane geometry. Editor relaunch and msmtp send are coordinated by
// AppRoot — the component only emits messages.

use std::cell::RefCell;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Margin, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Scrollbar, ScrollbarOrientation,
              ScrollbarState, Wrap},
};

use crate::compose::Compose;
use crate::theme::VulthorTheme;

use super::{Component, Ctx, Dir, MessageId, Msg};

/// Pane title — shared with the placeholder body so tests can grep for
/// either string.
pub const DRAFT_TITLE: &str = "Draft";

/// Tombstone body shown when there is no in-flight draft. Kept verbatim
/// so the "no draft yet" smoke test from vu-501 still passes.
pub const DRAFT_PLACEHOLDER: &str = "No draft — press r on a message to start one.";

/// Page step for j/k scrolling. Mirrors `ContentComponent`'s PAGE_SCROLL_STEP
/// rationale.
const PAGE_SCROLL_STEP: usize = 10;

/// Lifecycle of a draft inside the pre-send pane. Drives both the
/// status footer string and what action keys are legal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DraftStatus {
    /// Editor is currently open (modal terminal handoff). Reserved for
    /// the moments AppRoot is suspended — the pane should never actually
    /// render in this state, but the variant exists so the state
    /// machine is total.
    Editing,
    /// Editor exited; the draft is parsed and ready to send.
    ReadyToSend,
    /// Send in progress (msmtp pipe open).
    Sending,
    /// Send failed; carries the user-facing reason for the status bar.
    Failed(String),
}

/// In-flight draft state. `original_message_id` ties the draft to the
/// email it replies to (for In-Reply-To/References), `compose` is the
/// editable surface, `status` drives the footer + key gating, and
/// `scroll_offset` tracks the body viewport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftState {
    pub original_message_id: MessageId,
    pub compose: Compose,
    pub status: DraftStatus,
    pub scroll_offset: usize,
}

impl DraftState {
    pub fn ready(original_message_id: MessageId, compose: Compose) -> Self {
        Self {
            original_message_id,
            compose,
            status: DraftStatus::ReadyToSend,
            scroll_offset: 0,
        }
    }
}

#[derive(Default)]
pub struct DraftComponent {
    pub state: Option<DraftState>,
    scrollbar_state: RefCell<ScrollbarState>,
}

impl DraftComponent {
    pub fn new() -> Self {
        Self {
            state: None,
            scrollbar_state: RefCell::new(ScrollbarState::default()),
        }
    }

    /// Install a freshly-parsed draft and park it at `ReadyToSend`. Used
    /// by AppRoot after a successful editor exit.
    pub fn install_compose(&mut self, original_message_id: MessageId, compose: Compose) {
        self.state = Some(DraftState::ready(original_message_id, compose));
    }

    /// Replace the in-flight `Compose` without disturbing the original
    /// message id (used by editor relaunch, where the same draft is
    /// re-edited).
    pub fn replace_compose(&mut self, compose: Compose) {
        if let Some(s) = self.state.as_mut() {
            s.compose = compose;
            s.status = DraftStatus::ReadyToSend;
        }
    }

    /// Mark the draft as currently being sent. Footer renders "Sending…"
    /// during this window.
    pub fn mark_sending(&mut self) {
        if let Some(s) = self.state.as_mut() {
            s.status = DraftStatus::Sending;
        }
    }

    /// Move into a failed state with the given reason. The draft is
    /// preserved so the user can retry.
    pub fn mark_failed(&mut self, reason: String) {
        if let Some(s) = self.state.as_mut() {
            s.status = DraftStatus::Failed(reason);
        }
    }

    /// Drop the draft entirely. Used by `Msg::ComposeDiscard` and after
    /// a successful send.
    pub fn clear(&mut self) {
        self.state = None;
    }

    /// True when there's an in-flight draft. AppRoot uses this to gate
    /// the `l` (ViewNext) override that pushes the user into ContentDraft.
    pub fn has_active_draft(&self) -> bool {
        self.state.is_some()
    }
}

impl Component for DraftComponent {
    fn handle_msg(&mut self, msg: &Msg, _ctx: &Ctx) -> Vec<Msg> {
        match msg {
            Msg::DraftScroll(Dir::Down, n) => {
                if let Some(s) = self.state.as_mut() {
                    s.scroll_offset = s.scroll_offset.saturating_add(*n);
                }
            }
            Msg::DraftScroll(Dir::Up, n) => {
                if let Some(s) = self.state.as_mut() {
                    s.scroll_offset = s.scroll_offset.saturating_sub(*n);
                }
            }
            Msg::ComposeDiscard => {
                self.clear();
            }
            _ => {}
        }
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

        let Some(state) = self.state.as_ref() else {
            let body = Paragraph::new(DRAFT_PLACEHOLDER)
                .block(block)
                .style(Style::default().fg(VulthorTheme::GRAY_DARK))
                .wrap(Wrap { trim: true });
            f.render_widget(body, area);
            return;
        };

        f.render_widget(block, area);
        let inner = area.inner(Margin { vertical: 1, horizontal: 1 });

        // Vertical layout: 5-line header strip, body fills, 1-line status,
        // attachments at the bottom (variable up to 5 lines).
        let attachments_height: u16 = if state.compose.attachments.is_empty() {
            0
        } else {
            (state.compose.attachments.len() as u16 + 2).min(7)
        };
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(5),               // headers
                Constraint::Min(1),                  // body
                Constraint::Length(1),               // status footer
                Constraint::Length(attachments_height),
            ])
            .split(inner);

        render_headers(f, chunks[0], &state.compose);
        render_body(f, chunks[1], state, &self.scrollbar_state);
        render_status(f, chunks[2], &state.status);
        if attachments_height > 0 {
            render_attachments(f, chunks[3], &state.compose);
        }
    }

    fn on_key(&mut self, key: KeyEvent, _ctx: &Ctx) -> Option<Msg> {
        // Allow Esc with any modifier (some terminals report Esc + Shift).
        if key.code == KeyCode::Esc {
            return Some(Msg::ComposeDiscard);
        }
        // Body scroll arrows tolerate modifiers (matches ContentComponent).
        if matches!(
            key.code,
            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown
        ) {
            return match key.code {
                KeyCode::Down => Some(Msg::DraftScroll(Dir::Down, 1)),
                KeyCode::Up => Some(Msg::DraftScroll(Dir::Up, 1)),
                KeyCode::PageDown => Some(Msg::DraftScroll(Dir::Down, PAGE_SCROLL_STEP)),
                KeyCode::PageUp => Some(Msg::DraftScroll(Dir::Up, PAGE_SCROLL_STEP)),
                _ => None,
            };
        }
        // Everything else: reject keys carrying modifiers other than SHIFT.
        // SHIFT is required to distinguish 'S' / 'D' from 's' / 'd' (which
        // already map to other actions when this pane isn't focused —
        // here they aren't recognized, by design).
        if !key.modifiers.is_empty() && !matches!(key.modifiers, KeyModifiers::SHIFT) {
            return None;
        }
        match key.code {
            KeyCode::Char('j') => Some(Msg::DraftScroll(Dir::Down, 1)),
            KeyCode::Char('k') => Some(Msg::DraftScroll(Dir::Up, 1)),
            KeyCode::Char('e') => Some(Msg::EditorRelaunch),
            KeyCode::Char('S') => Some(Msg::ComposeSend),
            KeyCode::Char('D') => Some(Msg::DraftSave),
            KeyCode::Char('q') => Some(Msg::ComposeDiscard),
            // Stubs for Phase 2.b: VISION.md keymap reserves these keys,
            // but the inline-prompt / file-picker UX is deferred. Returning
            // a status message keeps the keymap discoverable without
            // pretending the feature works.
            KeyCode::Char('a') => Some(Msg::StatusSet(
                "Attachment picker: not yet implemented".into(),
            )),
            KeyCode::Char('t') => Some(Msg::StatusSet(
                "Edit To inline: not yet implemented".into(),
            )),
            KeyCode::Char('c') => Some(Msg::StatusSet(
                "Edit Cc inline: not yet implemented".into(),
            )),
            KeyCode::Char('b') => Some(Msg::StatusSet(
                "Edit Bcc inline: not yet implemented".into(),
            )),
            _ => None,
        }
    }
}

fn render_headers(f: &mut Frame, area: Rect, compose: &Compose) {
    let label_style = Style::default()
        .fg(VulthorTheme::GRAY_DARK)
        .add_modifier(Modifier::BOLD);
    let mut lines: Vec<Line> = Vec::with_capacity(5);
    lines.push(Line::from(vec![
        Span::styled("To:      ", label_style),
        Span::raw(compose.to.clone()),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Cc:      ", label_style),
        Span::raw(compose.cc.clone()),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Bcc:     ", label_style),
        Span::raw(compose.bcc.clone()),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Subject: ", label_style),
        Span::raw(compose.subject.clone()),
    ]));
    if let Some(irt) = &compose.in_reply_to {
        lines.push(Line::from(vec![
            Span::styled("In-Reply-To: ", label_style),
            Span::raw(irt.clone()),
        ]));
    }
    let p = Paragraph::new(lines).wrap(Wrap { trim: false });
    f.render_widget(p, area);
}

fn render_body(
    f: &mut Frame,
    area: Rect,
    state: &DraftState,
    scrollbar_state: &RefCell<ScrollbarState>,
) {
    let body = Paragraph::new(state.compose.body.clone())
        .wrap(Wrap { trim: false })
        .scroll((state.scroll_offset as u16, 0));
    f.render_widget(body, area);

    // Scrollbar tracks total line count; ratatui's Paragraph doesn't
    // expose the rendered line count, so we approximate with the raw
    // newline count. Over-scroll is cosmetic.
    let total = state.compose.body.lines().count().max(1);
    let mut sb = scrollbar_state.borrow_mut();
    *sb = ScrollbarState::default()
        .content_length(total)
        .viewport_content_length(area.height as usize)
        .position(state.scroll_offset);
    let scrollbar = Scrollbar::default().orientation(ScrollbarOrientation::VerticalRight);
    f.render_stateful_widget(scrollbar, area, &mut sb);
}

fn render_status(f: &mut Frame, area: Rect, status: &DraftStatus) {
    let (text, color) = match status {
        DraftStatus::Editing => ("Editing in $EDITOR…".to_string(), VulthorTheme::GRAY_DARK),
        DraftStatus::ReadyToSend => (
            "Ready to send — S send  D save  e edit  q discard".to_string(),
            VulthorTheme::CYAN_LIGHT,
        ),
        DraftStatus::Sending => ("Sending…".to_string(), VulthorTheme::CYAN_LIGHT),
        DraftStatus::Failed(reason) => (format!("Send failed: {}", reason), VulthorTheme::WARNING),
    };
    let p = Paragraph::new(text).style(Style::default().fg(color));
    f.render_widget(p, area);
}

fn render_attachments(f: &mut Frame, area: Rect, compose: &Compose) {
    let items: Vec<ListItem> = compose
        .attachments
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let display = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.display().to_string());
            ListItem::new(format!("{:2}. {}", i + 1, display))
        })
        .collect();
    let block = Block::default()
        .borders(Borders::TOP)
        .title("Attachments")
        .style(Style::default().fg(VulthorTheme::GRAY_DARK));
    let list = List::new(items).block(block);
    f.render_widget(list, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::ReplyKind;
    use crate::config::Config;
    use crate::email::EmailStore;
    use crossterm::event::KeyModifiers;
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

    fn sample_compose() -> Compose {
        Compose {
            from: "tester@example.com".into(),
            to: "alice@example.com".into(),
            cc: "carol@example.com".into(),
            bcc: "".into(),
            subject: "Re: hello".into(),
            body: "Thanks for the note.\n\nMore tomorrow.\n".into(),
            in_reply_to: Some("<parent@host>".into()),
            attachments: vec![],
            signature: None,
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    fn shift(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::SHIFT)
    }

    fn render_to_string(c: &DraftComponent, focused: bool) -> String {
        let backend = TestBackend::new(60, 20);
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

    // ---- placeholder behavior (no in-flight draft) ----

    #[test]
    fn render_with_no_state_paints_tombstone() {
        let c = DraftComponent::new();
        let rendered = render_to_string(&c, true);
        assert!(
            rendered.contains(DRAFT_PLACEHOLDER),
            "expected tombstone body, got:\n{}",
            rendered
        );
        assert!(
            rendered.contains(DRAFT_TITLE),
            "expected '{}' title, got:\n{}",
            DRAFT_TITLE,
            rendered
        );
    }

    #[test]
    fn has_active_draft_reflects_state() {
        let mut c = DraftComponent::new();
        assert!(!c.has_active_draft());
        c.install_compose("msg-1".into(), sample_compose());
        assert!(c.has_active_draft());
        c.clear();
        assert!(!c.has_active_draft());
    }

    // ---- install / replace / status mutators ----

    #[test]
    fn install_compose_parks_at_ready_to_send() {
        let mut c = DraftComponent::new();
        c.install_compose("msg-1".into(), sample_compose());
        let s = c.state.as_ref().unwrap();
        assert_eq!(s.original_message_id, "msg-1");
        assert_eq!(s.status, DraftStatus::ReadyToSend);
        assert_eq!(s.scroll_offset, 0);
    }

    #[test]
    fn replace_compose_resets_status_to_ready_to_send() {
        let mut c = DraftComponent::new();
        c.install_compose("msg-1".into(), sample_compose());
        c.mark_failed("smtp down".into());
        let mut c2 = sample_compose();
        c2.subject = "Re: hello (edited)".into();
        c.replace_compose(c2);
        let s = c.state.as_ref().unwrap();
        assert_eq!(s.status, DraftStatus::ReadyToSend);
        assert_eq!(s.compose.subject, "Re: hello (edited)");
    }

    #[test]
    fn mark_sending_updates_status_only() {
        let mut c = DraftComponent::new();
        c.install_compose("msg-1".into(), sample_compose());
        c.mark_sending();
        assert_eq!(c.state.as_ref().unwrap().status, DraftStatus::Sending);
    }

    #[test]
    fn mark_failed_captures_reason() {
        let mut c = DraftComponent::new();
        c.install_compose("msg-1".into(), sample_compose());
        c.mark_failed("smtp 550".into());
        assert!(matches!(
            c.state.as_ref().unwrap().status,
            DraftStatus::Failed(ref r) if r == "smtp 550"
        ));
    }

    // ---- render with state ----

    #[test]
    fn render_with_state_shows_headers_and_body() {
        let mut c = DraftComponent::new();
        c.install_compose("msg-1".into(), sample_compose());
        let rendered = render_to_string(&c, true);
        assert!(rendered.contains("To:"), "got:\n{}", rendered);
        assert!(rendered.contains("alice@example.com"), "got:\n{}", rendered);
        assert!(rendered.contains("Subject:"), "got:\n{}", rendered);
        assert!(rendered.contains("Re: hello"), "got:\n{}", rendered);
        assert!(rendered.contains("Thanks for the note."), "got:\n{}", rendered);
    }

    #[test]
    fn render_failed_status_surfaces_reason() {
        let mut c = DraftComponent::new();
        c.install_compose("msg-1".into(), sample_compose());
        c.mark_failed("smtp 421".into());
        let rendered = render_to_string(&c, true);
        assert!(rendered.contains("Send failed"), "got:\n{}", rendered);
        assert!(rendered.contains("smtp 421"), "got:\n{}", rendered);
    }

    #[test]
    fn render_with_attachments_lists_filenames() {
        let mut compose = sample_compose();
        compose.attachments = vec![PathBuf::from("/tmp/notes.txt")];
        let mut c = DraftComponent::new();
        c.install_compose("msg-1".into(), compose);
        let rendered = render_to_string(&c, false);
        assert!(rendered.contains("notes.txt"), "got:\n{}", rendered);
    }

    // ---- on_key ----

    #[test]
    fn key_e_maps_to_editor_relaunch() {
        let (theme, config, store) = fixtures();
        let ctx = ctx(&theme, &config, &store);
        let mut c = DraftComponent::new();
        c.install_compose("msg-1".into(), sample_compose());
        assert_eq!(c.on_key(key(KeyCode::Char('e')), &ctx), Some(Msg::EditorRelaunch));
    }

    #[test]
    fn shift_s_maps_to_compose_send() {
        let (theme, config, store) = fixtures();
        let ctx = ctx(&theme, &config, &store);
        let mut c = DraftComponent::new();
        c.install_compose("msg-1".into(), sample_compose());
        assert_eq!(c.on_key(shift(KeyCode::Char('S')), &ctx), Some(Msg::ComposeSend));
    }

    #[test]
    fn shift_d_maps_to_draft_save() {
        let (theme, config, store) = fixtures();
        let ctx = ctx(&theme, &config, &store);
        let mut c = DraftComponent::new();
        c.install_compose("msg-1".into(), sample_compose());
        assert_eq!(c.on_key(shift(KeyCode::Char('D')), &ctx), Some(Msg::DraftSave));
    }

    #[test]
    fn q_and_esc_map_to_compose_discard() {
        let (theme, config, store) = fixtures();
        let ctx = ctx(&theme, &config, &store);
        let mut c = DraftComponent::new();
        c.install_compose("msg-1".into(), sample_compose());
        assert_eq!(c.on_key(key(KeyCode::Char('q')), &ctx), Some(Msg::ComposeDiscard));
        assert_eq!(c.on_key(key(KeyCode::Esc), &ctx), Some(Msg::ComposeDiscard));
    }

    #[test]
    fn jk_arrows_pageup_pagedown_scroll_body() {
        let (theme, config, store) = fixtures();
        let ctx = ctx(&theme, &config, &store);
        let mut c = DraftComponent::new();
        c.install_compose("msg-1".into(), sample_compose());
        assert_eq!(
            c.on_key(key(KeyCode::Char('j')), &ctx),
            Some(Msg::DraftScroll(Dir::Down, 1))
        );
        assert_eq!(
            c.on_key(key(KeyCode::Char('k')), &ctx),
            Some(Msg::DraftScroll(Dir::Up, 1))
        );
        assert_eq!(
            c.on_key(key(KeyCode::Down), &ctx),
            Some(Msg::DraftScroll(Dir::Down, 1))
        );
        assert_eq!(
            c.on_key(key(KeyCode::PageDown), &ctx),
            Some(Msg::DraftScroll(Dir::Down, PAGE_SCROLL_STEP))
        );
        assert_eq!(
            c.on_key(key(KeyCode::PageUp), &ctx),
            Some(Msg::DraftScroll(Dir::Up, PAGE_SCROLL_STEP))
        );
    }

    #[test]
    fn a_t_c_b_emit_not_yet_implemented_status() {
        let (theme, config, store) = fixtures();
        let ctx = ctx(&theme, &config, &store);
        let mut c = DraftComponent::new();
        c.install_compose("msg-1".into(), sample_compose());
        for ch in ['a', 't', 'c', 'b'] {
            let msg = c.on_key(key(KeyCode::Char(ch)), &ctx);
            match msg {
                Some(Msg::StatusSet(s)) => {
                    assert!(s.contains("not yet implemented"), "ch={} got {:?}", ch, s)
                }
                other => panic!("expected StatusSet for '{}', got {:?}", ch, other),
            }
        }
    }

    #[test]
    fn on_key_rejects_modified_letter_keys() {
        let (theme, config, store) = fixtures();
        let ctx = ctx(&theme, &config, &store);
        let mut c = DraftComponent::new();
        c.install_compose("msg-1".into(), sample_compose());
        let ctrl_s = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert_eq!(c.on_key(ctrl_s, &ctx), None);
        let alt_e = KeyEvent::new(KeyCode::Char('e'), KeyModifiers::ALT);
        assert_eq!(c.on_key(alt_e, &ctx), None);
    }

    // ---- handle_msg ----

    #[test]
    fn handle_msg_draft_scroll_moves_body_offset() {
        let (theme, config, store) = fixtures();
        let ctx = ctx(&theme, &config, &store);
        let mut c = DraftComponent::new();
        c.install_compose("msg-1".into(), sample_compose());
        c.handle_msg(&Msg::DraftScroll(Dir::Down, 3), &ctx);
        assert_eq!(c.state.as_ref().unwrap().scroll_offset, 3);
        c.handle_msg(&Msg::DraftScroll(Dir::Up, 100), &ctx);
        assert_eq!(c.state.as_ref().unwrap().scroll_offset, 0, "saturates");
    }

    #[test]
    fn handle_msg_compose_discard_clears_state() {
        let (theme, config, store) = fixtures();
        let ctx = ctx(&theme, &config, &store);
        let mut c = DraftComponent::new();
        c.install_compose("msg-1".into(), sample_compose());
        c.handle_msg(&Msg::ComposeDiscard, &ctx);
        assert!(c.state.is_none());
    }

    #[test]
    fn handle_msg_is_no_op_for_unrelated_variants() {
        let (theme, config, store) = fixtures();
        let ctx = ctx(&theme, &config, &store);
        let mut c = DraftComponent::new();
        c.install_compose("msg-1".into(), sample_compose());
        c.handle_msg(&Msg::Quit, &ctx);
        c.handle_msg(
            &Msg::DraftStart(ReplyKind::Reply, "msg-2".into()),
            &ctx,
        );
        // State untouched.
        assert!(c.state.is_some());
    }
}
