---
bead: vu-wvd
polecat: nux
date: 2026-05-16
files:
  - src/config.rs
  - src/input.rs
  - src/ui.rs
  - src/maildir.rs
severity: low
category: refactor
---

# Pre-existing rustfmt drift and clippy warnings in untouched code

While migrating `maildir.rs` to `VulthorError`, `cargo fmt --check` flagged
formatting drift in code I did not touch:

- `src/config.rs:126` — `CliArgs::parse_from(&[...])` array splits one item
  per line under current rustfmt.
- `src/input.rs:81` — `get_folder_path_from_display_index(...)` call wraps
  differently.
- `src/ui.rs:2` — `use unicode_width::UnicodeWidthStr;` ordering vs.
  `use ratatui::...`.

`cargo clippy` also flags several preexisting warnings inside `maildir.rs`
on lines I did not change (and so left alone to keep the migration commit
scoped):

- `maildir.rs:39-40` — `for entry in entries { if let Ok(entry) = entry`
  should be `.flatten()`.
- `maildir.rs:130-133` — nested `if let Some(limit) = limit { if count >= limit }`
  is a collapsible-if pattern.
- `maildir.rs:142-145` — `.map_or(false, |name| name == "new")` simplifies
  to `.is_some_and(|name| name == "new")` (or `== Some("new")` after
  `as_deref()`).

I deliberately did **not** fold these into the thiserror migration commit
to keep the scope of `vu-wvd` clean. Both could be cleared in one tiny
follow-up pass: `cargo fmt` once, then targeted clippy fixes in
`maildir.rs`.

## Suggested next step

- File a small bead "chore: clear pre-existing fmt/clippy drift in
  config/input/ui/maildir" under whatever the Phase-0 hygiene epic is. It's
  a single commit and would let CI re-enable `cargo fmt --check` and
  `cargo clippy -D warnings` as gates without false positives.
