// Step 1 of the Phase-0.2 component refactor (vu-m6s).
//
// Flat `Msg` enum: a single grep target for every cross-component message
// the runtime can dispatch. Variants are grouped by addressee but the enum
// stays flat — nested variants (e.g. `Msg::Folders(FoldersMsg)`) buy us
// no type safety we actually use and add forwarding boilerplate.
//
// See DESIGN-COMPONENTS.md § "The Msg enum" for the contract this matches.

use std::path::PathBuf;

/// Opaque account identifier. Placeholder until multi-account lands in
/// Phase 1; alias keeps call sites stable.
pub type AccountId = String;

/// Path to a folder inside the active MailDir tree. Concrete type stays a
/// `PathBuf` for now — the alias documents intent.
pub type FolderPath = PathBuf;

/// Message-ID header of a parsed email. String for now; will harden into
/// a newtype if/when the store gains a real index.
pub type MessageId = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyKind {
    Reply,
    ReplyAll,
    Forward,
    ReplyLater,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Msg {
    // Global lifecycle
    Quit,
    ToggleHelp,
    StatusSet(String),
    StatusClear,

    // Layout
    ViewNext,
    ViewPrev,
    ToggleContentPane,
    FocusNext,
    FocusPrev,
    /// AppRoot publishes the new focused pane after every focus change.
    /// The web server reads this signal via `Arc<AtomicU8>` and decides
    /// between serving the selected email and the welcome screen. vu-7r1.
    FocusChanged(crate::layout::ActivePane),

    // Accounts (Phase 1)
    /// Move the cursor inside the Accounts pane. Only `Up`/`Down` are
    /// meaningful — `Left`/`Right` belong to the view-progression
    /// (`h`/`l`) handled by `AppRoot`.
    AccountMove(Dir),
    /// Switch the active account. The carried [`AccountId`] is the
    /// `[accounts.<key>]` table key from `vulthor.toml`. `AppRoot`
    /// rebuilds the [`EmailStore`] from the account's `maildir_path`
    /// and resets folder/message selection.
    ///
    /// [`EmailStore`]: crate::email::EmailStore
    AccountSelect(AccountId),

    // Folders
    FolderMove(Dir),
    FolderEnter,
    FolderLoaded(FolderPath),
    /// Back-navigation out of the current folder (Backspace from the
    /// Folders or Messages pane). Replaces the legacy
    /// `handle_back_navigation` path that wrote `selection.folder_index`
    /// directly (see notes/observations/2026-05-16-vu-sd6-backspace-...).
    /// `AppRoot::apply_root` pops the store path and resets scroll;
    /// `FoldersComponent`/`MessagesComponent` reset their own indices in
    /// `handle_msg`.
    FolderExitParent,

    // Messages
    MessageMove(Dir),
    MessageOpen(MessageId),
    MessageMarkRead(MessageId),
    /// Fired by `AppRoot` after a focus change that just blurred the
    /// Folders pane (focus moved Folders → Messages). `MessagesComponent`
    /// uses it to restore the remembered email selection — or pick the
    /// first email when there is none.
    FoldersBlur,
    /// Fired by `AppRoot` after a focus change that just blurred the
    /// Messages pane (focus moved Messages → Folders).
    /// `MessagesComponent` snapshots the current `email_index` into
    /// `remembered_email_index` so the next `FoldersBlur` can restore it.
    MessagesBlur,
    /// The user just scrolled to a message near the tail and the store
    /// may need another chunk of headers. `AppRoot` translates this into
    /// `load_more_messages_if_needed`. Carries the cursor index so the
    /// store knows where the user is reading.
    StoreLoadMore(usize),

    // Direct mutation actions (Phase 1.c, vu-bti). All three carry
    // a `MessageId` for forward compatibility, but until the store
    // grows a real index the action-key handler in `MessagesComponent`
    // emits an empty sentinel string and `AppRoot::apply_root` resolves
    // the target from the current cursor (same convention as
    // `Msg::MessageOpen`).
    /// Move the cursor-selected email to `<maildir_root>/Archive/cur/`.
    /// Creates the Archive folder on first use. Pushes an `Archive`
    /// mutation onto the undo stack.
    Archive(MessageId),
    /// Toggle the MailDir `F` (Flagged) flag on the cursor-selected
    /// email's filename. Pushes a `ToggleStar` mutation that captures
    /// the *previous* flag state so undo restores it directly.
    ToggleStar(MessageId),
    /// Move the cursor-selected email to `<maildir_root>/Trash/cur/`.
    /// Creates the Trash folder on first use. Pushes a `Delete`
    /// mutation onto the undo stack.
    Delete(MessageId),
    /// Move the cursor-selected email from `<folder>/cur/` to
    /// `<folder>/new/`, flipping `is_unread` to true and bumping the
    /// folder's `unread_count`. Idempotent: a no-op when the file is
    /// already in `new/`. Pushes a `MarkUnread` mutation onto the
    /// undo stack. Phase 1.e (vu-0o3).
    MarkUnread(MessageId),

    /// Open the folder-picker modal (Phase 1.d, vu-rr6). The
    /// `FolderPickerComponent` populates itself from the live store
    /// when it sees this message; AppRoot routes subsequent key events
    /// to the picker until it closes.
    OpenFolderPicker,
    /// Move the cursor-selected email into the folder at the given
    /// path. Carries a [`MessageId`] for forward compatibility — until
    /// the store grows a real index, AppRoot resolves the target from
    /// the cursor (same convention as `Msg::Archive` / `Msg::Delete`).
    /// The path is the folder's filesystem path; AppRoot appends
    /// `cur/<filename>` to produce the destination.
    MoveTo(MessageId, FolderPath),

    // Content
    ContentScroll(Dir, usize),

    // Draft (Phase 2)
    /// Begin a new draft for the cursor email. AppRoot stages an editor
    /// template (kind-specific quoted body etc.) and main.rs suspends the
    /// TUI to launch `$EDITOR`; on success a `DraftEditorExited` is
    /// enqueued with the resulting [`crate::compose::Compose`].
    DraftStart(ReplyKind, MessageId),
    /// Editor exited cleanly; payload is the parsed [`crate::compose::Compose`].
    /// AppRoot installs it on `DraftComponent::state`, switches the view to
    /// `ContentDraft`, and parks the status at `ReadyToSend`.
    DraftEditorExited(Box<crate::compose::Compose>),
    /// Re-enter `$EDITOR` against the in-flight draft. Phase 2.b 'e' key.
    EditorRelaunch,
    /// Pipe the current draft through msmtp and file a Sent copy. 'S' key.
    ComposeSend,
    /// Persist the current draft to `<maildir>/Drafts/cur/`. 'D' key.
    DraftSave,
    /// Drop the in-flight draft and return to the Content view. 'q'/Esc.
    ComposeDiscard,
    /// Local scroll inside the Draft pane body. Phase 2.b j/k.
    DraftScroll(Dir, usize),

    // Store mutations (handled by AppRoot/store owner)
    StoreLoadFolder(FolderPath),
    StoreLoadEmail(MessageId),

    /// Pop the most recent `Mutation` off the session undo stack and
    /// reverse it (vu-pas / Phase 1.f). No-op when the stack is empty.
    /// AppRoot is the sole handler; components do not observe undo.
    Undo,
}
