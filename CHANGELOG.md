# Changelog

All notable changes to Vulthor are documented in this file.

The format is based on [Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.0.0] - 2026-05-16

First stable release. Vulthor is now a daily-driver MailDir email client with
read, reply, send, search, and HTML viewing in a single TUI.

### Added

#### Architecture
- Custom `VulthorError` error types via `thiserror`, replacing `Box<dyn Error>`
  across the codebase with contextual variants for IO, config, mail parsing,
  and web errors.
- Component-based TUI state management — independent `Accounts`, `Folders`,
  `Messages`, `Content`, and `Draft` components communicating via an internal
  message bus, replacing the prior global `App` + `AppState` enum.
- Non-blocking I/O in the TUI thread: MailDir scans and email parsing run on
  `tokio::task::spawn_blocking` so keystrokes stay responsive on large boxes.

#### Multi-account & actions
- Multi-account support via `[accounts.*]` TOML tables, each with its own
  MailDir, SMTP command, and signature. The Accounts pane appears
  automatically when more than one account is configured.
- Action keymap in the Messages pane: `a` archive, `s`/`F` toggle star, `d`
  delete (to `Trash/`), `m` move (filterable folder picker), `U` mark
  unread, `Enter` open + auto mark-read.
- Safe MailDir mutation: actions rewrite `cur/`/`new/` flags and rename files
  according to the MailDir spec without touching the source directory layout.
- Session-only undo stack: `u` reverses the last move/star/mark action within
  the current session.

#### Compose & send
- Full compose flow opening `$EDITOR` with RFC-5322 headers and configurable
  signature, then sending via the per-account `msmtp` command.
- Pre-send preview pane: review the rendered draft before dispatch.
- Reply variants: `r` reply-all, `gr` reply-to-sender, `f` forward, `R`
  reply-later (saves an empty draft and surfaces an ⏰ chip).
- Drafts indexing: drafts live on disk in `Drafts/` with proper
  `In-Reply-To`/`References` headers, surfaced via chips on the original
  message in the list.

#### Search & viewer
- `/` search powered by `notmuch`, with live results inside the Messages
  pane.
- `v` HTML viewer launch: opens the selected message's HTML body in the
  embedded `axum` web pane.
- PWA manifest and service worker for the web viewer so it can be installed
  as a standalone HTML reader.

#### Configuration & theming
- `[web]` config section — bind address and port for the HTML viewer.
- `[ai]` config section — placeholders for the local classifier (post-v1
  feature).
- `[keybindings]` config section — override any action key, including
  multi-key chords (e.g. `gr`).
- `[theme]` config section — palette overrides loaded from a built-in
  default or a user themes directory.
- User themes directory support: drop a `<name>.toml` file into the themes
  dir and reference it as `theme = "<name>"`.
- `inotify`-based auto-refresh: external MailDir changes (e.g. from
  `mbsync`) are reflected without restarting Vulthor.

#### Tooling
- GitHub Actions CI pipeline running `cargo fmt --check`, `cargo clippy`,
  and `cargo test` on every push.
