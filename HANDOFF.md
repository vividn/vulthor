# Mayor handoff — 2026-05-19 morning

Picking up from a long AFK window. Vulthor v1.0 is in steady state on
`origin/main` at `5801d4c`. Everything below summarises what's done,
what's open, and the gt-side wrinkles to keep in mind.

## Landed this AFK window (2026-05-17 → 2026-05-18)

Code, oldest first:

| Commit | Bead | Summary |
|---|---|---|
| `bce1ae6` | — | **Gitignore root-cause fix**: added `/.beads /.claude /.runtime /CLAUDE.local.md` to vulthor's main `.gitignore`. This unwedged the morning's "mass stall" pattern that was actually polecats blocked at `gt done` because the gt-tooling artifacts were untracked. |
| `fc8cce9` | vu-wmp | Refactor: `AppRoot::set_active_pane` helper centralises pane invariants. |
| `9b1f753` | vu-bdy | Tooling: log file rotation (size + age caps) + doctor check. |
| `5ed5ba1` | vu-wb0 | Test: proptest fuzzing for email / maildir / config parsers. |
| `d42f705` | vu-gkj | CI: cargo-audit + cargo-deny wired into doctor + CI workflow. |
| `aaf3b34` | vu-hy8 | Correctness: `multipart/alternative` + `multipart/related` split (body_plain + body_html + inline_images). |
| `870ba95` | vu-6yi | Security: link-spoofing detection (`src/link_check.rs`, 445 lines). |
| `e8c19f1` | vu-62n | Theme: built-in preset palettes (default-dark / default-light / solarized-dark / nord) + `Ctrl+T` cycle. |
| `f46fc0a` + `ee23697` | vu-dzm | UX: `?` help overlay grouped by `PaneScope`. |
| `88346cf` | vu-c1s | Security: `Shift+P` plaintext-only toggle + `[plaintext]` status bar indicator. |
| `9e9cbc9` | vu-aoy | Security: HTML images hidden by default + `Shift+I` reveals per-message + `[img]` indicator. |
| `40af4d2` | — | Wire `components/help.rs` (vu-dzm wave 2 was orphan), drop dead `layout.show_help`, `draw_help_screen`, `help_screen_lines`, `get_selected_email_markdown`. Build now warning-free. |
| `7d37c86` | vu-6rn (slice 1) | `tests/fixtures/maildir/` + `tests/maildir_fixtures.rs`: 7 diverse `.eml` fixtures (plain, html-only, multipart/alternative, attachment, phishing-link, multipart/related, large-body). |
| `5801d4c` | — | Test coverage for Msg::TogglePlaintext / Msg::ToggleImages / generate_email_html images_visible. |

Test count: **~575** total (lib 529, fixtures 7, plus 5 integration suites).

## Epics closed

- **vu-hxf — Security hardening pass** (CLOSED): vu-aoy, vu-c1s, vu-6yi, vu-pcw, vu-gkj, vu-dwr all landed. PGP/GPG sign+verify left as a separate post-v1 epic to be filed when prioritised.
- **vu-0mf — Codebase refactor + simplification** (CLOSED): all observation-derived children landed (vu-251, vu-0pn, vu-q9b, vu-wmp, vu-8ub) plus the broader Phase 2/3/4 refactors. One low-sev observation (`2026-05-17-vu-bfb-dispatch-bounded-shadow.md`) intentionally left as a documented design note.

## Open work

| Bead | State | Notes |
|---|---|---|
| **vu-6rn** | Open (slice 1 landed, rest waiting) | Remaining: tmux launch script + Playwright wrapper + CONTRIBUTING docs. The Maildir fixtures slice landed; you can drive the rest when you want interactive harness work. |
| **vu-po7** | Deferred | Phase 6.a embedding pipeline. Salvage at `origin/salvage/vu-po7-slit-wave2` (~2840 lines) — has classifier module restructuring that conflicts with `vu-8ub`'s revert. Needs your scope call. |
| **vu-d8v** | Open epic | Phase 6 AI classifier. Children filed but not implemented: |
| └ vu-d8v.1 | Open | b: k-NN classifier head over embeddings |
| └ vu-d8v.2 | Open | c: training data store (XDG state, jsonl append) |
| └ vu-d8v.3 | Open | d: on-device retraining (periodic corpus reload) |
| └ vu-d8v.4 | Open | e: suggestion UX wiring (chip render + Tab/`;` accept) |
| **vu-1td** | Open epic | Post-v1 quality + extension backlog. No children filed yet. |

## Gt-side issues I diagnosed but didn't fully resolve

### Scheduler dispatch wedged on stale wisp `hq-wisp-ae86`

**Status**: closed by you mid-session. After I closed `hq-wisp-ae86` at the town level (`BEADS_DIR=/home/gastown/gt/.beads bd close hq-wisp-ae86`), `gt scheduler run` stopped failing with "Failed to record dispatch failure". Sanity dispatch wasn't retried — recommend trying one when re-slinging vu-6rn / vu-po7.

### 96 orphaned dep refs in `bd doctor`

**Status**: `bd doctor --fix` was a no-op when I tried; you ran `gt doctor --fix` and may have cleaned them. If `bd doctor` still shows the count, that's where to start.

### Convoy-ack flood

After you cleared the stale wisp, deacon (Wisp Compaction 2026-05-18) walked every still-open convoy and fired a "convoy complete" mail per cleared entry — ~140 stale acks arrived over a few hours. All archived this morning. Pattern is documented; if it happens again, the fix is `gt mail archive $(...)`.

## Memories saved during the AFK window

- `feedback.gitignore-missing-gt-tooling-patterns` — diagnostic for the "mass stall pattern looks like context-limit but is actually gitignore".
- `feedback.polecat-salvage-stale-base` — polecat branches don't auto-rebase; old salvages include reverts of anything landed since spawn. Use selective `git checkout origin/salvage/<branch> -- <files>` instead of cherry-pick.

## Suggested first moves next session

1. **Validate scheduler.** `gt scheduler status`. Try slinging a small fresh bead (file something tiny) and verify it spawns. If it still wedges, look for another stale wisp in `/home/gastown/gt/.beads` with `gt:sling-context` label.
2. **Pick a Phase 6 direction.** vu-d8v.1 (k-NN head) is implementable today with stub embeddings — the trait shape is in `src/classifier.rs`. Or unblock vu-po7 by deciding the classifier module layout.
3. **vu-6rn slice 2** — if you want interactive testing, the tmux launch script (`scripts/run-vulthor-fixture.sh`) is the next bite-sized piece on top of the fixtures that already exist.
4. **PGP/GPG epic** — if you want to file the deferred security work as a fresh epic, no harm in queueing it.

## Snapshot for `gt prime` continuity

- Cron `2ada622a` (hourly mayor patrol) has been cancelled.
- Mayor's beads DB freshness: `/home/gastown/gt/vulthor/.beads` has 119 issues, all expected.
- Branch `master` on mayor clone is clean against origin.
- 4 salvage branches still live on origin for reference: `salvage/vu-po7-slit-wave2`, `salvage/vu-aoy-nux`, `salvage/vu-6rn-rictus`, `salvage/vu-c1s-nux-wave2`. None are blocking.

Ready for reboot.
