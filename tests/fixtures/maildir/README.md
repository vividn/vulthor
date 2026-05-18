# Maildir fixtures

A committed reference Maildir for tests and ad-hoc polecat experimentation
against a live vulthor instance (vu-6rn first slice).

## Layout

```
Inbox/                   — single user-visible folder
  cur/                   — read messages
    01-plain-text.eml:2,S
    02-html-only.eml:2,S
    03-multipart-alternative.eml:2,S
    04-with-attachment.eml:2,S
    06-multipart-related.eml:2,S
  new/                   — unread messages
    05-phishing-link.eml:2,
    07-large-body.eml:2,
  tmp/                   — empty (used by maildir writers as a staging area)

.Sent.directory/         — Maildir++ "Sent" subfolder (empty for now)
  cur/  new/  tmp/
```

## Coverage matrix

Each fixture exercises a specific code path:

| File | Content-Type | Exercises |
|------|---|---|
| `01-plain-text.eml` | `text/plain` | baseline parser path; `body_plain` populated, `body_html` is `None`. |
| `02-html-only.eml` | `text/html` | HTML→text fallback in TUI; sanitizer + iframe sandbox in web pane. |
| `03-multipart-alternative.eml` | `multipart/alternative` | vu-hy8 dual-part split; status bar `TXT+HTML` indicator; vu-c1s Shift+P toggle. |
| `04-with-attachment.eml` | `multipart/mixed` + PDF | attachment strip render; `o`/`O` open path. |
| `05-phishing-link.eml` | `text/html` | vu-6yi link-spoofing detection (display text `paypal.com` → href `attacker.evil.test`). |
| `06-multipart-related.eml` | `multipart/related` + inline PNG | vu-hy8 `inline_images` preservation; cid: round-trip. |
| `07-large-body.eml` | `text/plain` | scroll-offset behaviour for long bodies; PageUp/PageDown. |

## How to use

### Smoke-test the parser against every fixture

```bash
cargo test --test maildir_fixtures
```

The test crate (`tests/maildir_fixtures.rs`) loads each fixture via
`Email::parse_from_file` and asserts the expected `body_plain` /
`body_html` / attachment / inline-image shape.

### Manual exploration (future slice — vu-6rn follow-ups)

Future infrastructure will spawn a vulthor process pointed at this
Maildir under a tmux session with the web pane on a free high port,
and a Playwright wrapper for headless DOM inspection. That work lives
outside this slice — see `vu-6rn` in beads.

## Adding a new fixture

1. Drop the `.eml` into `Inbox/cur/<name>.eml:2,S` (read) or
   `Inbox/new/<name>.eml:2,` (unread).
2. Add a row to the coverage table above with the code path it
   exercises.
3. Update `tests/maildir_fixtures.rs` to assert the relevant
   parsed-shape invariant.
4. Keep the file small — fixtures live in the repo, not on disk.

## Maildir flag notes

The `:2,X` suffix encodes Maildir flags:
- `S` — Seen / Read
- `R` — Replied
- `T` — Trashed
- `F` — Flagged
- `D` — Draft
- empty (`:2,`) — Unread (new mail)
