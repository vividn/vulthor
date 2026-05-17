# Vulthor

[![CI](https://github.com/vividn/vulthor/actions/workflows/ci.yml/badge.svg)](https://github.com/vividn/vulthor/actions/workflows/ci.yml)

A modern TUI email client for daily-driver use. Vulthor reads and writes
standard MailDirs, syncs through `mbsync`, searches through `notmuch`,
sends through `msmtp`, and pairs the terminal UI with an embedded HTML
viewer pane so rich-formatted mail still renders properly. Multiple
accounts, compose and drafts, configurable keybindings and theme, and an
opt-in local AI classifier are all built in.

## Quickstart

### 1. Install Rust

Vulthor requires the Rust 2024 edition toolchain (Rust 1.85+). Install
via [rustup](https://rustup.rs/):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

### 2. Install Vulthor

```bash
cargo install vulthor
```

Or build from source:

```bash
git clone https://github.com/vividn/vulthor.git
cd vulthor
cargo build --release
# binary at target/release/vulthor
```

Distro packaging templates live under `packaging/`:

- **Arch (AUR)** — `packaging/aur/PKGBUILD`. The `sha256sums` line is `SKIP`
  until the first release tarball is cut; replace it with the real sha before
  uploading to AUR.
- **Debian / Ubuntu** — `packaging/build-deb.sh` drives `cargo-deb`:
  ```bash
  cargo install cargo-deb
  cargo build --release
  packaging/build-deb.sh
  # .deb is written to target/debian/
  ```
  The `[package.metadata.deb]` block in `Cargo.toml` declares runtime
  dependencies (notmuch, msmtp, isync) and the install layout.

### 3. Install the companion tools

Vulthor is intentionally minimal — it doesn't sync, search, or send mail
itself. Set up each tool from its upstream documentation:

- **[mbsync](https://isync.sourceforge.io/)** — IMAP ↔ MailDir sync.
  Configure `~/.mbsyncrc`, then run `mbsync -a` on a timer.
- **[msmtp](https://marlam.de/msmtp/)** — SMTP relay for sending.
  Configure `~/.msmtprc` with one account block per address.
- **[notmuch](https://notmuchmail.org/)** (optional) — local mail index
  for `/` search. Run `notmuch setup` once, then `notmuch new` after
  each sync.

Vulthor still runs read-only over any existing MailDir without these.

### 4. Run

```bash
vulthor --help
vulthor                 # use config file (or default ~/Mail)
vulthor -p 9000         # override HTML viewer port
vulthor -m ~/OtherMail  # override MailDir path
```

## Configuration

Vulthor reads `vulthor.toml` from the first match in:

1. `-c <path>` on the command line
2. `~/.config/vulthor/config.toml`
3. `./vulthor.toml`
4. Built-in default (`~/Mail`, single account)

A minimal working config:

```toml
# Pick the account active on startup.
default_account = "personal"

# Required as a fallback when no [accounts.*] tables are defined.
maildir_path = "/home/me/Mail"

[accounts.personal]
name         = "Personal"
email        = "me@personal.tld"
maildir_path = "/home/me/Mail/personal"
smtp_command = "msmtp -a personal"   # optional; required to send
signature    = "Cheers,\nMe"         # optional
```

Add more `[accounts.<name>]` blocks for additional accounts. The
Accounts pane appears automatically when more than one is configured.

Overridable sections (all optional):

- `[accounts.<name>]` — one block per account.
- `[web]` — `port` and `bind` for the HTML viewer.
- `[keybindings]` — rebind any action (see table below).
- `[theme]` — palette overrides or a named theme from
  `~/.config/vulthor/themes/<name>.toml`.
- `[ai]` — local classifier settings (opt-in, experimental).

See `src/config.rs` for the full schema and field-level documentation.

CLI flags override the config file:

| Flag | Description |
|------|-------------|
| `-p`, `--port <PORT>` | HTML viewer port (overrides `[web].port`) |
| `-c`, `--config <PATH>` | Use a specific config file |
| `-m`, `--maildir <PATH>` | Override MailDir path |

## Keybindings

### Navigation

| Key | Action |
|-----|--------|
| `j` / `k` | Move down / up in the current pane |
| `h` / `l` | Move to broader / deeper view tier |
| `Tab` / `Shift+Tab` | Cycle panes within the current view |
| `Enter` | Enter folder, open email (auto mark-read), or activate selection |
| `Backspace` | Exit the current folder or view |
| `gg` / `G` | Jump to top / bottom |
| `gj` / `gk` | Jump to next / previous unread |

### Email actions

| Key | Action |
|-----|--------|
| `a` | Archive (move to `Archive/`) |
| `s` / `F` | Toggle star / flag |
| `d` | Delete (move to `Trash/`) |
| `m` | Move to folder (filterable picker) |
| `U` | Mark unread |
| `;` | Accept AI suggestion for current email |
| `u` | Undo last mutation (session-only) |
| `r` | Reply-all |
| `gr` | Reply to sender only |
| `f` | Forward |
| `R` | Reply-later (empty draft placeholder) |

### Search

| Key | Action |
|-----|--------|
| `/` | Search via notmuch |
| `n` / `N` | Next / previous match |

### View control

| Key | Action |
|-----|--------|
| `Alt+c` | Toggle the content pane |
| `v` | Toggle the HTML viewer window |
| `?` | Help overlay |
| `q` | Quit |

### Draft pane

| Key | Action |
|-----|--------|
| `e` | Edit body in `$EDITOR` |
| `S` | Send via `msmtp` |
| `Esc` | Discard the draft |

All keys above are rebindable via the `[keybindings]` block in
`vulthor.toml`.

## Drafts and reply variants

Drafts live in the active account's `Drafts/` folder as standard MailDir
files with `In-Reply-To` and `References` headers, so they stay
compatible with other mail clients. Each variant produces a draft for
the current message:

- **`r` — Reply-all.** Opens `$EDITOR` with the original sender, all
  recipients, and a quoted body. Default reply behaviour.
- **`gr` — Reply (sender only).** Same as `r`, but the recipient list
  is trimmed to just the original sender.
- **`f` — Forward.** Opens `$EDITOR` with the original body inlined,
  recipients left blank for you to address.
- **`R` — Reply-later.** Creates an empty draft attached to the
  current message. The message shows a ⏰ chip in the list so you can
  return to it. Drafts with real body content show a ✏ chip instead.

After saving in `$EDITOR`, Vulthor returns to a pre-send pane where you
can re-edit (`e`) or send (`S`).

## HTML viewer

Press `v` to launch a chromeless browser pinned to the currently
selected message. The viewer detects your installed browser (Chromium,
Firefox, or `xdg-open` fallback) and stays in sync with the TUI over
SSE — navigating in the terminal updates the open window instantly.
Press `v` again to close it.

## AI classifier

Vulthor ships with scaffolding for a local, on-device classifier that
suggests `archive`, `star`, or `delete` for each incoming message and
learns from corrections. The feature is **experimental and disabled by
default in v1**. The full classifier backend lands post-v1; today the
configuration block is parsed but otherwise inert.

To opt in once the backend ships:

```toml
[ai]
enabled = true
backend = "embeddings"
threshold = 0.6
```

When enabled and the model files are present, suggestions appear as
inline chips in the Messages list and on the status bar. Press `;` to
accept the suggestion for the selected message.

## Troubleshooting

- **No mail appears / inbox is empty.** Vulthor only reads MailDir; it
  doesn't sync. Run `mbsync -a` (or your equivalent) and re-launch.
  Verify `maildir_path` points at the directory `mbsync` writes into.
- **`/` search reports notmuch unavailable.** Install `notmuch` and run
  `notmuch setup`, then `notmuch new` after each sync. Vulthor probes
  for the binary on `PATH`; if it's missing, search is disabled but
  everything else continues to work.
- **Sending fails or `S` reports an error.** Vulthor shells out to
  whatever you set as `smtp_command` (typically `msmtp -a <account>`).
  Confirm the `msmtp` config at `~/.msmtprc` works on its own:
  `echo "test" | msmtp -a <account> you@example.com`. The draft is
  preserved when send fails — fix the config and retry from the
  pre-send pane.
- **Keybinding conflict at startup.** If two actions resolve to the
  same key (typically after a `[keybindings]` override that doesn't
  free the original key), Vulthor refuses to start and names both
  actions in the error. Rebind the colliding default to a free key.

## Links

- [VISION.md](VISION.md) — design intent, view progression, roadmap.
- [CHANGELOG.md](CHANGELOG.md) — release notes.
- [LICENSE](LICENSE) — MIT.

## Contributing

Issues and pull requests are welcome. Run the standard checks before
submitting:

```bash
cargo fmt --check
cargo clippy
cargo test
```

### Snapshot tests

Pane rendering is guarded by ratatui `TestBackend` snapshot tests in
`tests/snapshot_test.rs`; the expected output lives next to them under
`tests/snapshots/*.snap`. Snapshots cover the Folders, Messages,
Content, and Draft panes — a UI tweak that changes layout or text will
fail these tests until the snapshots are reviewed and re-accepted.

```bash
cargo install cargo-insta            # one-time
cargo test --test snapshot_test      # run the suite
cargo insta review                   # interactively accept changes
# or, non-interactive equivalents:
INSTA_UPDATE=always cargo test --test snapshot_test   # accept every diff
cargo insta accept                                    # accept pending .snap.new files
```

Always commit the updated `.snap` files alongside the code change that
produced them.

### Benchmarks

Performance regressions are guarded by a criterion benchmark suite in
`benches/`. The three suites cover the user-visible perf surfaces from
VISION.md (startup time, folder loading, body parsing):

```bash
cargo bench --bench startup       # cold + warm startup, first-paint work
cargo bench --bench folder_scan   # scan 100 and 10 000-message folders
cargo bench --bench body_load     # plain-text and HTML body parse + sanitize
```

Run all three with `cargo bench`. Criterion writes detailed reports to
`target/criterion/`; pass `--save-baseline <name>` to record a baseline
and `--baseline <name>` on a later run to diff against it.
