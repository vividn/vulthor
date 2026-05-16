// `FoldersComponent` — first pane migration (Phase 0.2.2b, vu-sd6).
//
// Owns the folder-pane selection (`folder_index`) and the ratatui
// `ListState` used to highlight it. Renders the folder tree from
// `Ctx::store.root_folder`. Translates Folders-pane keys into messages.
//
// **Sole writer of `folder_index`.** AppRoot mirrors this value into
// `app.selection.folder_index` after each dispatch so the legacy
// readers in `ui.rs` and `input.rs` keep working until the Messages
// pane is also extracted (vu-3yj). `Backspace` is the one remaining
// legacy path that still writes through `App`; AppRoot syncs the
// other direction after fall-through. That seam disappears when
// back-navigation moves into a `Msg` variant.
//
// **`RefCell<ListState>`** — ratatui's `render_stateful_widget`
// requires `&mut ListState`, but `Component::render` takes `&self`.
// `RefCell` is the documented workaround in DESIGN-COMPONENTS.md
// § "Risks & open questions". It costs a borrow-check at render time
// and nothing else.

use std::cell::RefCell;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::email::Folder;
use crate::theme::VulthorTheme;

use super::{Component, Ctx, Dir, Msg};

pub struct FoldersComponent {
    pub folder_index: usize,
    list_state: RefCell<ListState>,
}

impl FoldersComponent {
    /// Build a component whose selection points at `folder_index`. Used by
    /// tests and `AppRoot::new` after computing the auto-INBOX index.
    pub fn with_index(folder_index: usize) -> Self {
        let mut state = ListState::default();
        state.select(Some(folder_index));
        Self {
            folder_index,
            list_state: RefCell::new(state),
        }
    }

    /// Find the display index of the top-level folder named "INBOX"
    /// (case-insensitive), matching the sort order used in `render`.
    /// Returns 0 when no inbox exists — same fallback as the old
    /// `App::find_inbox_folder`.
    pub fn auto_select_inbox(root: &Folder) -> usize {
        for (i, sub) in root.get_sorted_subfolders().iter().enumerate() {
            if sub.get_display_name().eq_ignore_ascii_case("inbox") {
                return i;
            }
        }
        0
    }

    fn build_folder_list(folder: &Folder, depth: usize) -> Vec<ListItem<'static>> {
        let mut items = Vec::new();
        if depth > 0 {
            let indent = "  ".repeat(depth - 1);
            let display_name = folder.get_display_name();
            items.push(ListItem::new(format!("{}{}", indent, display_name)));
        }
        for subfolder in folder.get_sorted_subfolders() {
            items.extend(Self::build_folder_list(subfolder, depth + 1));
        }
        items
    }
}

/// Count every folder rendered in the pane (root excluded). Same shape as
/// `input::count_visible_folders` — kept private here to make the bound
/// check self-contained inside the component.
fn count_visible_folders(folder: &Folder) -> usize {
    let mut count = 0;
    for sub in &folder.subfolders {
        count += 1 + count_visible_folders(sub);
    }
    count
}

impl Component for FoldersComponent {
    fn handle_msg(&mut self, msg: &Msg, ctx: &Ctx) -> Vec<Msg> {
        match msg {
            Msg::FolderMove(Dir::Down) => {
                let total = count_visible_folders(&ctx.store.root_folder);
                if self.folder_index + 1 < total {
                    self.folder_index += 1;
                }
            }
            Msg::FolderMove(Dir::Up) => {
                if self.folder_index > 0 {
                    self.folder_index -= 1;
                }
            }
            Msg::FolderExitParent => {
                // Back-navigation collapses the selection to the top of
                // the (now parent) folder pane — matches the legacy
                // `handle_back_navigation` behavior and resolves the
                // vu-sd6 Backspace observation by writing through the
                // component instead of `app.selection.folder_index`.
                self.folder_index = 0;
            }
            _ => {}
        }
        Vec::new()
    }

    fn render(&self, f: &mut Frame, area: Rect, focused: bool, ctx: &Ctx) {
        let style = if focused {
            Style::default().fg(VulthorTheme::ACCENT)
        } else {
            Style::default()
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .style(style)
            .title("Folders");

        // Phase 0.3.4 (vu-w9i): the initial folder-structure scan now
        // runs off-thread. Until it lands, the store carries no folders;
        // render a splash instead of an empty list so the user sees that
        // launch is making progress rather than that their maildir is
        // empty.
        if ctx.store.scanning_folders {
            let splash = Paragraph::new(vec![Line::from(Span::styled(
                "Scanning folders…",
                Style::default().add_modifier(Modifier::ITALIC),
            ))])
            .block(block)
            .style(style)
            .wrap(Wrap { trim: true });
            f.render_widget(splash, area);
            return;
        }

        let folder_items = Self::build_folder_list(&ctx.store.root_folder, 0);
        let list = List::new(folder_items)
            .block(block)
            .style(style)
            .highlight_style(
                Style::default()
                    .bg(VulthorTheme::SELECTION_BG)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            );

        let mut state = self.list_state.borrow_mut();
        state.select(Some(self.folder_index));
        f.render_stateful_widget(list, area, &mut *state);
    }

    fn on_key(&mut self, key: KeyEvent, ctx: &Ctx) -> Option<Msg> {
        if !key.modifiers.is_empty() && !matches!(key.code, KeyCode::Up | KeyCode::Down) {
            // Only plain keys (and arrow keys, which carry no modifiers
            // we care about) belong to the Folders pane. Anything with a
            // modifier is either global (Alt+c, Shift+Tab) or unhandled.
            return None;
        }
        match key.code {
            KeyCode::Char('j') => Some(Msg::FolderMove(Dir::Down)),
            KeyCode::Char('k') => Some(Msg::FolderMove(Dir::Up)),
            KeyCode::Down => Some(Msg::FolderMove(Dir::Down)),
            KeyCode::Up => Some(Msg::FolderMove(Dir::Up)),
            KeyCode::Enter => Some(Msg::FolderEnter),
            KeyCode::Char('l') => {
                // 'l' from the Folders pane is overloaded: if the user is
                // *already* inside the selected folder, 'l' advances the
                // view; otherwise it enters the folder. This preserves
                // the pre-refactor UX (see `input.rs` legacy branch).
                let path = crate::layout::get_folder_path_from_display_index(
                    &ctx.store.root_folder,
                    self.folder_index,
                );
                match path {
                    Some(p) if p == ctx.store.current_folder => Some(Msg::ViewNext),
                    _ => Some(Msg::FolderEnter),
                }
            }
            KeyCode::Backspace => Some(Msg::FolderExitParent),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::email::{Email, EmailStore};
    use crossterm::event::KeyModifiers;
    use std::path::PathBuf;

    fn store_with_folders(names: &[&str]) -> EmailStore {
        let mut store = EmailStore::new(PathBuf::from("/tmp"));
        for name in names {
            let mut folder = Folder::new(name.to_string(), PathBuf::from(format!("/tmp/{}", name)));
            folder.add_email(Email::new(PathBuf::from(format!("/tmp/{}/m1", name))));
            folder.is_loaded = true;
            store.root_folder.add_subfolder(folder);
        }
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
    fn auto_select_finds_inbox_in_sorted_order() {
        // Inserted in non-alpha order; sort order should still surface INBOX.
        let store = store_with_folders(&["Sent", "Drafts", "INBOX", "Archive"]);
        let inbox_index = FoldersComponent::auto_select_inbox(&store.root_folder);
        let sorted = store.root_folder.get_sorted_subfolders();
        assert_eq!(sorted[inbox_index].get_display_name(), "INBOX");
    }

    #[test]
    fn auto_select_defaults_to_zero_when_no_inbox() {
        let store = store_with_folders(&["Sent", "Drafts"]);
        assert_eq!(FoldersComponent::auto_select_inbox(&store.root_folder), 0);
    }

    #[test]
    fn folder_move_down_advances_and_clamps_at_end() {
        let store = store_with_folders(&["A", "B", "C"]);
        let (theme, config) = (VulthorTheme, Config::default());
        let ctx = ctx(&theme, &config, &store);

        let mut comp = FoldersComponent::with_index(0);
        assert!(
            comp.handle_msg(&Msg::FolderMove(Dir::Down), &ctx)
                .is_empty()
        );
        assert_eq!(comp.folder_index, 1);
        comp.handle_msg(&Msg::FolderMove(Dir::Down), &ctx);
        assert_eq!(comp.folder_index, 2);
        // At the last folder — further Down is a no-op (clamp, not wrap).
        comp.handle_msg(&Msg::FolderMove(Dir::Down), &ctx);
        assert_eq!(comp.folder_index, 2);
    }

    #[test]
    fn folder_move_up_clamps_at_zero() {
        let store = store_with_folders(&["A", "B"]);
        let (theme, config) = (VulthorTheme, Config::default());
        let ctx = ctx(&theme, &config, &store);

        let mut comp = FoldersComponent::with_index(0);
        // At the top — Up is a no-op (does not wrap to last).
        comp.handle_msg(&Msg::FolderMove(Dir::Up), &ctx);
        assert_eq!(comp.folder_index, 0);
    }

    #[test]
    fn on_key_maps_jk_to_folder_move() {
        let store = store_with_folders(&["A"]);
        let (theme, config) = (VulthorTheme, Config::default());
        let ctx = ctx(&theme, &config, &store);
        let mut comp = FoldersComponent::with_index(0);

        let j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(comp.on_key(j, &ctx), Some(Msg::FolderMove(Dir::Down)));

        let k = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(comp.on_key(k, &ctx), Some(Msg::FolderMove(Dir::Up)));
    }

    #[test]
    fn on_key_enter_emits_folder_enter() {
        let store = store_with_folders(&["A"]);
        let (theme, config) = (VulthorTheme, Config::default());
        let ctx = ctx(&theme, &config, &store);
        let mut comp = FoldersComponent::with_index(0);

        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(comp.on_key(enter, &ctx), Some(Msg::FolderEnter));
    }

    #[test]
    fn on_key_l_enters_folder_when_not_already_inside() {
        let store = store_with_folders(&["A", "B"]);
        let (theme, config) = (VulthorTheme, Config::default());
        let ctx = ctx(&theme, &config, &store);
        // current_folder is empty (root) — 'l' must request entering.
        let mut comp = FoldersComponent::with_index(1);
        let l = KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE);
        assert_eq!(comp.on_key(l, &ctx), Some(Msg::FolderEnter));
    }

    #[test]
    fn on_key_l_advances_view_when_already_inside_selected_folder() {
        let mut store = store_with_folders(&["A", "B"]);
        // Pretend we're already inside the second folder.
        store.current_folder = vec![1];
        let (theme, config) = (VulthorTheme, Config::default());
        let ctx = ctx(&theme, &config, &store);
        let mut comp = FoldersComponent::with_index(1);
        let l = KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE);
        assert_eq!(comp.on_key(l, &ctx), Some(Msg::ViewNext));
    }

    #[test]
    fn on_key_ignores_modified_keys() {
        let store = store_with_folders(&["A"]);
        let (theme, config) = (VulthorTheme, Config::default());
        let ctx = ctx(&theme, &config, &store);
        let mut comp = FoldersComponent::with_index(0);
        let alt_j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::ALT);
        assert_eq!(comp.on_key(alt_j, &ctx), None);
    }
}
