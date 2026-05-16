---
bead: vu-8uj
polecat: furiosa
date: 2026-05-16
files:
  - src/app.rs
severity: high
category: bug
---

# Three `app::tests::*` web-serving tests fail on `origin/main` (ed719de)

`cargo test` on a clean checkout of `main` (HEAD = ed719de, no local edits)
produces three failures, all in `app.rs`:

- `app::tests::test_web_serving_behavior_based_on_active_pane` (app.rs:534)
- `app::tests::test_pane_switching_preserves_email_serving` (app.rs:572)
- `app::tests::test_view_transitions_maintain_correct_web_serving` (app.rs:636)

All three call `App::get_current_email_for_web()` after configuring a test app
with `active_pane = ActivePane::Messages` and `current_folder = vec![0]` and
`selected_email = 0`, and assert `.is_some()`. The method returns `None`.

This blocks the "cargo test green" acceptance gate for any Phase-0.1 follow-on
bead (e.g. vu-8uj, vu-icl) even when the bead introduces no new logic. The
failures pre-date this branch — `git diff origin/main..HEAD -- src/` is empty
on the polecat branch, yet the same three tests fail. So whichever recent commit
broke this either changed `get_current_email_for_web` behavior or changed the
test-fixture builder without updating the assertions. Likely candidates from the
recent log: `e34335f` (Phase 0.1a error migration) or `96ca06e` (content
display fix). A `git bisect run cargo test --lib -- app::tests::test_web_serving_behavior_based_on_active_pane`
would land on the offending commit in 1-2 steps.

## Suggested next step

- File a P1 bug bead under whichever epic owns Phase-0 work.
- `git bisect` between `96ca06e` and `ed719de` to identify the regression
  commit.
- Either fix `get_current_email_for_web` or update the test fixtures —
  whichever matches the intended behavior. The tests look authoritative
  (they encode the pane→serving contract); my prior is the fix belongs in
  `get_current_email_for_web`.
- Until then, any bead with "cargo test green" in its acceptance must either
  carry an exception note or rely on `cargo test --lib -- --skip app::tests::test_web_serving --skip app::tests::test_pane_switching --skip app::tests::test_view_transitions`.
