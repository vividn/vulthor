---
bead: vu-bfb
polecat: furiosa
date: 2026-05-17
files:
  - src/components/messages.rs
  - src/components/content.rs
  - src/components/accounts.rs
  - src/components/folders.rs
  - src/keymap.rs
  - src/components/root.rs
severity: medium
category: architecture
---

# Arrow keys, PageUp/PageDown, and Accounts j/k still bypass the keymap

VISION.md and vu-otn's centralisation work say every action key
should flow through the resolved `Keymap` so `[keybindings]`
overrides actually reach the runtime. The keymap (`src/keymap.rs:161-201`)
currently binds character keys only — `j`/`k` for MoveDown/MoveUp,
`Enter` for Confirm, etc. Arrow keys and PageUp/PageDown are
not in `DEFAULT_KEYMAP` at all.

Components still hold parallel hard-coded `KeyCode` arms:

- `messages.rs:526-528` — `KeyCode::Down`/`Up` → `Msg::MessageMove(Dir::Down/Up)`
- `content.rs:192-195` — `KeyCode::Down`/`Up`/`PageDown`/`PageUp` →
  `Msg::ContentScroll(...)`
- `accounts.rs:214-216` — `KeyCode::Char('j') | Down`,
  `KeyCode::Char('k') | Up`, `KeyCode::Enter | Char('l')`
- `folders.rs` mirrors the same convention (Up/Down arrows)

Why it matters:

1. **Arrow keys cannot be rebound.** A user who replaces `j` with
   `e` in `[keybindings]` still has `Down` doing the same thing,
   but they cannot disable arrow navigation or remap PageDown to
   "Jump next unread" — those keys never hit `resolve_keymap`.
2. **Accounts pane `j`/`k` is a second source of truth.** The
   centralised dispatch already emits `Msg::FolderMove(Dir::Down)`
   for the Folders pane (via `action_to_msg`), but Accounts has a
   different convention — `on_key` returns `Msg::AccountMove(...)`
   for the same `j` key. `action_to_msg` at `root.rs:862-867`
   explicitly returns `None` for Folders/Accounts/Attachments on
   MoveDown — i.e. there is a *deliberate* fall-through to the
   component handler. This works, but it means the keymap is the
   source of truth for some panes and the component is for others.
3. **vu-otn's payoff is partial.** The centralisation refactor's
   value proposition (`[keybindings]` overrides work everywhere) is
   only delivered for the panes whose actions go through
   `action_to_msg`. The audit prompt flagged this explicitly.

## Suggested next step

Two paths worth considering:
- **Add arrow / PageUp / PageDown / BackTab as additional
  `DEFAULT_KEYMAP` entries** alongside the j/k/Tab defaults, so the
  same key flows through `Keymap::lookup_single`. `Keymap::normalize`
  already knows how to canonicalise — adding more `Action` ↔ key
  pairs is mechanical.
- **Promote `AccountMove` to flow through `action_to_msg` like
  `FolderMove`/`MessageMove`** by giving the Accounts pane its own
  MoveDown/MoveUp branch (it is intentionally absent today — see
  the inline comment at `root.rs:862-867`). The current "Folders/
  Accounts own j/k" rule exists only because the components claim
  `l` for select-into; that can be expressed as a separate
  `ViewNext` branch without forcing the whole MoveDown family to
  defer.
