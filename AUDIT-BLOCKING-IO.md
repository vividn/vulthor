# AUDIT-BLOCKING-IO.md — Phase 0.3a

Audit of every blocking I/O call reachable from the TUI render path
(the thread that drives `terminal.draw()` and `handle_input()` in
`src/main.rs::run_app`). This is a research deliverable only — no
refactor lands here. Subsequent tasks under `vu-rvm` pick off entries
in priority order.

Issue: **vu-ct2** (depends on epic **vu-rvm**, formula `mol-polecat-work`).

## TUI thread call graph (summary)

```
src/main.rs::run_app  (TUI thread, holds Mutex<App>)
├── terminal.draw(|f| ui.draw(f, &mut app))
│   └── ui::UI::draw → draw_main_layout → draw_*_pane
│       ├── draw_messages_pane
│       │   ├── app.perform_initial_loading_if_needed()       ← fs::read_dir + per-file fs::read + MIME parse  (first frame only)
│       │   └── (reads &Folder; pure)
│       ├── draw_content_pane
│       │   ├── app.email_store.get_selected_email_headers()  ← pure
│       │   └── app.email_store.get_selected_email_markdown() ← fs::read + full MIME parse  (every frame while a body-only email is on screen)
│       └── draw_attachments_pane
│           └── app.email_store.get_selected_email()          ← fs::read + full MIME parse  (every frame while attachments pane visible)
│
└── event::read() then handle_input(...)
    └── input::handle_main_view_input
        ├── handle_navigation (Folders pane)
        │   └── app.load_selected_folder_messages()           ← fs::read_dir + N × (fs::read + headers parse) on every j/k keystroke
        ├── handle_navigation (Messages pane, Down)
        │   ├── app.email_store.load_more_messages_if_needed  ← UNLIMITED full-folder scan when index+5 ≥ loaded.len
        │   └── implicit ensure_fully_loaded via subsequent draw
        ├── handle_folder_selection_and_switch_view  (Enter / 'l')
        │   └── app.email_store.ensure_current_folder_loaded_with_limit
        │       └── scanner.load_folder_emails_with_limit     ← fs::read_dir + N × (fs::read + headers parse)
        ├── handle_selection  (legacy folder Enter path)
        │   └── same as above
        └── handle_navigation (Attachments pane)
            └── app.email_store.get_selected_email()          ← fs::read + full MIME parse
```

The shared `Arc<Mutex<App>>` is also held by `src/web.rs`, so any web
handler that calls `get_current_email_for_web()` blocks waiting for
the TUI thread (and vice versa) — and once it has the lock it performs
the same `fs::read` + parse on the executor thread. Listed below as a
secondary vector because it amplifies any TUI-thread blocking, not
because the web server runs on the TUI thread.

---

## Call sites

Format per entry:

- **What** — function / call
- **Where** — `path/file.rs:line`
- **Latency budget** — plausible worst case on a daily-driver maildir
  (10k+ messages, NFS or spinning-disk MailDirs)
- **Remediation** — proposed mechanism (a follow-up `vu-rvm` task will
  pick this up; do not implement here)

### A. Render path (`terminal.draw` → `ui::UI::draw`)

#### A1. `app.perform_initial_loading_if_needed()`
- **Where:** `src/ui.rs:155` (in `draw_messages_pane`), implemented in
  `src/app.rs:410` → `src/app.rs:418 load_selected_folder_messages`
  → `src/email.rs:293 ensure_folder_at_path_loaded`
  → `src/maildir.rs:81 load_folder_emails_with_limit`
- **Blocking primitives:** `Path::exists()` ×3 (`src/maildir.rs:103`),
  `WalkDir` over `cur/` and `new/` (`src/maildir.rs:138`), per-file
  `fs::read` + `MessageParser::parse` (`src/email.rs:59 parse_headers_only`).
- **Latency budget:** First frame only. With the default
  `(visible_rows + 5).max(10)` limit, ~20–30 small reads + parses.
  On a cold NFS mount: 200ms–2s. On local SSD: sub-100ms.
- **Remediation:** Trigger via a message-bus command on UI start
  (`Cmd::LoadInbox`) handled by a `tokio::task::spawn_blocking` worker;
  initial draw shows an "Loading…" placeholder. Pairs with the
  component refactor in `vu-rvm`.

#### A2. `app.email_store.get_selected_email_markdown()`
- **Where:** `src/ui.rs:292` (in `draw_content_pane`), implemented in
  `src/email.rs:401` → `src/email.rs:85 ensure_fully_loaded`
  → `src/email.rs:71 parse_from_file` (`fs::read` + full MIME parse,
  including body decode and attachment enumeration).
- **Latency budget:** Re-runs **every frame** the content pane is
  drawn against an email whose `load_state == HeadersOnly`. After the
  first frame the email transitions to `FullyLoaded`, so subsequent
  frames are O(1); but every email *selection change* repays the cost
  on the next frame. Large multipart messages with attachments
  (10–50 MB): 100ms–1s on local disk, multi-second on NFS.
- **Remediation:** Load bodies asynchronously off-thread. Render path
  reads only from in-memory `Email` struct; on selection-change, the
  message bus emits `Cmd::LoadBody(message_id)` to a
  `tokio::task::spawn_blocking` worker that mutates the store via a
  channel + `Cmd::BodyLoaded` reply. Until then, show a "Loading
  body…" placeholder.

#### A3. `app.email_store.get_selected_email()` (attachments pane)
- **Where:** `src/ui.rs:351` (in `draw_attachments_pane`), implemented
  in `src/email.rs:374` → same `ensure_fully_loaded` chain as A2.
- **Latency budget:** Same as A2 — first frame after selection. Note
  `draw_attachments_pane` calls `get_selected_email()` while the
  content pane in the same render also calls
  `get_selected_email_markdown()`; both go through
  `ensure_fully_loaded` on the same email, but the second is a no-op
  thanks to the `FullyLoaded` transition. Worst case is still bounded
  by one `fs::read` + parse per selection change.
- **Remediation:** Same fix as A2 — once the body is loaded
  asynchronously, attachments are populated as a side effect of
  `parse_from_file`.

### B. Input path (`event::read` → `input::handle_input`)

#### B1. `app.load_selected_folder_messages()` on j/k in folder pane
- **Where:** `src/input.rs:167` (in `handle_navigation`, `ActivePane::Folders`),
  implemented in `src/app.rs:418`.
- **Blocking primitives:** Same as A1 (`load_folder_emails_with_limit`
  → `read_dir` + N × `fs::read` + parse).
- **Latency budget:** Runs on **every** j/k keystroke that changes
  folder selection, because `ensure_folder_at_path_loaded` only short-
  circuits when `is_loaded` *or* `emails` is non-empty
  (`src/email.rs:309`). The first j into an unvisited folder
  blocks for the first ~10–30 messages' headers. On a maildir with a
  100k-msg archive folder, the WalkDir startup alone is non-trivial.
  Typical worst case: 200ms–1s of latency per first-touch keystroke.
- **Remediation:** Switch to event-driven: keystroke updates selection
  index (sync, instant); a debounced worker requests folder headers
  asynchronously and pushes them into the store via a message bus.
  Folder pane renders a "(loading)" annotation until headers arrive.

#### B2. `app.email_store.load_more_messages_if_needed()` on j scroll
- **Where:** `src/input.rs:181` (in `handle_navigation`, `ActivePane::Messages`
  Down branch), implemented in `src/email.rs:344`
  → `src/maildir.rs:73 load_folder_emails` (**no limit**).
- **Latency budget:** **Worst offender by latency.** When the user
  scrolls within 5 of the loaded tail and the folder is not yet fully
  loaded, this loads **all** remaining messages on the TUI thread.
  For a 50k-msg archive folder: tens of seconds, possibly minutes on
  NFS. The TUI freezes for the duration, including the SIGINT/`q`
  handler.
- **Remediation:** Replace with a streaming/paged loader. Worker reads
  N more messages per tick into a channel; UI thread drains the
  channel between frames. Removes the "unlimited blocking load" mode
  entirely.

#### B3. `handle_folder_selection_and_switch_view` (Enter / `l` into folder)
- **Where:** `src/input.rs:247` → `src/email.rs:331 ensure_current_folder_loaded_with_limit`
  → `src/maildir.rs:81 load_folder_emails_with_limit`.
- **Latency budget:** Same blocking primitives as B1/A1; runs on
  folder-enter. Bounded to ~25 message reads + parses by the hard-
  coded `estimated_visible_rows = 20` (`src/input.rs:244`) + 5 + min 10.
  100ms–500ms in the bad case.
- **Remediation:** Folder-enter becomes a `Cmd::OpenFolder(path)` on
  the message bus; pane switch happens immediately, headers stream in
  asynchronously. Subsumed by the component refactor in `vu-rvm`.

#### B4. `handle_selection` legacy folder-Enter branch
- **Where:** `src/input.rs:298` — duplicate of B3 kept "for backward
  compatibility". Same primitives, same latency, same fix. Probably
  deletable after the refactor.

#### B5. `app.email_store.get_selected_email()` on j/k in attachments pane
- **Where:** `src/input.rs:215` (in `handle_navigation`,
  `ActivePane::Attachments`), implemented in `src/email.rs:374`.
- **Latency budget:** Re-invokes `ensure_fully_loaded` on every
  navigation key while attachments pane is active. After the first
  call the email is `FullyLoaded` so it's free; worst case is the
  same one-shot parse as A2/A3.
- **Remediation:** Same as A2 (body-load worker) — by the time the
  attachments pane is shown, the body+attachments are already loaded.

### C. Pre-TUI startup (not on the render loop, but still on the main thread)

These run before `enable_raw_mode()` so they don't freeze a live UI,
but they push out time-to-first-frame and should be moved off the
main thread once `#[tokio::main]` is doing more work than spawning
the web server.

#### C1. `Config::load(...)`
- **Where:** `src/main.rs:39` → `src/config.rs:67 load_from_file`
  → `std::fs::read_to_string(path)`.
- **Latency budget:** Single tiny TOML file; <1ms in practice.
- **Remediation:** Leave as-is, or switch to `tokio::fs::read_to_string`
  when the rest of startup is async. Low priority.

#### C2. `MaildirScanner::scan()` (initial folder structure scan)
- **Where:** `src/main.rs:59` → `src/maildir.rs:17 scan`
  → `src/maildir.rs:39 scan_folder_structure_only` (recursive
  `fs::read_dir`).
- **Latency budget:** Walks the full folder hierarchy (skipping
  `cur`/`new`/`tmp` and dot-dirs) but **does not** open any message
  files. For a maildir with 100s of folders on NFS, this can still be
  multi-second. Currently happens before the TUI is initialized, with
  a `println!` "Scanning MailDir structure…" banner.
- **Remediation:** Move into a `tokio::task::spawn_blocking` after the
  TUI is up; render a "Scanning folders…" splash in the folder pane.
  Required once we want sub-second time-to-first-frame on large
  maildirs.

### D. Indirect: web server holds the same `Mutex<App>`

The Axum handlers in `src/web.rs` acquire the same
`Arc<Mutex<App>>` and then call `get_current_email_for_web()` (which
ultimately calls `ensure_fully_loaded`). Every such call is a
**blocking parse on a tokio executor thread**, and while it holds the
mutex the TUI render loop is blocked at `app_state.lock()` in
`src/main.rs:125`. Specifically:

- **D1.** `serve_email` — `src/web.rs:79` → `app.get_current_email_for_web()`
- **D2.** `email_events` SSE poll loop — `src/web.rs:125` (fires every
  200ms; each iteration calls `get_current_email_for_web()` which can
  trigger `ensure_fully_loaded` on the executor thread)
- **D3.** `get_current_email_json` — `src/web.rs:170`

These do not live on the TUI thread, but they share the mutex, so any
of A2/A3/B5 that races with the web server can stall the UI on the
mutex itself, not on disk.

**Remediation:** Out of scope for vu-ct2 but coupled to the component
refactor. The fix is the same: a message-bus model where reads are
non-blocking observations of an in-memory store, and disk I/O happens
on a single dedicated worker. Web handlers and the TUI both consume
the store; neither initiates blocking I/O while holding a lock.

---

## Underlying blocking primitives (one place each)

These are the leaf functions that actually touch the disk. The
remediation for all of them is the same shape (`spawn_blocking` or
`tokio::fs`); listing them once so follow-up tasks can refer to them
without re-walking the call graph.

| Function | Location | Used by |
|---|---|---|
| `fs::read(&self.file_path)` | `src/email.rs:59` (`parse_headers_only`) | A1, B1, B3, B4 (per-message headers load) |
| `fs::read(&self.file_path)` | `src/email.rs:72` (`parse_from_file`) | A2, A3, B5 (full-body load via `ensure_fully_loaded`) |
| `fs::read_dir(path)` + recursion | `src/maildir.rs:45` (`scan_folder_structure_only`) | C2 (startup folder tree walk) |
| `cur_path.exists() / new_path.exists() / tmp_path.exists()` | `src/maildir.rs:103` (3 stat calls) | A1, B1, B2, B3, B4 |
| `WalkDir::new(dir_path).min_depth(1).max_depth(1)` | `src/maildir.rs:138` (`scan_emails_in_folder_with_limit`) | A1, B1, B2, B3, B4 |
| `path.is_file()` | `src/maildir.rs:189` (`is_email_file`) | per-entry stat inside B2's unbounded loop |
| `std::fs::read_to_string(path)` | `src/config.rs:68` (`load_from_file`) | C1 (startup, pre-TUI) |

---

## Priority for follow-up tasks under vu-rvm

Suggested ordering (severity × frequency). Final scoping is for the
refactor planner; this is just what the audit suggests:

1. **B2** — Unbounded `load_folder_emails` on scroll. Catastrophic for
   large folders. Likely the first user-visible "vulthor froze" bug.
2. **A2 / A3 / B5** — Full-body parse on render thread. Hits every
   selection change. Drives perceived latency.
3. **A1 / B1 / B3 / B4** — Headers loader. Lower per-call cost but
   high-frequency (j/k in folder pane).
4. **C2** — Startup folder scan. Visible at launch only; easy win once
   the message-bus exists.
5. **D1–D3** — Web-server-side amplification. Falls out of the
   component refactor automatically.
6. **C1** — Config load. Optional; cosmetic.

All five remediations share infrastructure: a message-bus + a
`spawn_blocking` worker pool feeding an in-memory store. That
infrastructure is exactly the Phase 0.2 component refactor in
`VISION.md`; **vu-rvm** can land it once and chip off these call sites
in subsequent tasks.
