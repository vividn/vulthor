// Component-based state management — Phase 0.2.1 scaffold (vu-m6s).
//
// Step 1 of the migration in DESIGN-COMPONENTS.md: this module introduces
// the `Component` trait, the flat `Msg` enum, the read-only `Ctx`, and a
// dead-code `AppRoot` wrapper. No pane has been migrated yet. Wiring into
// `main.rs` lands in vu-q31 (FoldersComponent extraction).

// Step 1 lands the scaffold without callers; vu-q31 onward wires panes in.
#![allow(dead_code, unused_imports)]

mod ctx;
mod msg;
mod root;

pub use ctx::Ctx;
pub use msg::{AccountId, Dir, FolderPath, MessageId, Msg, ReplyKind};
pub use root::AppRoot;

use std::collections::VecDeque;

use crossterm::event::KeyEvent;
use ratatui::{Frame, layout::Rect};

/// Cap on per-tick message dispatch. A well-behaved component emits at
/// most a small handful of follow-ups; this cap exists to bound runaway
/// feedback loops between components without locking up the render
/// thread. See DESIGN-COMPONENTS.md § "The dispatch model".
pub const MAX_DISPATCH_DEPTH: usize = 64;

/// A component owns a slice of UI state, renders one pane, and reacts to
/// messages. Components communicate only via `Msg`; they never call into
/// each other directly.
pub trait Component {
    /// React to a broadcast message. Returns follow-up messages to enqueue.
    /// Mutates only `self`; shared resources change through emitted messages
    /// that the owner (today, `AppRoot`) applies.
    fn handle_msg(&mut self, msg: &Msg, ctx: &Ctx) -> Vec<Msg>;

    /// Draw this component into `area`. Must not mutate `self`.
    fn render(&self, f: &mut Frame, area: Rect, focused: bool, ctx: &Ctx);

    /// Translate a key event into a message. Only the focused component
    /// sees keys; global shortcuts are intercepted upstream.
    fn on_key(&mut self, _key: KeyEvent, _ctx: &Ctx) -> Option<Msg> {
        None
    }
}

/// Broadcast every queued message to every component, bounded by
/// `MAX_DISPATCH_DEPTH`. Returns `true` if the queue drained naturally,
/// `false` if the cap kicked in — the runaway-loop signal `AppRoot`
/// will log on once it wires this up.
pub fn dispatch_bounded(
    queue: &mut VecDeque<Msg>,
    components: &mut [&mut dyn Component],
    ctx: &Ctx,
) -> bool {
    let mut steps = 0usize;
    while let Some(msg) = queue.pop_front() {
        steps += 1;
        if steps > MAX_DISPATCH_DEPTH {
            return false;
        }
        for c in components.iter_mut() {
            queue.extend(c.handle_msg(&msg, ctx));
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::email::EmailStore;
    use crate::theme::VulthorTheme;
    use std::path::PathBuf;

    /// Minimal `Component` impl — verifies the trait can be satisfied with
    /// nothing but the two required methods (handle_msg + render).
    struct EmptyComponent;
    impl Component for EmptyComponent {
        fn handle_msg(&mut self, _msg: &Msg, _ctx: &Ctx) -> Vec<Msg> {
            Vec::new()
        }
        fn render(&self, _f: &mut Frame, _area: Rect, _focused: bool, _ctx: &Ctx) {}
    }

    /// Pathological component that always emits a follow-up. Used to prove
    /// the depth cap protects the render thread from runaway emission.
    struct EchoComponent;
    impl Component for EchoComponent {
        fn handle_msg(&mut self, _msg: &Msg, _ctx: &Ctx) -> Vec<Msg> {
            vec![Msg::StatusClear]
        }
        fn render(&self, _f: &mut Frame, _area: Rect, _focused: bool, _ctx: &Ctx) {}
    }

    fn make_ctx_fixture() -> (Config, EmailStore, VulthorTheme) {
        (
            Config::default(),
            EmailStore::new(PathBuf::from("/tmp")),
            VulthorTheme,
        )
    }

    #[test]
    fn empty_component_compiles_and_runs() {
        let (config, store, theme) = make_ctx_fixture();
        let ctx = Ctx {
            theme: &theme,
            config: &config,
            store: &store,
        };
        let mut c = EmptyComponent;
        assert!(c.handle_msg(&Msg::Quit, &ctx).is_empty());
        // Default `on_key` impl returns None — confirms the trait default works.
        let key = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('q'),
            crossterm::event::KeyModifiers::NONE,
        );
        assert!(c.on_key(key, &ctx).is_none());
    }

    #[test]
    fn bounded_dispatch_terminates_under_runaway_emission() {
        let (config, store, theme) = make_ctx_fixture();
        let ctx = Ctx {
            theme: &theme,
            config: &config,
            store: &store,
        };
        let mut echo = EchoComponent;
        let mut queue: VecDeque<Msg> = VecDeque::new();
        queue.push_back(Msg::StatusClear);

        let drained = dispatch_bounded(&mut queue, &mut [&mut echo], &ctx);
        assert!(!drained, "echo loop must trip the depth cap");
    }

    #[test]
    fn dispatch_drains_naturally_when_no_follow_ups() {
        let (config, store, theme) = make_ctx_fixture();
        let ctx = Ctx {
            theme: &theme,
            config: &config,
            store: &store,
        };
        let mut empty = EmptyComponent;
        let mut queue: VecDeque<Msg> = VecDeque::new();
        queue.push_back(Msg::Quit);
        queue.push_back(Msg::ToggleHelp);

        let drained = dispatch_bounded(&mut queue, &mut [&mut empty], &ctx);
        assert!(drained);
        assert!(queue.is_empty());
    }
}
