// `FolderPickerComponent` — modal "move to folder" picker.
//
// Owns its own visibility flag. When `visible == true`, AppRoot routes
// every key event here first; the picker absorbs all keys until it
// returns `Msg::MoveTo` (Enter) or hides itself (Esc).
//
// State:
//   - `visible`            modal currently shown
//   - `filter_text`        substring filter applied to the flat folder list
//   - `folder_list`        full flattened list of (display, fs_path)
//   - `selected_index`     cursor over the *filtered* view
//
// The picker renders via `render_modal` (called by `ui::UI::draw` only
// when `visible`). The bare-trait `render` is a no-op because the modal
// overlays the existing pane layout — see Ratatui's `Clear` widget call
// in `render_modal`.
//
// **j/k vs filter chars.** The bead text reads "j/k navigate, type for
// filter", but these two rules collide when `j` is also a typeable char.
// The picker resolves it as: arrow keys + Ctrl-N/Ctrl-P navigate,
// plain alphanumeric (including `j`/`k`) types into the filter. This
// matches the fzf convention TUI users expect.

use std::cell::RefCell;
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout as RLayout, Rect},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
};

use crate::email::Folder;
use crate::theme::VulthorTheme;

use super::{Component, Ctx, Msg};

pub struct FolderPickerComponent {
    pub visible: bool,
    pub filter_text: String,
    pub selected_index: usize,
    pub folder_list: Vec<(String, PathBuf)>,
    list_state: RefCell<ListState>,
}

impl FolderPickerComponent {
    pub fn new() -> Self {
        Self {
            visible: false,
            filter_text: String::new(),
            selected_index: 0,
            folder_list: Vec::new(),
            list_state: RefCell::new(ListState::default()),
        }
    }

    /// Show the modal and rebuild the folder list from the live store.
    pub fn open(&mut self, root: &Folder) {
        self.visible = true;
        self.filter_text.clear();
        self.selected_index = 0;
        self.folder_list = Self::build_folder_list(root);
    }

    /// Hide the modal and drop the cached folder list.
    pub fn close(&mut self) {
        self.visible = false;
        self.filter_text.clear();
        self.selected_index = 0;
        self.folder_list.clear();
    }

    /// Filtered view (case-insensitive substring match on the display
    /// label). Returns indices into `folder_list` so the caller can
    /// re-resolve the picked path even after the filter changes.
    pub fn filtered_indices(&self) -> Vec<usize> {
        if self.filter_text.is_empty() {
            (0..self.folder_list.len()).collect()
        } else {
            let q = self.filter_text.to_lowercase();
            self.folder_list
                .iter()
                .enumerate()
                .filter(|(_, (label, _))| label.to_lowercase().contains(&q))
                .map(|(i, _)| i)
                .collect()
        }
    }

    fn build_folder_list(root: &Folder) -> Vec<(String, PathBuf)> {
        let mut out = Vec::new();
        for sub in root.get_sorted_subfolders() {
            collect_folders(sub, "", &mut out);
        }
        out
    }

    fn clamp_selection(&mut self, len: usize) {
        if len == 0 {
            self.selected_index = 0;
        } else if self.selected_index >= len {
            self.selected_index = len - 1;
        }
    }

    /// Draw the centered modal overlay. No-op when `!self.visible`.
    pub fn render_modal(&self, f: &mut Frame, screen: Rect) {
        if !self.visible {
            return;
        }
        let modal = centered_rect(screen, 60, 70);
        f.render_widget(Clear, modal);

        let outer = Block::default()
            .borders(Borders::ALL)
            .style(Style::default().fg(VulthorTheme::CYAN))
            .title("Move to folder (Esc to cancel)");
        let inner = outer.inner(modal);
        f.render_widget(outer, modal);

        let chunks = RLayout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(1)])
            .split(inner);

        let filter = Paragraph::new(format!("Filter: {}", self.filter_text))
            .block(Block::default().borders(Borders::ALL));
        f.render_widget(filter, chunks[0]);

        let filtered = self.filtered_indices();
        let items: Vec<ListItem> = filtered
            .iter()
            .map(|&i| ListItem::new(Line::from(self.folder_list[i].0.clone())))
            .collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL))
            .highlight_style(
                Style::default()
                    .bg(VulthorTheme::SELECTION_BG)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            );

        let mut state = self.list_state.borrow_mut();
        let sel = if filtered.is_empty() {
            None
        } else {
            Some(self.selected_index.min(filtered.len() - 1))
        };
        state.select(sel);
        f.render_stateful_widget(list, chunks[1], &mut *state);
    }

    /// Resolve the path the cursor currently points at, given the filter.
    fn selected_path(&self) -> Option<PathBuf> {
        let filtered = self.filtered_indices();
        let idx = self.selected_index.min(filtered.len().saturating_sub(1));
        filtered
            .get(idx)
            .map(|&full_idx| self.folder_list[full_idx].1.clone())
    }
}

impl Default for FolderPickerComponent {
    fn default() -> Self {
        Self::new()
    }
}

fn collect_folders(folder: &Folder, prefix: &str, out: &mut Vec<(String, PathBuf)>) {
    let label = if prefix.is_empty() {
        folder.name.clone()
    } else {
        format!("{}/{}", prefix, folder.name)
    };
    out.push((label.clone(), folder.path.clone()));
    for sub in folder.get_sorted_subfolders() {
        collect_folders(sub, &label, out);
    }
}

/// Center an inner rect inside `outer`, sized as the given % of width/height.
fn centered_rect(outer: Rect, pct_x: u16, pct_y: u16) -> Rect {
    let v = RLayout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - pct_y) / 2),
            Constraint::Percentage(pct_y),
            Constraint::Percentage((100 - pct_y) / 2),
        ])
        .split(outer);
    RLayout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - pct_x) / 2),
            Constraint::Percentage(pct_x),
            Constraint::Percentage((100 - pct_x) / 2),
        ])
        .split(v[1])[1]
}

impl Component for FolderPickerComponent {
    fn handle_msg(&mut self, msg: &Msg, ctx: &Ctx) -> Vec<Msg> {
        if matches!(msg, Msg::OpenFolderPicker) {
            self.open(&ctx.store.root_folder);
        }
        Vec::new()
    }

    fn render(&self, _f: &mut Frame, _area: Rect, _focused: bool, _ctx: &Ctx) {
        // Modal renders via `render_modal` from `ui::UI::draw`. The
        // bare-trait impl stays a no-op so this component doesn't fight
        // with the pane layout.
    }

    fn on_key(&mut self, key: KeyEvent, _ctx: &Ctx) -> Option<Msg> {
        if !self.visible {
            return None;
        }
        let filtered_len = self.filtered_indices().len();
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => {
                self.close();
                None
            }
            (KeyCode::Enter, _) => {
                let target = self.selected_path();
                self.close();
                target.map(|p| Msg::MoveTo(String::new(), p))
            }
            (KeyCode::Up, _)
            | (KeyCode::Char('p'), KeyModifiers::CONTROL)
            | (KeyCode::Char('k'), KeyModifiers::CONTROL) => {
                if self.selected_index > 0 {
                    self.selected_index -= 1;
                }
                None
            }
            (KeyCode::Down, _)
            | (KeyCode::Char('n'), KeyModifiers::CONTROL)
            | (KeyCode::Char('j'), KeyModifiers::CONTROL) => {
                if self.selected_index + 1 < filtered_len {
                    self.selected_index += 1;
                }
                None
            }
            (KeyCode::Backspace, _) => {
                self.filter_text.pop();
                self.selected_index = 0;
                None
            }
            (KeyCode::Char(c), m) if m.is_empty() || m == KeyModifiers::SHIFT => {
                self.filter_text.push(c);
                self.selected_index = 0;
                let new_len = self.filtered_indices().len();
                self.clamp_selection(new_len);
                None
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::email::{EmailStore, Folder};

    fn store_with_folders() -> EmailStore {
        let mut store = EmailStore::new(PathBuf::from("/tmp"));
        for name in ["INBOX", "Archive", "Projects", "Trash"] {
            let mut f = Folder::new(name.to_string(), PathBuf::from(format!("/tmp/{}", name)));
            f.is_loaded = true;
            store.root_folder.add_subfolder(f);
        }
        // Nested: INBOX/Sub
        let mut inbox = Folder::new("INBOX".to_string(), PathBuf::from("/tmp/INBOX"));
        let sub = Folder::new("Sub".to_string(), PathBuf::from("/tmp/INBOX/Sub"));
        inbox.add_subfolder(sub);
        // Replace INBOX
        store.root_folder.subfolders[0] = inbox;
        store
    }

    fn ctx<'a>(theme: &'a VulthorTheme, config: &'a Config, store: &'a EmailStore) -> Ctx<'a> {
        Ctx {
            theme,
            config,
            store,
        }
    }

    #[test]
    fn open_populates_flat_folder_list() {
        let store = store_with_folders();
        let (theme, config) = (VulthorTheme, Config::default());
        let ctx = ctx(&theme, &config, &store);
        let mut p = FolderPickerComponent::new();
        assert!(!p.visible);
        p.handle_msg(&Msg::OpenFolderPicker, &ctx);
        assert!(p.visible);
        // INBOX first (sorted), then INBOX/Sub (nested), then Archive…
        let labels: Vec<&str> = p.folder_list.iter().map(|(l, _)| l.as_str()).collect();
        assert!(labels.contains(&"INBOX"));
        assert!(labels.contains(&"INBOX/Sub"));
        assert!(labels.contains(&"Archive"));
        assert!(labels.contains(&"Projects"));
        assert!(labels.contains(&"Trash"));
    }

    #[test]
    fn close_clears_state() {
        let store = store_with_folders();
        let (theme, config) = (VulthorTheme, Config::default());
        let ctx = ctx(&theme, &config, &store);
        let mut p = FolderPickerComponent::new();
        p.handle_msg(&Msg::OpenFolderPicker, &ctx);
        p.filter_text = "Arch".into();
        p.selected_index = 2;
        p.close();
        assert!(!p.visible);
        assert!(p.filter_text.is_empty());
        assert_eq!(p.selected_index, 0);
        assert!(p.folder_list.is_empty());
    }

    #[test]
    fn filter_narrows_visible_folders() {
        let store = store_with_folders();
        let (theme, config) = (VulthorTheme, Config::default());
        let ctx = ctx(&theme, &config, &store);
        let mut p = FolderPickerComponent::new();
        p.handle_msg(&Msg::OpenFolderPicker, &ctx);
        let total = p.folder_list.len();
        p.filter_text = "arch".into();
        let filtered = p.filtered_indices();
        assert!(filtered.len() < total);
        assert!(filtered.iter().any(|&i| p.folder_list[i].0 == "Archive"));
    }

    #[test]
    fn typing_keys_appends_to_filter() {
        let store = store_with_folders();
        let (theme, config) = (VulthorTheme, Config::default());
        let ctx = ctx(&theme, &config, &store);
        let mut p = FolderPickerComponent::new();
        p.handle_msg(&Msg::OpenFolderPicker, &ctx);

        for c in "arch".chars() {
            let key = KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
            assert!(p.on_key(key, &ctx).is_none());
        }
        assert_eq!(p.filter_text, "arch");
        let filtered = p.filtered_indices();
        assert!(filtered.iter().any(|&i| p.folder_list[i].0 == "Archive"));
    }

    #[test]
    fn backspace_pops_filter() {
        let store = store_with_folders();
        let (theme, config) = (VulthorTheme, Config::default());
        let ctx = ctx(&theme, &config, &store);
        let mut p = FolderPickerComponent::new();
        p.handle_msg(&Msg::OpenFolderPicker, &ctx);
        p.filter_text = "abc".into();

        let bksp = KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE);
        p.on_key(bksp, &ctx);
        assert_eq!(p.filter_text, "ab");
    }

    #[test]
    fn enter_emits_move_to_with_selected_path() {
        let store = store_with_folders();
        let (theme, config) = (VulthorTheme, Config::default());
        let ctx = ctx(&theme, &config, &store);
        let mut p = FolderPickerComponent::new();
        p.handle_msg(&Msg::OpenFolderPicker, &ctx);
        p.filter_text = "Archive".into();
        p.selected_index = 0;

        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let msg = p.on_key(enter, &ctx);
        match msg {
            Some(Msg::MoveTo(_, ref path)) => {
                assert_eq!(path, &PathBuf::from("/tmp/Archive"));
            }
            other => panic!("expected MoveTo, got {:?}", other),
        }
        assert!(!p.visible, "Enter must close the modal");
    }

    #[test]
    fn esc_cancels_without_emitting_message() {
        let store = store_with_folders();
        let (theme, config) = (VulthorTheme, Config::default());
        let ctx = ctx(&theme, &config, &store);
        let mut p = FolderPickerComponent::new();
        p.handle_msg(&Msg::OpenFolderPicker, &ctx);

        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        let msg = p.on_key(esc, &ctx);
        assert!(msg.is_none());
        assert!(!p.visible);
    }

    #[test]
    fn arrow_keys_navigate_filtered_list() {
        let store = store_with_folders();
        let (theme, config) = (VulthorTheme, Config::default());
        let ctx = ctx(&theme, &config, &store);
        let mut p = FolderPickerComponent::new();
        p.handle_msg(&Msg::OpenFolderPicker, &ctx);
        let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
        p.on_key(down, &ctx);
        assert_eq!(p.selected_index, 1);
        let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
        p.on_key(up, &ctx);
        assert_eq!(p.selected_index, 0);
        // Up at top clamps.
        p.on_key(up, &ctx);
        assert_eq!(p.selected_index, 0);
    }

    #[test]
    fn down_at_tail_clamps() {
        let store = store_with_folders();
        let (theme, config) = (VulthorTheme, Config::default());
        let ctx = ctx(&theme, &config, &store);
        let mut p = FolderPickerComponent::new();
        p.handle_msg(&Msg::OpenFolderPicker, &ctx);
        let n = p.folder_list.len();
        p.selected_index = n - 1;
        let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
        p.on_key(down, &ctx);
        assert_eq!(p.selected_index, n - 1);
    }

    #[test]
    fn on_key_returns_none_when_invisible() {
        let store = store_with_folders();
        let (theme, config) = (VulthorTheme, Config::default());
        let ctx = ctx(&theme, &config, &store);
        let mut p = FolderPickerComponent::new();
        // Not visible: every key falls through.
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert!(p.on_key(enter, &ctx).is_none());
        let j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
        assert!(p.on_key(j, &ctx).is_none());
    }
}
