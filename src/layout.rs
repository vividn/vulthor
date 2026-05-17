// Layout & pane state.
//
// `Layout` owns the slice of state that drives ratatui's pane composition:
// which `View` (h/l) is showing, which `ActivePane` (Tab) has focus,
// whether the content pane is hidden (Alt+c), and the attachment-pane
// cursor in `SelectionState` (no AttachmentsComponent yet). Pane
// components are canonical for their own cursors. AppRoot owns one
// `Layout` directly. There is no `Arc<Mutex<Layout>>` — only `EmailStore` ships
// across the AppRoot/web boundary.
//
// Methods on `View` and `Layout` mirror the old `App` methods 1:1 so the
// behavior change is purely structural. The new piece is
// `ActivePane::to_u8`/`from_u8`, used by AppRoot to publish the focused
// pane into the `Arc<AtomicU8>` the web server reads to decide between
// serving the selected email and the welcome screen.

use crate::email::Folder;

/// One step in the left-to-right view progression (VISION.md
/// § "The View Progression"). Each variant picks which two adjacent
/// panes are visible; `h` / `l` move between adjacent views, while
/// `Tab` cycles focus inside the current view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// Folders + Messages — the default entry view.
    FolderMessages,
    /// Messages + Content (rendered email body).
    MessagesContent,
    /// Content alone (full-width body view).
    Content,
    /// Messages alone (active when Alt+c hides the content pane).
    Messages,
    /// Messages + Attachments (content-hidden mode, attachment focus).
    MessagesAttachments,
    // Slot-only scaffolds: `AccountsFolders` is reachable only when more
    // than one account is configured; `ContentDraft` is reachable only
    // while a reply draft exists.
    /// Accounts + Folders. Only reachable when more than one account
    /// is configured (`Config::is_multi_account`).
    #[allow(dead_code)]
    AccountsFolders,
    /// Content + Draft (reply pre-send view). Only reachable while a
    /// reply draft exists for the current email.
    #[allow(dead_code)]
    ContentDraft,
}

impl View {
    /// Ordered list of panes visible in this view, honoring the Alt+c
    /// content-pane toggle. The order matches the left-to-right Tab
    /// cycle inside the view.
    pub fn get_available_panes(&self, content_hidden: bool) -> Vec<ActivePane> {
        if content_hidden {
            match self {
                View::FolderMessages => vec![ActivePane::Folders, ActivePane::Messages],
                View::Messages => vec![ActivePane::Messages],
                View::MessagesAttachments => vec![ActivePane::Messages, ActivePane::Attachments],
                View::AccountsFolders => vec![ActivePane::Accounts, ActivePane::Folders],
                _ => vec![ActivePane::Messages],
            }
        } else {
            match self {
                View::FolderMessages => vec![ActivePane::Folders, ActivePane::Messages],
                View::MessagesContent => vec![ActivePane::Messages, ActivePane::Content],
                View::Content => vec![ActivePane::Content],
                View::AccountsFolders => vec![ActivePane::Accounts, ActivePane::Folders],
                View::ContentDraft => vec![ActivePane::Content, ActivePane::Draft],
                _ => vec![ActivePane::Messages],
            }
        }
    }

    /// Pane to focus when entering this view fresh (no prior cursor).
    /// Used after every `h`/`l` view transition and after toggling the
    /// content pane.
    pub fn get_default_active_pane(&self, content_hidden: bool) -> ActivePane {
        if content_hidden {
            match self {
                View::FolderMessages => ActivePane::Folders,
                View::Messages => ActivePane::Messages,
                View::MessagesAttachments => ActivePane::Attachments,
                View::AccountsFolders => ActivePane::Folders,
                _ => ActivePane::Messages,
            }
        } else {
            match self {
                View::FolderMessages => ActivePane::Folders,
                View::MessagesContent => ActivePane::Messages,
                View::Content => ActivePane::Content,
                View::AccountsFolders => ActivePane::Folders,
                View::ContentDraft => ActivePane::Draft,
                _ => ActivePane::Messages,
            }
        }
    }

    /// The view one step deeper (rightward, via `l`). Returns `None`
    /// at the rightmost view; respects `content_hidden` so Alt+c
    /// doesn't surface views with the hidden pane.
    pub fn next_view(&self, content_hidden: bool) -> Option<View> {
        if content_hidden {
            match self {
                View::AccountsFolders => Some(View::FolderMessages),
                View::FolderMessages => Some(View::Messages),
                View::Messages => Some(View::MessagesAttachments),
                View::MessagesAttachments => None,
                _ => None,
            }
        } else {
            match self {
                View::AccountsFolders => Some(View::FolderMessages),
                View::FolderMessages => Some(View::MessagesContent),
                View::MessagesContent => Some(View::Content),
                View::Content => None,
                _ => None,
            }
        }
    }

    /// The view one step broader (leftward, via `h`). Inverse of
    /// [`Self::next_view`]; returns `None` at `FolderMessages`.
    pub fn prev_view(&self, content_hidden: bool) -> Option<View> {
        if content_hidden {
            match self {
                View::MessagesAttachments => Some(View::Messages),
                View::Messages => Some(View::FolderMessages),
                View::FolderMessages => None,
                _ => None,
            }
        } else {
            match self {
                View::Content => Some(View::MessagesContent),
                View::MessagesContent => Some(View::FolderMessages),
                View::FolderMessages => None,
                _ => None,
            }
        }
    }
}

/// Identifier for a focusable pane. Encoded into the
/// `Arc<AtomicU8>` the web server reads, so the numbering is stable
/// (see [`Self::to_u8`] / [`Self::from_u8`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivePane {
    /// Folder tree pane.
    Folders,
    /// Email list pane.
    Messages,
    /// Rendered body pane.
    Content,
    /// Attachment list pane (content-hidden alternative).
    Attachments,
    /// Accounts pane (multi-account installs only).
    #[allow(dead_code)]
    Accounts,
    /// Reply-draft pane (active during compose / pre-send).
    #[allow(dead_code)]
    Draft,
}

impl ActivePane {
    /// Encode for the `Arc<AtomicU8>` the web server reads. Stable
    /// numbering — adding a variant means appending; never reorder.
    pub fn to_u8(self) -> u8 {
        match self {
            ActivePane::Folders => 0,
            ActivePane::Messages => 1,
            ActivePane::Content => 2,
            ActivePane::Attachments => 3,
            ActivePane::Accounts => 4,
            ActivePane::Draft => 5,
        }
    }

    /// Decode a value previously produced by [`Self::to_u8`]. Unknown
    /// values fall back to `Folders` rather than panicking, so a stale
    /// `Arc<AtomicU8>` from a renumbered variant cannot crash the web
    /// server.
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => ActivePane::Messages,
            2 => ActivePane::Content,
            3 => ActivePane::Attachments,
            4 => ActivePane::Accounts,
            5 => ActivePane::Draft,
            _ => ActivePane::Folders,
        }
    }

    /// Whether the web pane should serve the selected email (vs welcome).
    /// Mirrors the legacy `App::get_current_email_for_web` pane match.
    pub fn serves_email(self) -> bool {
        matches!(
            self,
            ActivePane::Messages
                | ActivePane::Content
                | ActivePane::Attachments
                | ActivePane::Draft
        )
    }
}

/// Cursor state owned by [`Layout`]. Per-pane components
/// (`FoldersComponent`, `MessagesComponent`, `ContentComponent`) are
/// canonical for their own cursors; `attachment_index` has no
/// component yet because `AppRoot::handle_residual_key` mutates it
/// directly on the layout.
#[derive(Debug, Default, Clone)]
pub struct SelectionState {
    /// Cursor in the attachment list.
    pub attachment_index: usize,
}

/// Direction passed to [`Layout::switch_pane`] for Tab / Shift-Tab.
#[derive(Debug, Clone)]
pub enum PaneSwitchDirection {
    /// Shift-Tab — previous pane in the view's order (wraps to last).
    Left,
    /// Tab — next pane in the view's order (wraps to first).
    Right,
}

/// Pane composition state owned directly by `AppRoot`. Encapsulates
/// which [`View`] is visible, which [`ActivePane`] has focus, whether
/// the content pane is hidden (Alt+c), and the attachment-pane cursor
/// in [`SelectionState`].
#[derive(Debug)]
pub struct Layout {
    /// Currently visible view (h/l progression).
    pub current_view: View,
    /// Focused pane within `current_view` (Tab / Shift-Tab cycles it).
    pub active_pane: ActivePane,
    /// Alt+c toggle: when true, the content pane is hidden and views
    /// shift to their content-less alternatives.
    pub content_pane_hidden: bool,
    /// Attachment-pane cursor (no dedicated component yet).
    pub selection: SelectionState,
    /// `?` help overlay visibility (vu-dzm). When true, `ui.rs` paints
    /// the keyboard cheatsheet on top of the normal pane layout.
    pub show_help: bool,
}

impl Layout {
    /// Default startup layout: `FolderMessages` view with focus on the
    /// Folders pane, content pane visible, all cursors at zero.
    pub fn new() -> Self {
        Self {
            current_view: View::FolderMessages,
            active_pane: ActivePane::Folders,
            content_pane_hidden: false,
            selection: SelectionState::default(),
            show_help: false,
        }
    }

    /// Toggle Alt+c. Adjusts `current_view` so the displayed panes stay
    /// consistent with the new visibility, and resets focus to the new
    /// view's default pane. Mirrors the legacy `App::toggle_content_pane`.
    pub fn toggle_content_pane(&mut self) {
        self.content_pane_hidden = !self.content_pane_hidden;
        if self.content_pane_hidden {
            match self.current_view {
                View::MessagesContent | View::Content => {
                    self.current_view = View::Messages;
                }
                View::FolderMessages => {}
                _ => {
                    self.current_view = View::Messages;
                }
            }
        } else {
            match self.current_view {
                View::Messages | View::MessagesAttachments => {
                    self.current_view = View::MessagesContent;
                }
                View::FolderMessages => {}
                _ => {
                    self.current_view = View::MessagesContent;
                }
            }
        }
        self.active_pane = self
            .current_view
            .get_default_active_pane(self.content_pane_hidden);
    }

    /// Advance one step rightward in the view progression (`l`).
    /// No-op when already at the rightmost view. Focus snaps to the
    /// new view's default pane.
    pub fn next_view(&mut self) {
        if let Some(new_view) = self.current_view.next_view(self.content_pane_hidden) {
            self.current_view = new_view;
            self.active_pane = self
                .current_view
                .get_default_active_pane(self.content_pane_hidden);
        }
    }

    /// Move one step leftward in the view progression (`h`). No-op at
    /// the leftmost view. Focus snaps to the new view's default pane.
    pub fn prev_view(&mut self) {
        if let Some(new_view) = self.current_view.prev_view(self.content_pane_hidden) {
            self.current_view = new_view;
            self.active_pane = self
                .current_view
                .get_default_active_pane(self.content_pane_hidden);
        }
    }

    /// Tab/Shift-Tab pane cycle within the current view. Returns the
    /// (old, new) pane pair so AppRoot can fire the right blur message.
    pub fn switch_pane(&mut self, direction: PaneSwitchDirection) -> (ActivePane, ActivePane) {
        let available = self
            .current_view
            .get_available_panes(self.content_pane_hidden);
        let old = self.active_pane;
        if available.is_empty() {
            return (old, old);
        }
        let current_index = available
            .iter()
            .position(|p| *p == self.active_pane)
            .unwrap_or(0);
        let new_index = match direction {
            PaneSwitchDirection::Left => {
                if current_index > 0 {
                    current_index - 1
                } else {
                    available.len() - 1
                }
            }
            PaneSwitchDirection::Right => {
                if current_index < available.len() - 1 {
                    current_index + 1
                } else {
                    0
                }
            }
        };
        self.active_pane = available[new_index];
        (old, self.active_pane)
    }
}

impl Default for Layout {
    fn default() -> Self {
        Self::new()
    }
}

/// Resolve a flat display index (counting subfolders with their depth) to
/// the path of subfolder indices the store uses. Moved out of `input.rs`
/// — it's purely a function over the folder tree, not input handling.
pub fn get_folder_path_from_display_index(
    folder: &Folder,
    display_index: usize,
) -> Option<Vec<usize>> {
    let flat = build_flat_folder_list(folder, 0);
    if display_index < flat.len() {
        let (target, _depth) = &flat[display_index];
        return find_folder_path(folder, target);
    }
    None
}

fn find_folder_path(current: &Folder, target: &Folder) -> Option<Vec<usize>> {
    if std::ptr::eq(current, target) {
        return Some(Vec::new());
    }
    for (i, sub) in current.subfolders.iter().enumerate() {
        if let Some(mut path) = find_folder_path(sub, target) {
            path.insert(0, i);
            return Some(path);
        }
    }
    None
}

fn build_flat_folder_list(folder: &Folder, depth: usize) -> Vec<(&Folder, usize)> {
    let mut result = Vec::new();
    if depth > 0 {
        result.push((folder, depth));
    }
    for sub in folder.get_sorted_subfolders() {
        result.extend(build_flat_folder_list(sub, depth + 1));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accounts_folders_view_exposes_accounts_then_folders() {
        let panes = View::AccountsFolders.get_available_panes(false);
        assert_eq!(panes, vec![ActivePane::Accounts, ActivePane::Folders]);
    }

    #[test]
    fn content_draft_view_exposes_content_then_draft() {
        let panes = View::ContentDraft.get_available_panes(false);
        assert_eq!(panes, vec![ActivePane::Content, ActivePane::Draft]);
    }

    #[test]
    fn accounts_folders_default_focus_is_folders() {
        assert_eq!(
            View::AccountsFolders.get_default_active_pane(false),
            ActivePane::Folders
        );
    }

    #[test]
    fn content_draft_default_focus_is_draft() {
        assert_eq!(
            View::ContentDraft.get_default_active_pane(false),
            ActivePane::Draft
        );
    }

    #[test]
    fn new_view_variants_are_not_yet_wired_into_navigation_chain() {
        // The Accounts → Folders direction goes through `next_view` so
        // 'l' from the Accounts pane advances back into the folder list.
        // The conditional reverse direction (`h` from FolderMessages →
        // AccountsFolders) is a multi-account-only policy enforced by
        // AppRoot, not layout.
        assert_eq!(
            View::AccountsFolders.next_view(false),
            Some(View::FolderMessages)
        );
        assert_eq!(View::AccountsFolders.prev_view(false), None);

        assert_eq!(View::FolderMessages.prev_view(false), None);
        assert_eq!(View::Content.next_view(false), None);
        assert_eq!(View::ContentDraft.next_view(false), None);
        assert_eq!(View::ContentDraft.prev_view(false), None);
    }

    #[test]
    fn accounts_folders_next_view_advances_to_folder_messages_in_both_layouts() {
        // Content-hidden mode (Alt+c) must keep the same Accounts →
        // FolderMessages transition so the pane is reachable from the
        // Accounts pane regardless of the content-pane toggle.
        assert_eq!(
            View::AccountsFolders.next_view(true),
            Some(View::FolderMessages)
        );
    }

    #[test]
    fn active_pane_u8_round_trip() {
        for p in [
            ActivePane::Folders,
            ActivePane::Messages,
            ActivePane::Content,
            ActivePane::Attachments,
            ActivePane::Accounts,
            ActivePane::Draft,
        ] {
            assert_eq!(ActivePane::from_u8(p.to_u8()), p);
        }
    }

    #[test]
    fn active_pane_serves_email_matches_legacy_match() {
        assert!(!ActivePane::Folders.serves_email());
        assert!(!ActivePane::Accounts.serves_email());
        assert!(ActivePane::Messages.serves_email());
        assert!(ActivePane::Content.serves_email());
        assert!(ActivePane::Attachments.serves_email());
        assert!(ActivePane::Draft.serves_email());
    }
}
