// `AppRoot` — the live main-loop driver (Phase 0.2.2a, vu-gje).
//
// AppRoot is now functional: it owns the `Msg` queue, intercepts global keys
// before they reach the legacy `handle_input` path, dispatches messages via
// `apply_root` (which calls back into `App` methods), and renders by
// delegating to today's `ui::UI::draw`. No panes have been migrated to
// components yet — that lands in vu-sd6 onward.
//
// **Sharing model.** `AppRoot` holds a clone of the `SharedAppState`
// (`Arc<Mutex<App>>`) so the web server keeps its existing direct access
// path. AppRoot does not own the lock; it acquires it inside `tick` and
// `render` for the duration of those operations.
//
// **Global key interception.** `handle_global_key` returns `Some(Msg)` for
// keys that don't depend on pane state: `q`, `?`, `Alt+c`, `Tab`, `BackTab`,
// `h`, and `l`. The `l` key from the Folders pane has pane-specific
// folder-enter behavior that the legacy `input.rs` path still owns; we
// return `None` in that case and let it fall through. Help-state keys also
// skip global interception so the legacy "any key exits help" behavior is
// preserved.
//
// See DESIGN-COMPONENTS.md § "Composition" for the target shape this is
// converging toward.

use std::collections::VecDeque;
use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::app::{ActivePane, App, AppState, PaneSwitchDirection, SharedAppState};
use crate::error::Result;
use crate::ui::UI;

use super::{MAX_DISPATCH_DEPTH, Msg};

pub struct AppRoot {
    state: SharedAppState,
    queue: VecDeque<Msg>,
}

impl AppRoot {
    pub fn new(state: SharedAppState) -> Self {
        Self {
            state,
            queue: VecDeque::new(),
        }
    }

    /// Enqueue a message for the next dispatch cycle. Exposed primarily for
    /// tests and future component plumbing; the runtime fills the queue via
    /// `handle_global_key` inside `tick`.
    pub fn enqueue(&mut self, msg: Msg) {
        self.queue.push_back(msg);
    }

    pub fn queue_len(&self) -> usize {
        self.queue.len()
    }

    /// Read-only handle to the shared app state, used by `main.rs` to hand
    /// a clone to the web server.
    pub fn shared_state(&self) -> SharedAppState {
        self.state.clone()
    }

    /// Render one frame. Locks the app, delegates to `ui::UI::draw`, and
    /// returns whether the loop should exit (quit state observed).
    pub fn render(
        &self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        ui: &mut UI,
    ) -> Result<bool> {
        let mut app = self.state.lock().unwrap();
        terminal.draw(|f| ui.draw(f, &mut app))?;
        Ok(app.should_quit || matches!(app.state, AppState::Quit))
    }

    /// Poll for an input event (with the same 100ms tick the legacy loop
    /// used) and process it. Returns `true` when the runtime should exit.
    pub fn tick(&mut self) -> Result<bool> {
        if !event::poll(Duration::from_millis(100))? {
            return Ok(false);
        }
        let event = event::read()?;
        self.process_event(event)
    }

    /// Apply a single input event: drain queued global messages, then fall
    /// back to the legacy `handle_input` path for keys we don't intercept.
    /// Split out from `tick` so tests can drive it without `event::poll`.
    pub fn process_event(&mut self, event: Event) -> Result<bool> {
        // Clone the Arc so the MutexGuard doesn't borrow `self.state` —
        // we need `&mut self` available for `self.drain(&mut app)` below.
        let state = self.state.clone();
        let mut app = state.lock().unwrap();

        // Status messages clear on any non-resize event — preserves the
        // pre-refactor behavior from main.rs's run_app loop.
        if !matches!(event, Event::Resize(_, _)) {
            app.clear_status();
        }

        if let Event::Key(key) = event {
            if !matches!(app.state, AppState::Help) {
                if let Some(msg) = Self::handle_global_key(key, &app.active_pane) {
                    self.queue.push_back(msg);
                    self.drain(&mut app);
                    return Ok(app.should_quit);
                }
            }
        }

        let should_quit = crate::input::handle_input(&mut app, event);
        Ok(should_quit || app.should_quit)
    }

    /// Translate a key event into a global `Msg`, or `None` if the key
    /// should fall through to pane-specific handling. Pure: no side
    /// effects, no `self` borrow — makes it trivial to unit-test in
    /// isolation from the runtime.
    fn handle_global_key(key: KeyEvent, active_pane: &ActivePane) -> Option<Msg> {
        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), m) if m.is_empty() => Some(Msg::Quit),
            (KeyCode::Char('?'), m) if m.is_empty() => Some(Msg::ToggleHelp),
            (KeyCode::Char('c'), KeyModifiers::ALT) => Some(Msg::ToggleContentPane),
            (KeyCode::Tab, _) => Some(Msg::FocusNext),
            (KeyCode::BackTab, _) => Some(Msg::FocusPrev),
            (KeyCode::Char('h'), m) if m.is_empty() => Some(Msg::ViewPrev),
            (KeyCode::Char('l'), m) if m.is_empty() => {
                // 'l' from the Folders pane triggers folder-entering logic
                // that still lives in `input.rs`. Defer to that path; the
                // pane-aware behavior moves into FoldersComponent in vu-sd6.
                if matches!(active_pane, ActivePane::Folders) {
                    None
                } else {
                    Some(Msg::ViewNext)
                }
            }
            _ => None,
        }
    }

    /// Drain the message queue. For each message, apply root-level effects
    /// (App method calls) and broadcast to components (no-op until the
    /// first component lands). Bounded by `MAX_DISPATCH_DEPTH` to catch
    /// runaway emission. Pub for tests; the runtime calls it via `tick`.
    pub fn drain(&mut self, app: &mut App) -> bool {
        let mut steps = 0usize;
        while let Some(msg) = self.queue.pop_front() {
            steps += 1;
            if steps > MAX_DISPATCH_DEPTH {
                return false;
            }
            Self::apply_root(&msg, app);
            // No components registered yet — broadcast is a no-op. When
            // FoldersComponent lands (vu-sd6), iterate components here.
        }
        true
    }

    /// Drain helper that locks internally. Used by tests; production code
    /// holds the lock across `process_event` and calls `drain` directly.
    pub fn drain_locked(&mut self) -> bool {
        let state = self.state.clone();
        let mut app = state.lock().unwrap();
        self.drain(&mut app)
    }

    /// Apply a `Msg` against the root-owned `App`. This is the bridge from
    /// the new message-driven contract to today's imperative `App` API.
    /// As components migrate, the variants they own will stop landing
    /// here and start landing in their `handle_msg`.
    fn apply_root(msg: &Msg, app: &mut App) {
        match msg {
            Msg::Quit => app.set_state(AppState::Quit),
            Msg::ToggleHelp => {
                if matches!(app.state, AppState::Help) {
                    app.set_state(AppState::FolderView);
                } else {
                    app.set_state(AppState::Help);
                }
            }
            Msg::ToggleContentPane => app.toggle_content_pane(),
            Msg::FocusNext => app.switch_pane(PaneSwitchDirection::Right),
            Msg::FocusPrev => app.switch_pane(PaneSwitchDirection::Left),
            Msg::ViewNext => app.next_view(),
            Msg::ViewPrev => app.prev_view(),
            // All other variants belong to components that haven't been
            // extracted yet. Leaving them as no-ops here is correct: the
            // legacy `input.rs` path still drives those behaviors.
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::email::EmailStore;
    use crate::maildir::MaildirScanner;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    fn make_root() -> AppRoot {
        let store = EmailStore::new(PathBuf::from("/tmp"));
        let scanner = MaildirScanner::new(PathBuf::from("/tmp"));
        let app = App::new(store, scanner);
        AppRoot::new(Arc::new(Mutex::new(app)))
    }

    /// Acceptance test for vu-gje: enqueueing `Msg::Quit` and draining the
    /// queue must set `should_quit = true` on the underlying `App`.
    #[test]
    fn approot_dispatches_quit_msg() {
        let mut root = make_root();
        root.enqueue(Msg::Quit);
        assert!(root.drain_locked());

        let app = root.state.lock().unwrap();
        assert!(app.should_quit, "Msg::Quit must flip should_quit");
        assert!(matches!(app.state, AppState::Quit));
    }

    /// `Msg::ToggleHelp` round-trips: Help → FolderView → Help. Documents
    /// the same toggle semantics that `input.rs::handle_key_event` has.
    #[test]
    fn approot_toggles_help() {
        let mut root = make_root();
        root.enqueue(Msg::ToggleHelp);
        root.drain_locked();
        assert!(matches!(root.state.lock().unwrap().state, AppState::Help));

        root.enqueue(Msg::ToggleHelp);
        root.drain_locked();
        assert!(matches!(
            root.state.lock().unwrap().state,
            AppState::FolderView
        ));
    }

    /// Global key mapping is pure — exercise the table directly so we
    /// don't need a fake event loop to assert it.
    #[test]
    fn handle_global_key_maps_q_to_quit() {
        let key = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        assert_eq!(
            AppRoot::handle_global_key(key, &ActivePane::Messages),
            Some(Msg::Quit)
        );
    }

    #[test]
    fn handle_global_key_l_from_folders_defers() {
        let key = KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE);
        // From Folders pane: must defer to legacy input.rs (returns None).
        assert!(AppRoot::handle_global_key(key, &ActivePane::Folders).is_none());
        // From other panes: emits ViewNext.
        assert_eq!(
            AppRoot::handle_global_key(key, &ActivePane::Messages),
            Some(Msg::ViewNext)
        );
    }

    #[test]
    fn handle_global_key_alt_c_toggles_content_pane() {
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::ALT);
        assert_eq!(
            AppRoot::handle_global_key(key, &ActivePane::Folders),
            Some(Msg::ToggleContentPane)
        );
        // Plain 'c' is not a global — falls through to handle_input.
        let plain = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE);
        assert!(AppRoot::handle_global_key(plain, &ActivePane::Folders).is_none());
    }
}
