---
bead: vu-2y8
polecat: furiosa
date: 2026-05-16
files:
  - src/components/root.rs
  - src/components/accounts.rs
severity: medium
category: bug
---

# 'l' on Accounts pane navigates instead of selecting the account

`AccountsComponent::on_key` (accounts.rs:200-212) maps `KeyCode::Char('l')`
to `Msg::AccountSelect(current_id)`, and the Phase 1.a unit test
`lowercase_l_emits_account_select_for_current_row` asserts this contract.
But when the Accounts pane is focused in a running app, that handler is
never reached.

`AppRoot::process_event` consults `handle_global_key` (root.rs:561-579)
*before* dispatching to the focused pane. The global handler intercepts
`'l'` and returns `Some(Msg::ViewNext)` for every pane except Folders.
With AccountsFolders focused, `'l'` therefore advances the view to
FolderMessages *without* triggering an `AccountSelect`. The user's
intent — pick this account — is silently dropped.

The Phase 1.g integration bead asked for a workflow that begins by
"switching account via 'l' on AccountsComponent". The integration test
landed under vu-2y8 had to drive the switch via `Msg::AccountSelect`
directly to capture the intended end-to-end effect.

## Suggested next step

- File a bead to extend the `handle_global_key` `'l'`-defer branch to
  Accounts in addition to Folders: e.g. return `None` when
  `active_pane` is `Accounts`, letting the per-pane handler emit
  `AccountSelect`. Add a regression integration test that drives
  account switching by `'l'` end-to-end through `process_event`.
- VISION.md "View Progression" implies `l` is the right-advance key
  in every pane; pairing it with AccountSelect from Accounts matches
  the spec.
