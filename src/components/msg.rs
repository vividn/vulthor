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

    // Messages
    MessageMove(Dir),
    MessageOpen(MessageId),
    MessageMarkRead(MessageId),

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
