---
bead: vu-sd6
polecat: furiosa
date: 2026-05-16
files:
  - src/input.rs
  - src/components/root.rs
severity: low
category: refactor
---

# Backspace still mutates `App.selection.folder_index` outside FoldersComponent

The vu-sd6 acceptance criterion says "FoldersComponent is the sole writer of
folder_index." The j/k/Enter/l paths now honor that — keys are routed through
`FoldersComponent::on_key` and `apply_root` mirrors the component value into
`App.selection.folder_index` for legacy readers (still used by `ui.rs`
draw_messages_pane and `input.rs::get_selected_folder`).

The one exception is **Backspace** in the Folders pane:

```rust
// src/input.rs::handle_back_navigation
ActivePane::Folders | ActivePane::Messages => {
    app.email_store.exit_folder();
    app.selection.folder_index = 0;   // <-- write outside FoldersComponent
    ...
}
```

When the focused pane is `Folders`, `FoldersComponent::on_key` returns `None`
for Backspace, the event falls through to `input.rs::handle_input`, and
`handle_back_navigation` writes `app.selection.folder_index = 0`. AppRoot
catches this with `sync_app_to_folders` after the fall-through, so the
component value stays in sync — but the *write itself* originates outside
the component, violating the strict reading of the acceptance criterion.

## Why I didn't fix it now

The bead scope is "extract j/k/Enter/l + render." Adding a new `Msg` variant
for back-navigation (e.g. `Msg::FolderExitParent`) and rewriting
`handle_back_navigation` to route through messages widens the diff and
touches `App.email_store.current_folder` semantics that the Messages pane
also needs. That work belongs with **vu-3yj** (Phase 0.2.3 — extract
MessagesComponent + ContentComponent), which already has to grapple with
folder-traversal state ownership.

## Suggested follow-up

When vu-3yj lands, replace the legacy Backspace branch with a
`Msg::FolderExitParent` (or equivalent) that:

1. Calls `app.email_store.exit_folder()` from `apply_root`.
2. Resets `FoldersComponent.folder_index = 0` via `handle_msg`.
3. Removes the `sync_app_to_folders` call in `AppRoot::process_event` and
   the corresponding mirror-back logic, since no path outside the component
   will write `folder_index` anymore.

Until then, the mirror is harmless: every Backspace clamps both sides to 0
in the same tick.
