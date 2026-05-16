# Vulthor 1.0.0

First stable release. Vulthor is a modern TUI email client for MailDir with an
integrated HTML viewer — daily-driver-ready for read, reply, send, search, and
HTML rendering, all in a single terminal interface.

Vulthor pairs with `mbsync` (sync), `notmuch` (search), and `msmtp` (send),
and composes via `$EDITOR`.

## Highlights

### Read, write, and act on mail
- **Five-pane workflow**: Accounts → Folders → Messages → Content → Draft,
  navigable with vim-style keys (`h`/`j`/`k`/`l`).
- **Action keymap** in the Messages pane:
  - `a` archive · `s`/`F` toggle star · `d` delete (to `Trash/`)
  - `m` move (filterable folder picker)
  - `U` mark unread · `Enter` open + auto mark-read
- **Session-only undo** (`u`) reverses the last move/star/mark action.
- **Safe MailDir mutation**: actions rewrite `cur/`/`new/` flags and rename
  files per the MailDir spec, without touching the source directory layout.

### Compose & send
- Full compose flow opens `$EDITOR` with RFC-5322 headers and your
  configured signature, then sends via the per-account `msmtp` command.
- **Pre-send preview** lets you review the rendered draft before dispatch.
- **Reply variants**:
  - `r` reply-all · `gr` reply-to-sender · `f` forward
  - `R` reply-later (saves an empty draft and surfaces an ⏰ chip)
- **Drafts indexing**: drafts live on disk in `Drafts/` with proper
  `In-Reply-To`/`References` headers, surfaced via chips on the original
  message.

### Multi-account
- `[accounts.*]` TOML tables, each with its own MailDir, SMTP command, and
  signature.
- The Accounts pane appears automatically when more than one account is
  configured.

### Search
- `/` opens a search powered by `notmuch`, with live results inside the
  Messages pane.

### HTML viewer
- `v` launches the selected message's HTML body in an embedded `axum` web
  pane.
- **PWA-ready**: a manifest and service worker ship with the viewer so it
  can be installed as a standalone HTML reader.

### Configuration & theming
- `[web]` — bind address and port for the HTML viewer.
- `[keybindings]` — override any action key, including multi-key chords
  (e.g. `gr`).
- `[theme]` — palette overrides loaded from a built-in default or your
  user themes directory; drop a `<name>.toml` into the themes dir and
  reference it as `theme = "<name>"`.
- `[ai]` — placeholders for a local classifier (a post-v1 feature).

### Live refresh
- `inotify`-based auto-refresh: external MailDir changes (e.g. from
  `mbsync`) are reflected without restarting Vulthor.

## Under the hood

- Custom `VulthorError` error types via `thiserror`.
- Component-based TUI state (`Accounts`, `Folders`, `Messages`, `Content`,
  `Draft`) communicating via an internal message bus.
- Non-blocking I/O on the TUI thread — MailDir scans and email parsing run
  on `tokio::task::spawn_blocking` so keystrokes stay responsive on large
  mailboxes.

## Installation

`cargo install vulthor` once published; AUR and `.deb` packaging are
included in the source tree.

## Compatibility

- Requires `mbsync` / `isync` for sync, `notmuch` for search, and `msmtp`
  for sending (each optional but recommended for the full experience).
- Tested on Linux terminals (crossterm-based, cross-platform-capable).
