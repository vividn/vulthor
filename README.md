# 📧 Vulthor

[![CI](https://github.com/vividn/vulthor/actions/workflows/ci.yml/badge.svg)](https://github.com/vividn/vulthor/actions/workflows/ci.yml)

A modern TUI email client with an integrated HTML viewer, built in Rust.

Vulthor is a daily-driver terminal email client for people who live in the
shell but still want HTML emails rendered properly. It pairs with `mbsync`
(for sync) and `notmuch` (for search), composes via `$EDITOR`, and sends via
`msmtp`. The companion web pane is a render-only HTML viewer — not a second
UI.

## ✨ Features

- **MailDir-native**: reads (and safely mutates) standard MailDir folders. No
  IMAP/POP code — let `mbsync` do that.
- **Vim-flavored navigation**: `j`/`k` to move, `h`/`l` to step between view
  tiers (Accounts → Folders → Messages → Content), `Tab` to cycle panes.
- **Multi-account**: configure any number of `[accounts.*]` sections, each
  with its own MailDir. The Accounts pane appears automatically when more
  than one account is configured.
- **Action keymap**: `a` archive, `s`/`F` star, `d` delete, `m` move (with
  filterable folder picker), `U` mark unread, `Enter` opens and marks read.
- **Session undo**: `u` reverses the last mutation (move/star/mark) within
  the session.
- **Drafts on disk**: drafts live in the MailDir's `Drafts/` folder with
  proper `In-Reply-To`/`References` headers, so they remain compatible with
  other mail clients.
- **HTML pane**: an embedded `axum` server renders the selected email's
  HTML body in your browser and stays in sync over SSE.
- **Lazy loading**: only headers are read on folder scan; bodies load on
  demand for fast startup even on large MailDirs.
- **Help overlay**: `?` shows the full keymap at any time.

## 🚀 Installation

Vulthor builds from source with a recent Rust toolchain (edition 2024;
Rust 1.85+ recommended).

```bash
git clone https://github.com/vividn/vulthor.git
cd vulthor
cargo build --release
# binary at target/release/vulthor
```

### Packaging

Templates for downstream packagers live under `packaging/`:

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

### Pairing tools

Vulthor is intentionally minimal — it does not sync, search, or send mail by
itself. For a full daily-driver setup you'll want:

- **[mbsync](https://isync.sourceforge.io/)** (`isync` package) — IMAP ↔ MailDir sync.
- **[notmuch](https://notmuchmail.org/)** — local mail indexer used for `/` search.
- **[msmtp](https://marlam.de/msmtp/)** — SMTP relay used by Vulthor's send pipeline.

Vulthor still runs without any of these — read-only over an existing MailDir
works out of the box.

## ⚙️ Configuration

Vulthor looks for a TOML config in this order:

1. Path passed via `-c <path>`
2. `~/.config/vulthor/config.toml`
3. `./vulthor.toml`
4. Built-in default (`~/Mail`, single-account)

CLI flags override the config:

| Flag | Description |
|------|-------------|
| `-p`, `--port <PORT>` | HTML viewer port (default `8080`) |
| `-c`, `--config <PATH>` | Use a specific config file |
| `-m`, `--maildir <PATH>` | Override MailDir path |

### Single-account (legacy)

```toml
maildir_path = "/home/me/Mail"
```

### Multi-account

```toml
# Optional: which account is active on startup. Falls back to the first
# account in alphabetical order when unset.
default_account = "personal"

# Required as a fallback when no [accounts.*] tables are configured.
maildir_path = "/home/me/Mail"

[accounts.personal]
name          = "Personal"
email         = "me@personal.tld"
maildir_path  = "/home/me/Mail/personal"
smtp_command  = "msmtp -a personal"   # optional; required to send
signature     = "Cheers,\nMe"         # optional

[accounts.work]
name          = "Work"
email         = "me@company.com"
maildir_path  = "/home/me/Mail/work"
smtp_command  = "msmtp -a work"
```

The Accounts pane appears automatically when more than one `[accounts.*]`
table is configured.

### Additional config sections

- `[web]` — bind address / port for the HTML viewer.
- `[keybindings]` — overrides for any action key, including multi-key
  chords (e.g. `gr`).
- `[theme]` — palette overrides. Drop a `<name>.toml` into the themes
  directory and reference it as `theme = "<name>"`.
- `[ai]` — placeholder for the local classifier (post-v1 feature).

## ⌨️ Keybindings

### Navigation

| Key | Action |
|-----|--------|
| `j` / `k` | Move down / up in the current pane |
| `↓` / `↑` | Same, with arrow keys |
| `h` / `l` | Step to broader / deeper view tier |
| `Tab` / `Shift+Tab` | Cycle panes in the current view |
| `Enter` | Enter folder, open email (auto mark-read), or activate attachment |
| `Backspace` | Exit the current folder |
| `Page Up` / `Page Down` | Scroll content pane by 10 lines |

### Email actions (in the Messages pane)

| Key | Action |
|-----|--------|
| `a` | Archive (move to `Archive/`) |
| `s` / `F` | Toggle star/flag |
| `d` | Delete (move to `Trash/`) |
| `m` | Move to folder (opens a filterable folder picker) |
| `U` | Mark unread |
| `Enter` | Open + auto mark-read |
| `r` | Reply-all (opens `$EDITOR` with quoted body) |
| `gr` | Reply to sender only |
| `f` | Forward |
| `R` | Reply-later — empty draft saved to `Drafts/`, ⏰ chip appears |

### Global

| Key | Action |
|-----|--------|
| `/` | Search via notmuch |
| `v` | Open the selected message in the HTML viewer |
| `u` | Undo last mutation (session-only) |
| `Alt+c` | Toggle the content pane |
| `?` | Toggle help overlay |
| `q` | Quit |

## 📂 MailDir setup with mbsync

A minimal `~/.mbsyncrc` snippet:

```
IMAPAccount personal
Host        imap.example.com
User        me@personal.tld
PassCmd     "pass mail/personal"
TLSType     IMAPS

IMAPStore personal-remote
Account     personal

MaildirStore personal-local
Path        ~/Mail/personal/
Inbox       ~/Mail/personal/INBOX
SubFolders  Verbatim

Channel personal
Far     :personal-remote:
Near    :personal-local:
Patterns *
Create  Both
Expunge Both
SyncState *
```

Run `mbsync personal` (or `mbsync -a`) periodically — typically via a
systemd timer or cron job. After each sync, run `notmuch new` to refresh
the search index.

## 🔍 Search with notmuch

Index your mail once:

```bash
notmuch setup    # one-time
notmuch new      # after each mbsync run
```

Vulthor's `/` search hands queries directly to notmuch and shows results in
the Messages pane.

## ✉️ Sending with msmtp

A minimal `~/.msmtprc`:

```
defaults
auth       on
tls        on
tls_trust_file /etc/ssl/certs/ca-certificates.crt

account    personal
host       smtp.example.com
port       587
from       me@personal.tld
user       me@personal.tld
passwordeval "pass mail/personal-smtp"
```

Vulthor invokes `msmtp` via the `smtp_command` configured per account.

## 🤝 Contributing

Issues and pull requests are welcome. Please run `cargo fmt`, `cargo clippy`,
and `cargo test` before submitting.

```bash
cargo fmt --check
cargo clippy
cargo test
```

See `VISION.md` for the long-term direction and `CLAUDE.md` for working
conventions.

## 📄 License

Vulthor is licensed under the MIT License. See [LICENSE](LICENSE).
