---
date: 2026-05-16
bead: vu-8qr
slug: clippy-failures-on-main
status: open
severity: medium
---

# Pre-existing clippy errors will fail CI on first run

While adding `.github/workflows/ci.yml` for vu-8qr, ran the same checks
locally that CI will run. `cargo clippy --all-targets --all-features --
-D warnings` exits non-zero on `origin/main` (21 errors, e.g.
`needless_borrows_for_generic_args` in `src/config.rs:204`).

`cargo fmt --all -- --check` passes.

Implication: the CI workflow file itself satisfies vu-8qr's acceptance
criteria (file exists, triggers a run), but the first run will be red.

Worth a follow-up bead to either:
1. Fix the existing clippy warnings, or
2. Loosen the lint level (e.g. drop `-D warnings`) — not recommended.

Out of scope for vu-8qr per the bead's stated acceptance criteria.
