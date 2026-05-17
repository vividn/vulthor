---
bead: vu-bfb
polecat: furiosa
date: 2026-05-17
files:
  - src/components/mod.rs
  - src/components/root.rs
severity: low
category: refactor
---

# `components::dispatch_bounded` is unused — `AppRoot::drain` reimplements it inline

`components::dispatch_bounded` (`src/components/mod.rs:94-110`) was
introduced as the generic bounded-dispatch loop the
DESIGN-COMPONENTS.md "dispatch model" section describes. It is fully
tested by `bounded_dispatch_terminates_under_runaway_emission` and
`dispatch_drains_naturally_when_no_follow_ups` (`mod.rs:167-198`).

It is also unused outside its own tests. The production dispatch
path is `AppRoot::drain` (`src/components/root.rs:957-981`), which
hand-rolls the same steps-bounded loop, calls each component's
`handle_msg` explicitly, then invokes `apply_root` per message.

Why it matters:

1. **Two ways to do the same thing, and only one is wired.** A
   future reader who reads `mod.rs` first will assume
   `dispatch_bounded` is the entry point and trace a phantom code
   path.
2. **The tests on the unused helper give false coverage signal.**
   They prove the helper terminates; they prove nothing about the
   real `AppRoot::drain` (which is currently only stress-tested
   transitively through the integration tests).
3. **The `Component` trait's symmetric `handle_msg(&Msg, &Ctx) ->
   Vec<Msg>` signature lives to feed `dispatch_bounded`.** With
   `AppRoot::drain` calling each component by name, the trait could
   shed the `Vec<Msg>` return (every component already discards
   most messages) and become simpler — but only if we either commit
   to `dispatch_bounded` or commit to ripping it out.

## Suggested next step

Pick one path: either (a) make `AppRoot::drain` call
`dispatch_bounded` with a small `&mut [&mut dyn Component]` slice
plus a post-pass for `apply_root`, deleting the inline loop; or
(b) delete `dispatch_bounded` and its tests outright. Today both
exist, and the design-doc reader has to guess which is canonical.
