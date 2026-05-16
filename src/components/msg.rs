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

    // Accounts (Phase 1, scaffold now)
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

    // Content
    ContentScroll(Dir, usize),

    // Draft (Phase 2)
    DraftStart(ReplyKind, MessageId),
    DraftEditorExited,
    DraftSend,

    // Store mutations (handled by AppRoot/store owner)
    StoreLoadFolder(FolderPath),
    StoreLoadEmail(MessageId),
}
