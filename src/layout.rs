// Layout & pane state — split out of the legacy `App` god object (vu-7r1).
//
// `Layout` owns the slice of state that drives ratatui's pane composition:
// which `View` (h/l) is showing, which `ActivePane` (Tab) has focus,
// whether the content pane is hidden (Alt+c), and the `SelectionState`
// indices each pane uses for its cursor. AppRoot owns one `Layout`
// directly. There is no `Arc<Mutex<Layout>>` — only `EmailStore` ships
// across the AppRoot/web boundary.
//
// Methods on `View` and `Layout` mirror the old `App` methods 1:1 so the
// behavior change is purely structural. The new piece is
// `ActivePane::to_u8`/`from_u8`, used by AppRoot to publish the focused
// pane into the `Arc<AtomicU8>` the web server reads to decide between
// serving the selected email and the welcome screen.

use crate::email::Folder;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    FolderMessages,
    MessagesContent,
    Content,
    Messages,
    MessagesAttachments,
    // Phase 0.2.4 scaffolds (vu-501). Slot-only: not yet reachable via h/l
    // navigation in non-test builds. Phase 1 wires Accounts into the
    // prev-view chain (conditional on >1 account configured); Phase 2
    // wires Draft into the next-view chain when a reply draft exists.
    #[allow(dead_code)]
    AccountsFolders,
    #[allow(dead_code)]
    ContentDraft,
}

impl View {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivePane {
    Folders,
    Messages,
    Content,
    Attachments,
    #[allow(dead_code)]
    Accounts,
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

#[derive(Debug, Default, Clone)]
pub struct SelectionState {
    pub folder_index: usize,
    pub email_index: usize,
    pub scroll_offset: usize,
    pub attachment_index: usize,
    pub remembered_email_index: Option<usize>,
}

#[derive(Debug, Clone)]
pub enum PaneSwitchDirection {
    Left,
    Right,
}

#[derive(Debug)]
pub struct Layout {
    pub current_view: View,
    pub active_pane: ActivePane,
    pub content_pane_hidden: bool,
    pub selection: SelectionState,
}

impl Layout {
    pub fn new() -> Self {
        Self {
            current_view: View::FolderMessages,
            active_pane: ActivePane::Folders,
            content_pane_hidden: false,
            selection: SelectionState::default(),
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

    pub fn next_view(&mut self) {
        if let Some(new_view) = self.current_view.next_view(self.content_pane_hidden) {
            self.current_view = new_view;
            self.active_pane = self
                .current_view
                .get_default_active_pane(self.content_pane_hidden);
        }
    }

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
        // Phase 1.a (vu-nja) wired the Accounts → Folders direction
        // through `next_view` so 'l' from the Accounts pane advances
        // back into the folder list. The conditional reverse direction
        // (`h` from FolderMessages → AccountsFolders) is a multi-
        // account-only policy enforced by AppRoot, not layout.
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
