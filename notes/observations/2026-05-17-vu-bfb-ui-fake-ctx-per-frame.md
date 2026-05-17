---
bead: vu-bfb
polecat: furiosa
date: 2026-05-17
files:
  - src/ui.rs
  - src/components/ctx.rs
  - src/components/root.rs
severity: medium
category: architecture
---

# ui.rs builds a throw-away `Ctx` from `Config::default()` on every render

`ui::UI::draw_main_layout` constructs a fresh `Ctx` at four call sites
(`src/ui.rs:91`, `:142`, `:162`, `:234`) by allocating
`Config::default()` and a `VulthorTheme` *every frame*. The components'
`render` methods then receive a `&Ctx` whose `config` is a default
struct, not the user's loaded `Config`, and whose `theme` is the
zero-sized unit-struct constant table.

Meanwhile `AppRoot` already owns the real `Config` (`root.rs:59`) and a
fully resolved runtime `Theme` (`root.rs:123`) — `render` passes the
borrowed components but throws those two away when it crosses into
`ui.rs::draw_main_layout`.

Why it matters:

1. **The runtime theme never reaches a renderer.** `theme::build_theme`
   resolves `[theme]` overrides into a `Theme` and `AppRoot::set_theme`
   stashes it (`root.rs:250`), but no component's `render` can read it
   — the `#[allow(dead_code)]` on `AppRoot::theme` admits this. Every
   render site instead reads `VulthorTheme::*` compile-time constants,
   so `[theme].overrides` is invisible at runtime even though the
   config plumbing is wired end-to-end and exercised by
   `phase4_integration_tests::theme_user_file_and_overrides_resolve_via_config`.
2. **Components cannot observe the real `Config`** at render time. If
   a future feature needs (say) `[ai].chip_style` or
   `[messages].date_format` it has to either thread the field through
   a bespoke argument or burn the dead `Ctx` and reach for a global.
3. **Per-frame allocations** of `Config::default()` (which is
   non-trivial — accounts map, keybindings BTreeMap, web/ai/theme
   blocks). Not catastrophic but wasted on the hot path.

## Suggested next step

Plumb `AppRoot`'s real `&Config` and `&Theme` (or the new role-typed
`Theme` struct) through `UI::draw` into the per-pane `Ctx`. Then drop
`VulthorTheme` from `Ctx` and let render sites read the runtime
`Theme` so `[theme].overrides` finally adopts. This is also a
prerequisite for the dead-code allowance on `AppRoot::theme` going
away.
