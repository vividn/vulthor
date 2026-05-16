// `ModalComponent` — popup modal flow (Phase 1.d, vu-3e0).
//
// Hosts the general "modal eats all keys until dismissed" pattern. The
// only specialization today is the folder picker for `m` (move-to-folder);
// `/` search input will reuse the same machinery in a later phase.
//
// AppRoot owns the modal and, while `is_visible()`, routes every key
// through `ModalComponent::on_key` before any pane handler or global
// shortcut sees it. The modal mutates its own filter/selection in place
// (interior to the component, no Msg round-trip) and emits the externally-
// meaningful messages: `Msg::HideModal` on Esc, `Msg::MoveTo` on Enter.
//
// Opening is dispatched through `Msg::ShowFolderPicker`: AppRoot snapshots
// the currently-selected email and the flattened folder tree, then hands
// them to `open_folder_picker`. The snapshot locks in the target email so
// the user can't accidentally move the wrong message by scrolling under
// the open modal.

use std::cell::RefCell;
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout as RLayout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
};

use crate::email::Folder;
use crate::theme::VulthorTheme;

use super::{Component, Ctx, Msg};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderEntry {
    /// Human-readable path used for filter matching and display, e.g.
    /// "INBOX" or "Projects/Vulthor". Excludes the synthetic "Mail" root.
    pub display_path: String,
    /// Filesystem path of the folder. The maildir cur/new subdir is
    /// appended at move time.
    pub fs_path: PathBuf,
}

#[derive(Debug)]
pub struct FolderPickerState {
    /// All folders in the active account, in pane-sort order. Snapshot
    /// taken at open time so the list does not shift under the user.
    pub(super) folders: Vec<FolderEntry>,
    /// Substring filter (case-insensitive) typed by the user.
    pub(super) filter: String,
    /// Cursor into the filtered list. Reset to 0 on every filter edit.
    pub(super) selected_idx: usize,
    /// File path of the email this picker will move on Enter.
    pub source_path: PathBuf,
    /// Subject of the source email, for the "Moved: <subject> → …" status.
    pub source_subject: String,
}

impl FolderPickerState {
    /// Return the entries that survive the current filter, in display
    /// order. Empty filter matches every folder.
    pub fn filtered(&self) -> Vec<&FolderEntry> {
        if self.filter.is_empty() {
            return self.folders.iter().collect();
        }
        let needle = self.filter.to_lowercase();
        self.folders
            .iter()
            .filter(|f| f.display_path.to_lowercase().contains(&needle))
            .collect()
    }

    /// The entry the cursor currently points at, after filtering.
    pub fn selected(&self) -> Option<&FolderEntry> {
        self.filtered().get(self.selected_idx).copied()
    }
}

#[derive(Debug)]
enum ModalState {
    Hidden,
    FolderPicker(FolderPickerState),
}

pub struct ModalComponent {
    state: ModalState,
    list_state: RefCell<ListState>,
}

impl Default for ModalComponent {
    fn default() -> Self {
        Self::new()
    }
}

impl ModalComponent {
    pub fn new() -> Self {
        Self {
            state: ModalState::Hidden,
            list_state: RefCell::new(ListState::default()),
        }
    }

    pub fn is_visible(&self) -> bool {
        !matches!(self.state, ModalState::Hidden)
    }

    /// Open the folder picker. `source_path` and `source_subject` snapshot
    /// the email under the cursor at open time; `root` is the account's
    /// folder tree (`store.root_folder`).
    pub fn open_folder_picker(
        &mut self,
        source_path: PathBuf,
        source_subject: String,
        root: &Folder,
    ) {
        let mut folders = Vec::new();
        flatten_folders(root, "", &mut folders);
        self.state = ModalState::FolderPicker(FolderPickerState {
            folders,
            filter: String::new(),
            selected_idx: 0,
            source_path,
            source_subject,
        });
    }

    pub fn close(&mut self) {
        self.state = ModalState::Hidden;
    }

    /// Read-only access to the folder-picker state for tests and the
    /// renderer. Returns `None` if the modal is not currently the picker.
    pub fn folder_picker(&self) -> Option<&FolderPickerState> {
        match &self.state {
            ModalState::FolderPicker(s) => Some(s),
            _ => None,
        }
    }

    /// Render the popup centered in `area`. No-op when the modal is
    /// hidden, so callers can unconditionally invoke this after the main
    /// layout draw.
    pub fn render_overlay(&self, f: &mut Frame, area: Rect) {
        let ModalState::FolderPicker(state) = &self.state else {
            return;
        };
        let popup = center_rect(area, 60, 70);
        f.render_widget(Clear, popup);

        let outer = Block::default()
            .borders(Borders::ALL)
            .style(Style::default().fg(VulthorTheme::ACCENT_LIGHT))
            .title("Move to folder");
        let inner = outer.inner(popup);
        f.render_widget(outer, popup);

        let chunks = RLayout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(1)])
            .split(inner);

        let filter_line = Line::from(vec![
            Span::styled("/ ", Style::default().fg(VulthorTheme::GRAY_DARK)),
            Span::styled(&state.filter, Style::default().add_modifier(Modifier::BOLD)),
        ]);
        f.render_widget(Paragraph::new(filter_line), chunks[0]);

        let filtered = state.filtered();
        let items: Vec<ListItem> = filtered
            .iter()
            .map(|e| ListItem::new(e.display_path.clone()))
            .collect();
        let list = List::new(items).highlight_style(
            Style::default()
                .bg(VulthorTheme::SELECTION_BG)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        );
        let mut list_state = self.list_state.borrow_mut();
        list_state.select(if filtered.is_empty() {
            None
        } else {
            Some(state.selected_idx.min(filtered.len().saturating_sub(1)))
        });
        f.render_stateful_widget(list, chunks[1], &mut *list_state);
    }
}

impl Component for ModalComponent {
    fn handle_msg(&mut self, _msg: &Msg, _ctx: &Ctx) -> Vec<Msg> {
        // The modal does not react to broadcast messages — opening goes
        // through `open_folder_picker` (called from AppRoot::apply_root
        // on `Msg::ShowFolderPicker` after it snapshots the email) and
        // closing goes through `close()` on `Msg::HideModal` / after a
        // successful `MoveTo`.
        Vec::new()
    }

    fn render(&self, _f: &mut Frame, _area: Rect, _focused: bool, _ctx: &Ctx) {
        // The overlay needs the *full* terminal area, not the focused
        // pane's slice — use `render_overlay` from ui.rs after the main
        // draw instead. This bare impl is a deliberate no-op.
    }

    fn on_key(&mut self, key: KeyEvent, _ctx: &Ctx) -> Option<Msg> {
        let ModalState::FolderPicker(state) = &mut self.state else {
            return None;
        };
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => Some(Msg::HideModal),
            (KeyCode::Enter, _) => state.selected().map(|entry| {
                Msg::MoveTo(
                    state.source_path.to_string_lossy().into_owned(),
                    entry.fs_path.clone(),
                )
            }),
            (KeyCode::Char('j'), m) if m.is_empty() => {
                advance(state, 1);
                None
            }
            (KeyCode::Down, _) => {
                advance(state, 1);
                None
            }
            (KeyCode::Char('k'), m) if m.is_empty() => {
                retreat(state);
                None
            }
            (KeyCode::Up, _) => {
                retreat(state);
                None
            }
            (KeyCode::Backspace, _) => {
                state.filter.pop();
                state.selected_idx = 0;
                None
            }
            (KeyCode::Char(c), m) if m.is_empty() || m == KeyModifiers::SHIFT => {
                state.filter.push(c);
                state.selected_idx = 0;
                None
            }
            _ => None,
        }
    }
}

fn advance(state: &mut FolderPickerState, by: usize) {
    let len = state.filtered().len();
    if len == 0 {
        state.selected_idx = 0;
        return;
    }
    state.selected_idx = (state.selected_idx + by).min(len - 1);
}

fn retreat(state: &mut FolderPickerState) {
    if state.selected_idx > 0 {
        state.selected_idx -= 1;
    }
}

fn flatten_folders(root: &Folder, prefix: &str, out: &mut Vec<FolderEntry>) {
    for sub in root.get_sorted_subfolders() {
        let display_path = if prefix.is_empty() {
            sub.name.clone()
        } else {
            format!("{}/{}", prefix, sub.name)
        };
        out.push(FolderEntry {
            display_path: display_path.clone(),
            fs_path: sub.path.clone(),
        });
        flatten_folders(sub, &display_path, out);
    }
}

/// Return a centered sub-rect that is `pct_x`% wide and `pct_y`% tall.
fn center_rect(area: Rect, pct_x: u16, pct_y: u16) -> Rect {
    let v = RLayout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - pct_y) / 2),
            Constraint::Percentage(pct_y),
            Constraint::Percentage((100 - pct_y) / 2),
        ])
        .split(area);
    RLayout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - pct_x) / 2),
            Constraint::Percentage(pct_x),
            Constraint::Percentage((100 - pct_x) / 2),
        ])
        .split(v[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::email::{Email, EmailStore, Folder};
    use crate::theme::VulthorTheme;

    fn ctx<'a>(theme: &'a VulthorTheme, config: &'a Config, store: &'a EmailStore) -> Ctx<'a> {
        Ctx {
            theme,
            config,
            store,
        }
    }

    fn root_with_folders(names: &[&str]) -> Folder {
        let mut root = Folder::new("Mail".to_string(), PathBuf::from("/tmp"));
        for n in names {
            let f = Folder::new((*n).to_string(), PathBuf::from(format!("/tmp/{}", n)));
            root.add_subfolder(f);
        }
        root
    }

    #[test]
    fn modal_starts_hidden() {
        let m = ModalComponent::new();
        assert!(!m.is_visible());
        assert!(m.folder_picker().is_none());
    }

    #[test]
    fn open_folder_picker_flattens_root_subfolders() {
        let root = root_with_folders(&["INBOX", "Archive", "Drafts"]);
        let mut m = ModalComponent::new();
        m.open_folder_picker(PathBuf::from("/tmp/INBOX/cur/m1"), "Hello".into(), &root);
        assert!(m.is_visible());
        let s = m.folder_picker().unwrap();
        // INBOX sorts first (get_sorted_subfolders pins INBOX to the top).
        assert_eq!(
            s.folders
                .iter()
                .map(|f| f.display_path.as_str())
                .collect::<Vec<_>>(),
            vec!["INBOX", "Archive", "Drafts"],
        );
    }

    #[test]
    fn open_folder_picker_includes_nested_subfolders() {
        // Mail/Projects/Vulthor — nested folders should appear with
        // their slash-joined display path.
        let mut root = Folder::new("Mail".to_string(), PathBuf::from("/tmp"));
        let mut projects = Folder::new("Projects".to_string(), PathBuf::from("/tmp/Projects"));
        projects.add_subfolder(Folder::new(
            "Vulthor".to_string(),
            PathBuf::from("/tmp/Projects/Vulthor"),
        ));
        root.add_subfolder(projects);

        let mut m = ModalComponent::new();
        m.open_folder_picker(PathBuf::from("/tmp/INBOX/cur/m1"), "x".into(), &root);
        let names: Vec<_> = m
            .folder_picker()
            .unwrap()
            .folders
            .iter()
            .map(|f| f.display_path.as_str())
            .collect();
        assert!(names.contains(&"Projects"));
        assert!(names.contains(&"Projects/Vulthor"));
    }

    #[test]
    fn close_returns_to_hidden() {
        let root = root_with_folders(&["INBOX"]);
        let mut m = ModalComponent::new();
        m.open_folder_picker(PathBuf::from("/tmp/INBOX/cur/m1"), "x".into(), &root);
        m.close();
        assert!(!m.is_visible());
    }

    #[test]
    fn filter_typing_narrows_list_and_resets_cursor() {
        let root = root_with_folders(&["INBOX", "Archive", "Drafts"]);
        let mut m = ModalComponent::new();
        m.open_folder_picker(PathBuf::from("/tmp/INBOX/cur/m1"), "x".into(), &root);
        let (theme, config, store) = (
            VulthorTheme,
            Config::default(),
            EmailStore::new(PathBuf::from("/tmp")),
        );
        let cx = ctx(&theme, &config, &store);

        // Move cursor away from 0 first; typing must reset it.
        m.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE), &cx);
        let s = m.folder_picker().unwrap();
        assert_eq!(s.selected_idx, 1);

        // Type 'a' — matches Archive (case-insensitive) and Drafts.
        m.on_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE), &cx);
        let s = m.folder_picker().unwrap();
        assert_eq!(s.filter, "a");
        assert_eq!(s.selected_idx, 0);
        let names: Vec<_> = s
            .filtered()
            .iter()
            .map(|f| f.display_path.as_str())
            .collect();
        assert_eq!(names, vec!["Archive", "Drafts"]);
    }

    #[test]
    fn backspace_pops_filter_and_resets_cursor() {
        let root = root_with_folders(&["INBOX", "Archive"]);
        let mut m = ModalComponent::new();
        m.open_folder_picker(PathBuf::from("/tmp/INBOX/cur/m1"), "x".into(), &root);
        let (theme, config, store) = (
            VulthorTheme,
            Config::default(),
            EmailStore::new(PathBuf::from("/tmp")),
        );
        let cx = ctx(&theme, &config, &store);

        m.on_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE), &cx);
        m.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE), &cx);
        assert_eq!(m.folder_picker().unwrap().filter, "ar");
        m.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE), &cx);
        assert_eq!(m.folder_picker().unwrap().filter, "a");
    }

    #[test]
    fn jk_arrows_navigate_filtered_list() {
        let root = root_with_folders(&["INBOX", "Archive", "Drafts"]);
        let mut m = ModalComponent::new();
        m.open_folder_picker(PathBuf::from("/tmp/INBOX/cur/m1"), "x".into(), &root);
        let (theme, config, store) = (
            VulthorTheme,
            Config::default(),
            EmailStore::new(PathBuf::from("/tmp")),
        );
        let cx = ctx(&theme, &config, &store);

        m.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE), &cx);
        m.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE), &cx);
        assert_eq!(m.folder_picker().unwrap().selected_idx, 2);
        // Clamp at end.
        m.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE), &cx);
        assert_eq!(m.folder_picker().unwrap().selected_idx, 2);
        m.on_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE), &cx);
        assert_eq!(m.folder_picker().unwrap().selected_idx, 1);
        // Clamp at zero.
        m.on_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE), &cx);
        m.on_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE), &cx);
        assert_eq!(m.folder_picker().unwrap().selected_idx, 0);
    }

    #[test]
    fn esc_emits_hide_modal() {
        let root = root_with_folders(&["INBOX"]);
        let mut m = ModalComponent::new();
        m.open_folder_picker(PathBuf::from("/tmp/INBOX/cur/m1"), "x".into(), &root);
        let (theme, config, store) = (
            VulthorTheme,
            Config::default(),
            EmailStore::new(PathBuf::from("/tmp")),
        );
        let cx = ctx(&theme, &config, &store);
        let out = m.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &cx);
        assert_eq!(out, Some(Msg::HideModal));
    }

    #[test]
    fn enter_emits_move_to_with_selected_folder() {
        let root = root_with_folders(&["INBOX", "Archive"]);
        let mut m = ModalComponent::new();
        m.open_folder_picker(PathBuf::from("/tmp/INBOX/cur/m1"), "x".into(), &root);
        let (theme, config, store) = (
            VulthorTheme,
            Config::default(),
            EmailStore::new(PathBuf::from("/tmp")),
        );
        let cx = ctx(&theme, &config, &store);
        // Move to Archive (second in INBOX-pinned sort) and press Enter.
        m.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE), &cx);
        let out = m.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &cx);
        assert_eq!(
            out,
            Some(Msg::MoveTo(
                "/tmp/INBOX/cur/m1".to_string(),
                PathBuf::from("/tmp/Archive"),
            )),
        );
    }

    #[test]
    fn enter_with_empty_filtered_list_is_noop() {
        let root = root_with_folders(&["INBOX", "Archive"]);
        let mut m = ModalComponent::new();
        m.open_folder_picker(PathBuf::from("/tmp/INBOX/cur/m1"), "x".into(), &root);
        let (theme, config, store) = (
            VulthorTheme,
            Config::default(),
            EmailStore::new(PathBuf::from("/tmp")),
        );
        let cx = ctx(&theme, &config, &store);
        // Type something no folder matches.
        for c in "zzz".chars() {
            m.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), &cx);
        }
        assert!(m.folder_picker().unwrap().filtered().is_empty());
        let out = m.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &cx);
        assert_eq!(out, None);
    }

    #[test]
    fn on_key_is_noop_when_modal_hidden() {
        let (theme, config, store) = (
            VulthorTheme,
            Config::default(),
            EmailStore::new(PathBuf::from("/tmp")),
        );
        let cx = ctx(&theme, &config, &store);
        let mut m = ModalComponent::new();
        let out = m.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE), &cx);
        assert_eq!(out, None);
    }

    #[test]
    fn open_picker_records_source_email_metadata() {
        let root = root_with_folders(&["INBOX"]);
        let mut store = EmailStore::new(PathBuf::from("/tmp"));
        let mut inbox = Folder::new("INBOX".to_string(), PathBuf::from("/tmp/INBOX"));
        let mut email = Email::new(PathBuf::from("/tmp/INBOX/cur/m1"));
        email.headers.subject = "Important".into();
        inbox.add_email(email);
        store.root_folder.add_subfolder(inbox);

        let mut m = ModalComponent::new();
        m.open_folder_picker(
            PathBuf::from("/tmp/INBOX/cur/m1"),
            "Important".into(),
            &root,
        );
        let s = m.folder_picker().unwrap();
        assert_eq!(s.source_path, PathBuf::from("/tmp/INBOX/cur/m1"));
        assert_eq!(s.source_subject, "Important");
    }
}
