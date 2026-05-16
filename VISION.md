# Vulthor Vision

This document encodes the long-term vision for Vulthor. Future contributors
(human or agent) should read this **before** making architectural decisions or
adding features. CLAUDE.md governs *how* to work; VISION.md governs *what* to
build.

## North Star

Vulthor is a daily-driver TUI email client for the user who lives in the
terminal but wants HTML emails rendered properly. It pairs with `mbsync`
(for sync) and `notmuch` (for search). It composes via `$EDITOR` and sends via
`msmtp`. Its signature differentiator is a **local AI classifier** that
suggests an action for every incoming email and learns from corrections.

The TUI is the primary surface. The web pane is a render-only convenience
viewer for HTML emails — never a second primary UI.

## What Vulthor Is

- A read+write MailDir client. Full daily-driver: reply, archive, delete,
  star, mark, move, compose.
- Multi-account. One MailDir per account, switchable from an Accounts pane.
- AI-assisted. A local, self-contained embedding-based classifier suggests
  archive / star / delete on each email. The user accepts with `;` or
  overrides with `a`/`s`/`d`. The classifier learns from corrections.
- Vim-flavored. j/k/h/l, hjkl-style navigation between view tiers.
- Two-surface: TUI (primary) + companion web view (HTML render).

## What Vulthor Is Not

- Not a sync engine. `mbsync` (or equivalent) handles IMAP/POP. Vulthor
  reads/writes the MailDir; it never talks to mail servers for sync.
- Not a search engine. `notmuch` provides search. Vulthor renders results.
- Not a full HTML renderer in the terminal. HTML emails render in the web
  pane (or a PWA window). The TUI shows markdown/plaintext only.
- Not a multi-protocol client. MailDir only. No IMAP/POP/JMAP code.
- Not threaded (for now). Flat message list. Revisit post-v1.

## The View Progression

The TUI is structured as a left-to-right progression of view tiers. The user
moves rightward with `l` (deeper) and leftward with `h` (broader). At any
tier, two adjacent panes are visible.

```
Accounts  →  Folders  →  Messages  →  Content  →  Draft
            (current entry point)
```

- **Accounts**: leftmost pane. Lists configured accounts with unread counts.
  Hidden when only one account is configured. Reached by `h` from Folders.
- **Folders**: folder tree for the active account. Current default entry.
- **Messages**: email list for the selected folder.
- **Content**: rendered body of the selected email (markdown/plaintext).
- **Draft**: reply draft for the current email. Reached by `l` from Content
  when a draft exists or when starting a new reply. Original email stays
  visible in the adjacent pane.

The existing `View` enum (`FolderMessages`, `MessagesContent`, `Content`, etc.)
extends naturally with `AccountsFolders` on the left and `ContentDraft` on
the right.

## Action Keybindings

The full keymap targets a daily-driver workflow. Subject to refinement, but
the shape is locked.

### Navigation
| Key | Action |
|-----|--------|
| `j` / `k` | Move down / up in current pane |
| `h` / `l` | Move to broader / deeper view tier |
| `Tab` / `S-Tab` | Cycle panes within current view |
| `Enter` | Enter selected folder / open selected email |
| `Backspace` | Go back / exit current folder |
| `gg` / `G` | Jump to top / bottom |
| `gj` / `gk` | Jump to next / previous unread |

### Email Actions
| Key | Action |
|-----|--------|
| `a` | Archive (move to Archive folder) |
| `s` | Star / flag |
| `d` | Delete (move to Trash) |
| `;` | Accept AI suggestion for current email |
| `u` | Undo last mutation |
| `r` | Reply-all (default) |
| `gr` | Reply (sender only) |
| `R` | Reply-later (creates empty draft, distinct chip) |
| `f` | Forward |
| `m` | Move to folder (prompts for destination) |
| `F` | Toggle flag |
| `U` | Mark unread |
| `Enter` (on email) | Open + auto mark-read |

### Search
| Key | Action |
|-----|--------|
| `/` | Search forward (notmuch query) |
| `n` / `N` | Next / previous match |

### View Control
| Key | Action |
|-----|--------|
| `Alt+c` | Toggle content pane visibility |
| `v` | Open/close HTML viewer window (PWA-style) |
| `?` | Help screen |
| `q` | Quit |

Keybindings are user-overridable via `[keybindings]` in `vulthor.toml`.

## AI Classifier

The classifier suggests one of `{archive, star, delete}` for each email and
learns from user corrections.

### Backend

Embeddings-based, self-contained. No external services, no subprocesses.

- A small ONNX embedding model (e.g., all-MiniLM-L6-v2 int8 quantized, ~10MB)
  embeds the email's headers + a snippet of body text.
- A lightweight classifier head (logistic regression or small MLP) maps the
  embedding to one of the three actions plus a confidence score.
- Runs in-process via `fastembed-rs` or `candle`. ~5-20ms inference on CPU.
- Model weights and trained head live in
  `~/.local/share/vulthor/classifier/`.

The `Classifier` trait abstracts the backend so a smaller TF-IDF + logistic
regression fallback can be wired in for resource-constrained users.

### Surface

- **Inline chip** in the Messages list, one per row: `[a]`, `[s]`, `[d]`, or
  none if confidence is below the threshold. Color or intensity reflects
  confidence.
- **Status bar detail** for the selected email: `Suggested: archive (87%)`.
- **Acceptance**: `;` performs the suggested action. `a`/`s`/`d` perform the
  direct action regardless of suggestion. Both record a training signal.
- **Learning**: each direct action (matching or differing from the suggestion)
  produces a labeled training example. On-device gradient updates run after
  each correction or in small batches.

The classifier is optional. If model files are missing or `ai.enabled = false`
in config, chips and status hints simply don't appear. The keybindings still
work; they're just unassisted.

### Configuration

```toml
[ai]
enabled = true
backend = "embeddings"        # or "tiny"
confidence_threshold = 0.6    # below this, no chip shown
model_dir = "~/.local/share/vulthor/classifier"
```

## Drafts & Reply-Later

Drafts live in the MailDir's `Drafts/` folder using standard MailDir
semantics. Each draft sets `In-Reply-To` (and `References`) pointing at the
original email. This keeps drafts compatible with other mail clients.

### Surfacing

- On folder load, Vulthor indexes `In-Reply-To` headers in Drafts/ to build a
  `original_message_id → draft_path` map.
- Original emails with an associated draft show a chip in the Messages list:
  - `✏` if the draft has body content (real reply in progress).
  - `⏰` if the draft body is empty (reply-later marker).
- From an email's Content pane, pressing `l` moves to the Draft pane,
  showing the original alongside the draft. The progression is:
  `Content → ContentDraft`.
- If no draft exists, `l` from Content starts a new one.

### Pre-Send Flow

When the user saves the draft in `$EDITOR` and returns, the Draft pane
becomes a review surface. The original is on the left; the draft preview is
on the right. Action keys in the Draft pane:

| Key | Action |
|-----|--------|
| `e` | Edit body again in `$EDITOR` |
| `a` | Add attachment (file picker) |
| `t` / `c` / `b` | Edit To / Cc / Bcc |
| `S` | Send (shells out to `msmtp`) |
| `D` | Save as draft and exit |
| `q` | Discard |

Sent emails are appended to the active account's `Sent/` folder.

## Undo

Session-only, in-memory. A `Vec<Mutation>` on the `App` tracks every
file-affecting action (move, flag-change, mark-read, send). `u` pops the
last entry and reverses it.

- Lost on quit. No on-disk journal.
- Sufficient for "oops, I hit `d` by mistake."
- Reversal is best-effort: if the underlying file has been moved by another
  process (e.g., `mbsync`), undo logs a warning and skips.

If this proves insufficient in practice, escalate to a persistent journal
(post-v1).

## HTML Viewer (`v`)

Pressing `v` opens the existing axum web pane in a chromeless window.

- **First implementation**: shell out to `chromium --app=URL`, `firefox --kiosk URL`,
  or detect the user's browser. Browser-agnostic via small detection logic.
- **Enhancement**: serve a `manifest.json` + minimal service worker so the
  user can "Install as app" and get a permanent PWA entry.
- The page already auto-updates via SSE as Vulthor navigates. No new sync
  logic needed.
- Pressing `v` again closes the spawned window (Vulthor tracks the child PID).
- If neither browser is available, fall back to `xdg-open`.

Native webview (`wry`/`webkit-gtk`) is a post-v1 consideration if browser
launch proves clumsy.

## Multi-Account

One MailDir per account. The user configures any number of `[accounts.*]`
sections in `vulthor.toml`. One account is "active" at a time; the Accounts
pane lets the user switch.

```toml
[accounts.work]
name = "Work"
email = "me@company.com"
maildir_path = "~/Mail/work"
smtp_command = "msmtp -a work"
signature = "Best,\nMe"

[accounts.personal]
name = "Personal"
email = "me@personal.tld"
maildir_path = "~/Mail/personal"
smtp_command = "msmtp -a personal"
```

Vulthor does not aggregate unread counts into a unified inbox. Each account
is a separate world; switching is explicit.

## Configuration Schema

`vulthor.toml` (search order: `-c` flag → `~/.config/vulthor/config.toml` →
`./vulthor.toml`):

```toml
# Global settings
default_account = "personal"   # which account is active on startup

[web]
port = 8080
bind = "127.0.0.1"

[accounts.<name>]
# see Multi-Account section

[ai]
# see AI Classifier section

[keybindings]
# overrides for any key in the action map
archive = "e"          # rebind 'a' to 'e'
reply = "gr"           # confirm or change defaults

[theme]
# overrides for the Vulthor color palette
primary = "#2C4F5D"
accent = "#FF8C42"
# ... etc
```

A single `default` theme ships built-in. Users can drop additional themes
at `~/.config/vulthor/themes/*.toml` and reference by name.

## Search

`notmuch` is the search backend. Vulthor assumes the user runs `notmuch new`
periodically (typically via `mbsync` post-hook).

- `/` opens a query input at the bottom of the screen.
- Query syntax is notmuch's native query language.
- Results render as a virtual folder in the Messages pane.
- If notmuch is not installed or `notmuch_path` is unset, `/` shows a status
  message and does nothing. Vulthor remains usable without search.

Vulthor does **not** build its own full-text index. The AI classifier uses
its own per-message embedding store, which does not duplicate notmuch's
inverted index.

## Compose & Send

- `r` / `gr` / `f` / `R` opens `$EDITOR` (or `$VISUAL`) with a templated
  draft (headers, quoted body, signature).
- On editor exit, Vulthor reads the file and enters the Draft pre-send pane.
- Sending shells out to `msmtp` (configurable via `smtp_command` per account).
  Vulthor never opens an SMTP connection itself.
- After successful send, the message is appended to the account's `Sent/`
  folder following MailDir conventions.
- If `msmtp` fails, the draft is preserved and the error appears in the
  status bar.

## Architecture Refactors (Phase 0)

The current code is functional but not the foundation we want for v1.0.
Before adding features, three refactors land:

1. **Custom error types via `thiserror`**. Replace `Box<dyn Error>` with a
   typed `VulthorError` enum. Each module gets context-rich errors.
2. **Component-based state management**. The global `App` struct + `AppState`
   enum is replaced by independent components (Accounts, Folders, Messages,
   Content, Draft) communicating via a message bus. Inspired by Elm /
   The Elm Architecture, adapted to Rust + ratatui.
3. **Selective async unification**. The TUI loop runs sync; the web server
   runs async. Long-term goal is unified async, but the immediate refactor
   just removes blocking I/O from the TUI thread (MailDir scans, email
   parsing) using `tokio::task::spawn_blocking` or full `tokio::fs`.

These refactors are **prerequisites**, not parallel work. Feature work
lands on the clean foundation, not the old one.

## v1.0 Milestone

v1.0 ships when the following work end-to-end:

- [x] MailDir scan, lazy email load, content pane render *(done)*
- [x] Web pane with SSE auto-update *(done)*
- [ ] Phase-0 refactor: thiserror + components + non-blocking I/O
- [ ] Accounts pane with multi-account switching
- [ ] Mark-as-read on Enter; move new/ → cur/
- [ ] Action keys: a / s / d / u / m / F / U
- [ ] Compose flow: r / gr / f / R + `$EDITOR` + msmtp send
- [ ] Drafts surfacing with In-Reply-To linkage; pre-send review pane
- [ ] notmuch search via `/`
- [ ] HTML viewer (`v`) launching a chromeless browser window
- [ ] Full TOML config: accounts, keybindings, theme
- [ ] Comprehensive test coverage (TDD)

The AI classifier is **post-v1** but the classifier interface (trait,
suggestion chip in list, `;` accept) ships in v1 disabled-by-default so
the feature can be enabled without code changes.

## Roadmap Phases

- **Phase 0 — Foundation** (1-3 weeks): refactors above.
- **Phase 1 — Multi-account + write** (2-4 weeks): Accounts pane, action
  keys, undo, MailDir mutation safety.
- **Phase 2 — Compose** (2-4 weeks): editor integration, msmtp send, draft
  lifecycle, pre-send pane.
- **Phase 3 — Search + viewer** (1-2 weeks): notmuch integration, HTML viewer.
- **Phase 4 — Config & polish** (1-2 weeks): full TOML config, theme system,
  keybinding overrides, documentation.
- **Phase 5 — v1.0 release**: tagging, packaging.
- **Phase 6 — AI** (3-6 weeks post-v1): embedding pipeline, classifier head,
  on-device learning, suggestion UX.

## Anti-Goals

These are explicitly **not** going to be built:

- IMAP/POP/JMAP client code. `mbsync` does this.
- Multi-protocol or non-MailDir storage backends.
- Threaded conversation view (revisit after v1 based on user feedback).
- Mail filtering / sieve rules. `afew` / procmail / sieve servers handle this.
- Encrypted mail composition (PGP/SMIME). Possibly post-v1; not in scope.
- Calendar / CalDAV integration.
- LDAP address book integration.
- A web pane that is anything more than a render-only HTML viewer.

## Open Questions

Items marked deliberately to revisit:

- Reply-later icon: `⏰` chip vs. another glyph — settle when implementing.
- AI suggestion firing scope: inbox-only, or all folders? (Default proposal:
  inbox + unread elsewhere; configurable.)
- MailDir refresh cadence: continuous watch via `inotify`, manual `G`-key
  refresh, or both? (Default proposal: inotify watch + manual force-refresh.)
- Folder picker for `m` (move): full filterable list, recent-folders quick
  list, or both?
