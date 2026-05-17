---
bead: vu-q9b
polecat: furiosa
date: 2026-05-17
files:
  - src/components/root.rs
severity: low
category: bug
---

# Many `active_pane` assignments bypass `on_focus_change`

`AppRoot::on_focus_change` is the documented funnel for pane transitions
(it `publish_focus()`es, fires `FoldersBlur`/`MessagesBlur`, and — after
vu-q9b — clears `pending_keys`). But only the `FocusNext`/`FocusPrev`
arms in `apply_root` actually route through it. Roughly fifteen other
sites assign `self.layout.active_pane = ...` directly (Backspace from
Content, `handle_back_navigation`, draft start, search result rendering,
`switch_active_maildir`, `set_active_pane_for_test`, etc.) — grep
`active_pane =` in `root.rs` for the full list.

For most of those the gap is harmless today (they happen inside a
dispatch loop that ran the user's key to completion; `pending_keys` is
empty), but it's a latent footgun: any future code path that changes
the pane while a sequence prefix is held will leak the prefix into the
new pane. vu-q9b mitigates this in the test seam
(`set_active_pane_for_test` now clears `pending_keys`), but the
production callers are still ad-hoc.

## Suggested next step

Wrap the bare assignments in a single `set_active_pane(new)` helper on
AppRoot that owns the invariants: clear `pending_keys`, `publish_focus`,
and dispatch blur Msgs. Each existing call site is a one-line swap, and
`on_focus_change` collapses into the helper.
