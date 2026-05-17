---
bead: vu-bfb
polecat: furiosa
date: 2026-05-17
files:
  - src/components/root.rs
  - src/layout.rs
  - src/ui.rs
severity: medium
category: architecture
---

# `layout::SelectionState` is mostly a write-only mirror of component state

`layout::SelectionState` (`src/layout.rs:209-223`) carries five fields
— `folder_index`, `email_index`, `scroll_offset`, `attachment_index`,
`remembered_email_index`. The components `FoldersComponent`,
`MessagesComponent`, and `ContentComponent` already own the canonical
copies. `AppRoot::apply_root` faithfully writes the mirror after
nearly every dispatch:

- `root.rs:1066-1069` (FolderMove)
- `root.rs:1091-1093` (FolderEnter)
- `root.rs:1097-1100` (FolderExitParent)
- `root.rs:1111-1112` (MessageMove)
- `root.rs:1127` (MessageOpen)
- `root.rs:1135` (ContentScroll)
- `root.rs:1152-1153` (FoldersBlur/MessagesBlur)
- `root.rs:763-765` (`enter_selected_folder_async`)
- `root.rs:1425-1426` (`apply_search_results_named`)
- `root.rs:2033` (`switch_active_maildir`)
- `root.rs:166`, `674-675` (initial / scan-applied)

That's **eleven write paths**. The *read* side is tiny:

- `ui.rs:195` — `lay.selection.folder_index`
- `ui.rs:206` — `lay.selection.folder_index`
- `ui.rs:297` — `lay.selection.attachment_index`

`email_index`, `scroll_offset`, and `remembered_email_index` are
never read off `SelectionState` anywhere outside `apply_root`'s own
writes. They survive only because `ui.rs` once owned the selection
in the legacy `App` layout — and `attachment_index` is the one field
that genuinely *should* live on `Layout` because `AppRoot` mutates
it directly in `handle_residual_key` (`root.rs:612`, `:619`) without
a component owner.

Why it matters:

1. **Two sources of truth.** Every action handler that forgets a
   mirror write quietly drifts. The `Backspace` legacy path
   (`handle_back_navigation`) already does not mirror — relying on
   the components to be the canonical source — so the convention is
   already inconsistent.
2. **`apply_root` is the longest method in the codebase**
   (`root.rs:992-1217`); ~30 of its lines are pure mirror plumbing.
   Removing them shrinks the file and aligns the code with the
   component contract DESIGN-COMPONENTS.md describes ("components
   are canonical").
3. **The `// Cursor positions mirrored from the per-pane components`
   doc on `layout.rs:247`** is a known smell that has not been
   acted on — this audit is the trigger to act.

## Suggested next step

File under the Phase 0 refactor epic: drop the unread mirror fields
from `SelectionState` (keep only `attachment_index`, the one
field that has a real owner on `Layout`), delete every write site
in `apply_root`, and rewrite the three `ui.rs` reads to consult the
component directly (`folders.folder_index`, etc.) via the
existing `AppRoot` accessors.
