# Component-Based State Management — Design

Status: **DESIGN** — no implementation yet. This document defines the target
architecture for Phase 0.2 of the VISION.md roadmap. Subsequent tasks under
`vu-dke` execute against this design.

## Why

Today's `App` struct (`src/app.rs`) is a single 658-line god object holding
selection state, the email store, the scanner, view enums, and pane state.
Input handlers (`src/input.rs`, 829 lines) reach into `App` directly and mutate
many fields at once. UI rendering (`src/ui.rs`, 833 lines) receives `&mut App`
and pulls whatever it needs. The web server (`src/web.rs`) holds the same `App`
behind a global `Mutex`.

This works at today's scale (one folder pane, one message list, one content
pane) but blocks the v1.0 surface:

- An **Accounts** pane needs its own selection + per-account state.
- A **Draft** pane needs an editor lifecycle (open, edit, save, send) that is
  independent of message-list state.
- The future **AI classifier** must observe selection changes without coupling
  to `App` internals.
- The **web server** should observe a snapshot of the current email without
  holding a write lock on the entire app.

The fix is structural: split the god object into independent components that
own their state, render their pane, and communicate via a message bus. This
matches the surface progression already in VISION.md (Accounts → Folders →
Messages → Content → Draft) one-to-one.

## Non-goals

- This design does **not** introduce a new TUI framework. We stay on
  `ratatui`.
- This design does **not** unify async/sync. That's `vu-rvm` (Phase 0.3).
- This design does **not** change `thiserror` adoption. That's `vu-tje`
  (Phase 0.1).
- This design does **not** introduce a virtual DOM, diffing, or
  retained-mode trickery. ratatui is immediate-mode; we keep it that way.
- This design does **not** change the persistence layer (MailDir) or the
  web pane contract (SSE + JSON endpoints stay identical).

## Inspiration & shape

The Elm Architecture (TEA), adapted to Rust + ratatui. Each component is
a value type with:

- **State** — owned data (selection indices, scroll offset, cached lists).
- **Update** — a single `handle_msg(&mut self, Msg, &mut Ctx) -> Vec<Msg>`
  entry point. The only way state changes.
- **View** — a `render(&self, &mut Frame, Rect, &Ctx)` that draws into the
  passed rect. No mutation of self during render.

Components do not call each other. They emit messages. The runtime
(`AppRoot`) dispatches messages, possibly broadcasting to other components.

This is also the shape Iced and several Rust TUI projects (`gpui`,
`tui-realm`) converge on. We are not inventing.

## The `Component` trait

```rust
// src/components/mod.rs (proposed)

use crossterm::event::KeyEvent;
use ratatui::{Frame, layout::Rect};

/// Shared, read-only context passed to every component each tick.
/// Holds resources components need but do NOT own — email store, scanner,
/// theme, config. Components borrow from `Ctx`; they do not mutate it.
/// Mutations to shared resources happen via messages handled by the
/// component that owns the resource (e.g., `EmailStoreComponent`).
pub struct Ctx<'a> {
    pub theme: &'a crate::theme::VulthorTheme,
    pub config: &'a crate::config::Config,
    // Read-only handle to the email store. Mutations dispatched as Msg::Store(_).
    pub store: &'a crate::email::EmailStore,
}

/// A component owns a slice of UI state, renders one pane (or contributes to
/// one), and reacts to messages. Components communicate only via `Msg`.
pub trait Component {
    /// Handle a message. Return zero or more follow-up messages to dispatch.
    /// Pure-ish: only `self` is mutated; shared resources change via emitted
    /// messages, never by reaching into `Ctx`.
    fn handle_msg(&mut self, msg: &Msg, ctx: &Ctx) -> Vec<Msg>;

    /// Render this component into `area`. Must not mutate `self`.
    /// Ratatui-stateful widgets (ListState, ScrollbarState) live inside the
    /// component as `Cell<_>` or are recomputed each frame from owned state.
    fn render(&self, f: &mut Frame, area: Rect, focused: bool, ctx: &Ctx);

    /// Optional: translate a raw key event into a `Msg`. Default: no-op.
    /// Only the focused component receives key events. Global keys
    /// (`q`, `?`, Alt+c, h/l) are intercepted by `AppRoot` before this.
    fn on_key(&mut self, _key: KeyEvent, _ctx: &Ctx) -> Option<Msg> {
        None
    }
}
```

### Design choices, explicit

- **`&Ctx`, not `&mut Ctx`.** Components never directly mutate shared state.
  Mutations to the email store (e.g., marking read) flow as `Msg::Store(...)`
  to a dedicated `EmailStoreComponent` (or to `AppRoot` which owns the store).
  This is what makes parallel review and testing tractable.
- **No child component list on the trait.** Composition is explicit in the
  parent's `handle_msg`/`render`. We do *not* model components as a generic
  tree because the Vulthor layout is fixed (five panes, one of which is
  visible at a time). A trait-object tree would obscure the layout and add
  dispatch overhead for no gain. See "Composition" below.
- **`Vec<Msg>` return, not a channel.** Synchronous dispatch is simpler than
  a tokio channel for this layer. Async work (file I/O, network) is wrapped
  by `vu-rvm` and reported back as a single `Msg::AsyncDone(...)`. Keeping
  message dispatch sync means we can step the whole tree in one terminal
  draw cycle with no atomicity worries.
- **No lifetime parameter on the trait.** `Ctx<'a>` is constructed fresh per
  tick from `AppRoot`-owned resources. The trait method takes `&Ctx`, which
  lets each call site pick its own lifetime.

## The `Msg` enum

A single flat enum. Variants are grouped by addressee. Keep it boring:

```rust
// src/components/msg.rs (proposed)

pub enum Msg {
    // Global lifecycle
    Quit,
    ToggleHelp,
    StatusSet(String),
    StatusClear,

    // Layout
    ViewNext,                      // 'l'
    ViewPrev,                      // 'h'
    ToggleContentPane,             // Alt+c
    FocusNext,                     // Tab
    FocusPrev,                     // S-Tab

    // Accounts (Phase 1, scaffold now)
    AccountSelect(AccountId),

    // Folders
    FolderMove(Dir),               // j/k inside Folders
    FolderEnter,                   // Enter or 'l' on a folder
    FolderLoaded(FolderPath),      // async fanout from store

    // Messages
    MessageMove(Dir),
    MessageOpen(MessageId),        // Enter on a message
    MessageMarkRead(MessageId),

    // Content
    ContentScroll(Dir, usize),

    // Draft (Phase 2)
    DraftStart(ReplyKind, MessageId),
    DraftEditorExited,
    DraftSend,

    // Store mutations (handled by AppRoot/store owner)
    StoreLoadFolder(FolderPath),
    StoreLoadEmail(MessageId),
}

pub enum Dir { Up, Down, Left, Right }
pub enum ReplyKind { Reply, ReplyAll, Forward, ReplyLater }
```

### Why flat, why a single enum

A flat enum is the cheapest thing that gives us:

- Exhaustive `match` ergonomics in each component's `handle_msg`.
- Easy serialization for the future "headless / record-replay" test harness.
- One spot to grep for every message that exists.

We resist the urge to make `Msg` generic over component type (`Msg<C>`) or
to nest variants by component (`Msg::Folders(FoldersMsg)`). Nested variants
add forwarding boilerplate without adding type safety we actually use —
each component just ignores messages it doesn't care about.

## The dispatch model

`AppRoot` owns every component plus the message queue. The main loop is a
fixed three-phase tick:

```text
┌── tick ────────────────────────────────────────────┐
│ 1. INPUT      poll crossterm; turn key/event       │
│                into 0..1 Msg.                       │
│                                                     │
│ 2. DISPATCH   while !queue.empty():                 │
│                  msg = queue.pop()                  │
│                  broadcast msg to every component   │
│                  (each returns Vec<Msg>)            │
│                  push results back to queue.        │
│                                                     │
│ 3. RENDER     compute layout based on `View` +      │
│                content_hidden + focused pane.       │
│                draw each visible component in turn. │
└────────────────────────────────────────────────────┘
```

Two important rules:

- **Broadcast, not addressing.** Every component sees every message. Most
  ignore most of them. This means a new component (e.g., `AiSuggestion`)
  can subscribe to `MessageOpen` without anyone re-routing it. The cost
  (a dozen no-op `match` arms per tick) is negligible.
- **Dispatch is bounded.** `handle_msg` returning new messages can in
  principle loop. We cap the per-tick dispatch at `MAX_DISPATCH_DEPTH`
  (proposal: 64) and log if exceeded. In practice, well-designed components
  emit at most 1–2 follow-ups per input.

### Async / blocking work

For Phase 0.2 (this doc), `handle_msg` is fully synchronous. When a
component needs work that today blocks the TUI (folder scan, full-email
parse), it emits a `Msg::StoreLoadFolder(_)` / `Msg::StoreLoadEmail(_)`
and `AppRoot` handles it inline (blocking, as today).

When `vu-rvm` lands, `AppRoot` swaps the inline call for
`tokio::task::spawn_blocking(...)` whose result is pushed back into the
queue as `Msg::FolderLoaded(_)` / `Msg::EmailLoaded(_)`. **Components
do not change.** This is the payoff of routing all I/O through messages
instead of direct method calls.

## Which existing structs become components

Mapping today's code to the target components. **One component per VISION
pane**, plus a store component for the shared email model.

| VISION pane | Component         | Today's home (src/app.rs)                       | Notes |
|-------------|-------------------|-------------------------------------------------|-------|
| Accounts    | `AccountsComponent` | _not present_                                   | Scaffold-only in 0.2; populated in Phase 1. |
| Folders     | `FoldersComponent`  | `App.selection.folder_index`, `find_inbox_folder`, `get_selected_folder`, `load_selected_folder_messages` | First migration target (POC). |
| Messages    | `MessagesComponent` | `App.selection.email_index`, `remembered_email_index`, `message_pane_visible_rows`, parts of `switch_pane` | |
| Content     | `ContentComponent`  | `App.selection.scroll_offset`, `ScrollDirection` handling | |
| Draft       | `DraftComponent`    | _not present_                                   | Scaffold-only in 0.2; populated in Phase 2. |
| —           | `StatusBar`         | `App.status_message`                            | Renders bottom strip; subscribes to `StatusSet`/`StatusClear`. |
| —           | `HelpOverlay`       | `AppState::Help` branch in `ui.rs`              | Modal; `AppRoot` toggles `visible`. |

Owned-by-`AppRoot` (not a `Component`, but root-owned):

| Resource     | Today                          | Tomorrow |
|--------------|--------------------------------|----------|
| Email store  | `App.email_store`              | `AppRoot.store: EmailStore` — read-only `&` into `Ctx`. Mutations via `Msg::Store...` handled in `AppRoot::dispatch`. |
| Scanner      | `App.scanner`                  | `AppRoot.scanner: MaildirScanner`. |
| Layout state | `App.current_view`, `content_pane_hidden`, `active_pane` | `AppRoot.layout: Layout` (small struct). Drives which components render and which receives keys. |
| Config       | passed in `main.rs`            | `AppRoot.config`. Read-only via `Ctx`. |
| Should-quit  | `App.should_quit`              | `AppRoot.should_quit`. Flipped by `Msg::Quit`. |

### Why the store stays root-owned

The email store is shared across at least two writers (the TUI tick and the
web server). Today both grab the same `Arc<Mutex<App>>`. After the refactor,
the web server will hold an `Arc<Mutex<EmailStore>>` directly, and `AppRoot`
will lock briefly to apply store mutations from messages.

Making the store a *component* would either (a) require components to mutate
each other (breaks the model) or (b) require channels between the store
component and every reader (overkill). Root-owned is the pragmatic answer.

### Layout / focus is root-owned, not a component

`current_view`, `content_pane_hidden`, and `active_pane` are not pane
state — they're *which* panes are visible and *which* one gets keys.
That's `AppRoot`'s job. Putting layout state inside a `LayoutComponent`
would create circular ownership (every component needing to ask the layout
component "am I visible?"), so we keep it where the dispatcher already lives.

## Composition: how `AppRoot` glues it together

```rust
// src/components/root.rs (proposed shape, illustrative)

pub struct AppRoot {
    // Resources (root-owned, lent to components via Ctx)
    store: EmailStore,
    scanner: MaildirScanner,
    config: Config,
    theme: VulthorTheme,

    // Components (root-owned)
    accounts: AccountsComponent,
    folders:  FoldersComponent,
    messages: MessagesComponent,
    content:  ContentComponent,
    draft:    DraftComponent,
    status:   StatusBar,
    help:     HelpOverlay,

    // Layout / focus
    layout: Layout,           // current_view, content_pane_hidden, active_pane
    queue:  VecDeque<Msg>,
    should_quit: bool,
}

impl AppRoot {
    pub fn tick(&mut self, ev: Option<Event>) -> Result<(), VulthorError> {
        // 1. INPUT
        if let Some(ev) = ev {
            if let Some(m) = self.layout.intercept_global(&ev) {
                self.queue.push_back(m);
            } else if let Event::Key(k) = ev {
                let focused = self.layout.focused_pane();
                if let Some(m) = self.component_mut(focused).on_key(k, &self.ctx()) {
                    self.queue.push_back(m);
                }
            }
        }

        // 2. DISPATCH (bounded)
        let mut steps = 0;
        while let Some(msg) = self.queue.pop_front() {
            steps += 1;
            if steps > MAX_DISPATCH_DEPTH { break; }
            self.apply_root(&msg);                 // store mutations, layout changes
            self.broadcast(&msg);                   // every component handle_msg
        }
        Ok(())
    }

    pub fn render(&self, f: &mut Frame) { /* layout-driven; see ui.rs equivalent */ }
}
```

The body of `render` retains today's match-on-`View` shape from `ui.rs`,
but each arm calls `self.<component>.render(f, rect, focused, &ctx)`
instead of inline drawing.

## Migration order

Five steps, each is its own follow-on bead under `vu-dke`. Each step ships
green tests and a working binary — no half-states on `main`.

**Step 1 — Introduce the trait + types, zero behavior change.**

- Add `src/components/{mod.rs, msg.rs, ctx.rs, root.rs}`.
- Define `Component`, `Msg`, `Dir`, `Ctx`.
- `AppRoot` exists but is a thin wrapper around today's `App` (no
  components migrated yet). The main loop in `main.rs` still uses `App`.
- Tests: trait-shape compiles; an empty `Component` impl works.

**Step 2 — Extract `FoldersComponent` as the proof of concept.**

This is the load-bearing step. Folders is chosen because:

- It's the leftmost pane — input flow is "Folders gets keys → emits
  `FolderEnter` → Messages reacts." Migrating it first establishes the
  message contract for everything to the right.
- Its state is small and well-bounded: `folder_index`, scroll offset,
  the auto-INBOX selection logic.
- The `load_selected_folder_messages` method shows exactly how an
  intra-component action turns into `Msg::StoreLoadFolder(path)`.

Concretely:

- Move `folder_index`, `find_inbox_folder`, `get_selected_folder` into
  `FoldersComponent`.
- Move the folder-pane half of `ui.rs::draw_folder_pane` into
  `FoldersComponent::render`.
- Move the folder-pane half of `input.rs::handle_main_view_input` into
  `FoldersComponent::on_key`.
- `App` still owns Messages/Content state, but now receives `FolderEnter`
  via the queue and reacts.
- Tests: golden tests on key sequences (`j j Enter` selects 3rd folder
  and emits `FolderLoaded`).

**Exit criteria for Step 2**: `cargo test` green, `cargo run` against
`fixture/maildir` looks identical to user.

**Step 3 — Extract `MessagesComponent` + `ContentComponent`.**

These can be one PR because they share the `remembered_email_index`
hand-off logic, which is best refactored atomically. After this step:

- `App` is essentially gone; `AppRoot` is the entry point.
- The remaining shared state in `AppRoot` is store + scanner + layout.
- `input.rs` is reduced to "translate event → global Msg or forward to
  focused component."
- `ui.rs` is reduced to "compute layout rects, ask each component to
  render."

**Step 4 — Scaffold `AccountsComponent` + `DraftComponent`.**

Both are render-only placeholders (single-line "Coming in Phase N" pane).
The point is to lock in their place in the layout enum and message
addressing *before* Phase 1/2 work begins. This prevents Phase 1 from
having to re-shuffle component IDs mid-flight.

**Step 5 — Delete `app.rs`'s `App` and `AppState`; `SharedAppState`
becomes `Arc<Mutex<EmailStore>>` for the web server.**

The web server no longer needs the full `App` — only the email store.
`get_current_email_for_web` becomes a small helper on `EmailStore`
parameterized by which-pane-is-focused (passed in by `AppRoot` whenever
focus changes, via `Msg::FocusChanged`).

After Step 5, `App` and `AppState` are removed. `current_view` lives
inside `Layout`. No file is named `app.rs`.

## Risks & open questions

- **Ratatui `ListState` ownership.** `ListState` is mutable each frame.
  Components own it as `RefCell<ListState>` (or recreate each render).
  Recommendation: `RefCell` for now; revisit if performance traces show
  it. Document the choice in `FoldersComponent` so it's not copy-pasted
  blindly.
- **Web server snapshot model.** Today the web server locks the whole
  `App` to read one field. Post-refactor it locks only `EmailStore`.
  The "which pane is focused" signal is a small atomic
  (`Arc<AtomicU8>`) updated by `AppRoot` on `Msg::FocusChanged`. This is
  noted but the exact wiring lands in Step 5.
- **Message dispatch cost.** Five components × ~40 msg variants × per-tick
  matches = order of 200 match arms per tick. Negligible — terminal
  redraw budget is milliseconds, this is nanoseconds. If it ever shows
  in a profile, components can declare a `subscribed_mask` to skip
  uninteresting messages; we don't pre-optimize.
- **TEA without commands.** Pure TEA models side effects as `Cmd`s
  returned from `update`. We collapse that into "messages targeting
  `AppRoot`" (`Msg::StoreLoadFolder(_)`, etc.). This is simpler in Rust
  where `Cmd<Msg>` would require boxing closures.
- **Open question: undo.** VISION says undo is a `Vec<Mutation>` on
  `App`. Post-refactor: where does it live? Proposal: `AppRoot` owns
  `undo_stack: Vec<Mutation>`; mutations recorded when `AppRoot::apply_root`
  consumes a mutating `Msg`. Settled when Phase 1 lands action keys.
- **Open question: keybinding rebinding.** VISION says
  `[keybindings]` in config overrides defaults. With per-component
  `on_key`, the rebind table must be consulted *before* each component's
  match. Proposal: `Layout::intercept_global` does the rebind lookup and
  rewrites events into "canonical" key events before dispatch. Locked in
  at Phase 4.

## Acceptance for this design

This document is "done" when a reader (human or agent) can answer all of:

- What is a `Component`? (Trait with `handle_msg` + `render` + `on_key`.)
- How do components talk? (Flat `Msg` enum, broadcast each tick.)
- Where does shared state live? (`AppRoot`; lent read-only via `Ctx`;
  mutated via root-handled `Msg` variants.)
- Which existing structs become components? (Five pane components +
  StatusBar + HelpOverlay; store and layout stay root-owned.)
- What migrates first? (`FoldersComponent`, because it's leftmost in
  the message flow and small enough to ship in one PR.)

If any of those answers feels under-specified, that's a comment on this
doc — file a follow-on under `vu-dke` rather than shipping ambiguity into
code.
