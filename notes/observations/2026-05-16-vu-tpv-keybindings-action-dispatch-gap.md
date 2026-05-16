---
bead: vu-tpv
polecat: furiosa
date: 2026-05-16
files:
  - src/components/root.rs
  - src/components/messages.rs
severity: medium
category: architecture
---

# `[keybindings]` overrides don't reach component-local action handlers

## What I noticed

While writing Phase 4 integration tests for `vu-tpv`, I confirmed that
`[keybindings] archive = "e"` propagates correctly into
`AppRoot.keymap()` — `lookup_single(Char('e'))` returns `Some(Action::Archive)`
and the default `a` no longer maps to anything.

But pressing `e` in `MessagesComponent` does **not** emit `Msg::Archive`
today. The dispatch flow in `AppRoot::process_event`:

1. `keymap.lookup_single(key)` resolves to `Some(Action::Archive)`.
2. `Self::action_to_msg(Action::Archive, ...)` returns `None`
   (the match has no arm for `Archive`, `Star`, `Delete`, `Reply*`,
   `Forward`, `MoveToFolder`, `ToggleFlag`, `MarkUnread`, `JumpTop`,
   `JumpBottom`, `JumpNextUnread`, `JumpPrevUnread`, `AcceptSuggestion`,
   `SearchNext`, `SearchPrev`, `Back`, `Confirm`, etc.).
3. The `if let Some(msg) = action_to_msg(...)` guard fails, so we fall
   through to `MessagesComponent.on_key('e')` — which only recognises
   the hard-coded `KeyCode::Char('a')` for archive
   (`src/components/messages.rs:457`).
4. Result: no `Msg::Archive` is emitted, and the override is silently
   inert at runtime even though the keymap reflects it.

## Why it matters

vu-6kf's acceptance ("override file rebinds `archive=e` and the
resolved map has `e → Archive`, no longer `a → Archive`") was met at
the keymap layer but the dispatch layer is still partial. Users who
configure `[keybindings]` for action keys other than the global ones
already wired (`Quit`, `ToggleHelp`, `Undo`, `ToggleViewer`,
`ToggleContentPane`, `FocusNext/Prev`, `ViewPrev/Next`, `Search`,
`DraftSend/Edit/Discard`) get a silently-broken config.

This is the gap between VISION.md §Configuration Schema's promise
("any of them") and what's wired today.

## Suggested follow-up

Two reasonable shapes:

1. **Extend `action_to_msg`** to route every component-bound `Action`
   to its `Msg` when the matching pane is active, and remove the
   parallel hard-coded `KeyCode::Char(...)` arms from
   `MessagesComponent.on_key` / `FoldersComponent.on_key` /
   `ContentComponent.on_key` / `DraftComponent.on_key`. Sequence
   bindings (`gg`, `G`, `gj`, `gk`, `gr`) stay with their components
   until `Keymap` grows a prefix-dispatch helper.

2. **Pass the resolved `Keymap` into each `Component`** so the
   component itself does `keymap.lookup_single(key)` instead of
   matching raw `KeyCode`s. More work; cleaner separation.

Either way, the integration test in
`src/phase4_integration_tests.rs::keybindings_override_archive_to_e_propagates_to_apphroot_keymap`
already asserts the keymap end of the contract — once the dispatch
side lands, extend it to also press `e` through `process_event` and
observe a `Msg::Archive` side effect (e.g. mark-as-archived in the
store).

## Out of scope for vu-tpv

vu-tpv is "Phase 4.e: Phase 4 integration tests" — its job is to
cover the features as they exist, not to extend them. Filing this so
mayor can decide whether to spin up a Phase 4.f follow-up.
