# Observation Log

A scratchpad for things polecats notice during their work that are **out of
scope for their current bead**. The goal is to capture the insight without
derailing the in-flight work.

Mayor patrol scans this directory periodically and converts worth-doing
items into beads (or batches them into existing epics). Items not converted
remain here as a record.

## When to file an observation

File one when you notice:

- A refactor opportunity in code you touched (extract helper, simplify
  branching, remove dead code) that isn't directly required by your bead.
- A potential bug or correctness issue not in your scope.
- A performance smell (allocation in a hot path, redundant parse, etc).
- A missing test for existing functionality.
- A documentation gap (function names that don't match behavior, stale
  comments, etc).
- An architectural inconsistency relative to VISION.md or the migration
  direction.

Do **not** file an observation when:

- The issue is in scope for your bead — fix it.
- The issue blocks your bead — surface to mayor; don't bury it here.
- You're not sure it's an issue — write the observation anyway with low
  severity. It's cheap to discard later.

## Filename format

```
<YYYY-MM-DD>-<bead-id>-<short-slug>.md
```

Examples:
- `2026-05-16-vu-d3a-error-conversion-helper.md`
- `2026-05-17-vu-ktp-message-bus-back-pressure.md`

The bead-id is the bead you were working on when you spotted it. Slug is a
2-4 word lowercase-with-hyphens summary.

## File format

Required frontmatter (YAML, between `---` fences) followed by free-form
markdown body.

```markdown
---
bead: vu-d3a            # the bead you were working on
polecat: furiosa        # your polecat name (set via $POLECAT_NAME or omit)
date: 2026-05-16
files:                  # files that prompted this; relative to repo root
  - src/email.rs
  - src/maildir.rs
severity: medium        # low | medium | high
category: refactor      # refactor | bug | perf | docs | test-gap | architecture
---

# Short title (one line)

What you noticed (1-3 paragraphs). Be specific: cite file:line where
possible. Explain *why* it matters, not just *what* it is. A future reader
should be able to decide whether to act on this without re-discovering the
context.

## Suggested next step

(Optional.) What you'd do if you were tackling it:
- File a bead under epic vu-XYZ
- Write a regression test for case X
- Run a benchmark before/after
- Raise to mayor for design call
```

## Severity guidelines

- **low**: nice-to-have polish; safe to ignore for months.
- **medium**: real improvement, would compound if left; revisit within the
  current phase.
- **high**: latent bug, perf regression risk, or architectural drift that
  will hurt soon if not addressed. Mayor should triage promptly.

## Categories

- `refactor` — code clarity, structure, naming.
- `bug` — potential or latent correctness issue.
- `perf` — performance smell or regression risk.
- `docs` — code comments, README, or VISION drift.
- `test-gap` — uncovered behavior or missing regression test.
- `architecture` — inconsistency with the component/error/async migration
  direction; cross-cutting concerns.

## Examples

Good (specific, actionable):

```markdown
---
bead: vu-d3a
polecat: furiosa
date: 2026-05-16
files:
  - src/email.rs
severity: low
category: refactor
---

# Email::parse_headers and parse_body duplicate addr extraction

Both `parse_headers` (email.rs:93-117) match on the
`(name, address)` tuple shape identically — once for the from address,
once for the to. Extracting a small helper `format_addr(addr)` would cut
~15 lines and prevent the two from drifting apart.

Not blocking the thiserror migration, but worth picking up when someone is
already in this file.
```

Bad (vague, no actionable context):

```markdown
---
bead: vu-d3a
date: 2026-05-16
severity: medium
category: refactor
---

# Email module is messy

This file could use a cleanup pass.
```
