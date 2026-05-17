# Vulthor test fixture MailDir

This directory is the reference MailDir used by the polecat experimentation
harness (`scripts/run-vulthor-fixture.sh`) and the Playwright smoke test
(`tests/playwright/smoke.mjs`). It is intentionally separate from
`fixture/maildir/` (the dev convenience fixture under
`run-test-maildir.sh`) so the harness has a stable, minimal corpus.

## Contents (INBOX / cur and new)

| File | Scenario | DOM marker |
|------|----------|------------|
| `1700000001.1.fixture:2,S` | plain text | `FIXTURE_PLAIN_MARKER` |
| `1700000002.2.fixture:2,S` | HTML newsletter (multipart/alternative) | `#fixture-html-heading` |
| `1700000003.3.fixture:2,S` | HTML with remote `<img>` | `#fixture-remote-img` |
| `1700000004.4.fixture:2,S` | PDF attachment | `FIXTURE_ATTACHMENT_MARKER` |
| `1700000005.5.fixture` (new) | phishing-style mismatched link | `#fixture-phish-link` |
| `1700000006.6.fixture:2,S` | nested multipart (mixed → alternative) | `[data-testid=build-status]` |
| `1700000007.7.fixture:2,S` | large body (~400 paragraphs) | `FIXTURE_LARGE_MARKER_START` |

Sent/ contains a single reply so two-folder navigation works.

## Stability

Filenames use synthetic `<timestamp>.<seq>.fixture` so they sort deterministically.
Do not rewrite content casually — the smoke test and any future visual snapshots
will diff against these exact bytes.
