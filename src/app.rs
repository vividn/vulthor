use crate::email::EmailStore;
use crate::maildir::MaildirScanner;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq)]
pub enum AppState {
    FolderView,     // Navigating folders
    EmailList,      // Viewing emails in current folder
    EmailContent,   // Reading an email
    AttachmentView, // Viewing attachments popup
    Help,           // Help screen
    Quit,           // Application should quit
}

#[derive(Debug, Clone, PartialEq)]
pub enum ViewMode {
    FolderMessage,  // Show folders and messages (2-panel view)
    MessageContent, // Show messages and content (2-panel view)
}

#[derive(Debug, Clone, PartialEq)]
pub enum ActivePane {
    Folders, // Left pane
    List,    // Center pane
    Content, // Right pane
}

#[derive(Debug)]
pub struct PaneVisibility {
    pub folders_visible: bool,
    pub content_visible: bool,
    pub view_mode: ViewMode,
}

impl Default for PaneVisibility {
    fn default() -> Self {
        Self {
            folders_visible: true,
            content_visible: false,
            view_mode: ViewMode::FolderMessage,
        }
    }
}

impl PaneVisibility {

    /// Get available panes based on view mode
    pub fn get_available_panes(&self) -> Vec<ActivePane> {
        match self.view_mode {
            ViewMode::FolderMessage => vec![ActivePane::Folders, ActivePane::List],
            ViewMode::MessageContent => vec![ActivePane::List, ActivePane::Content],
        }
    }

    /// Switch to folder/message view mode
    pub fn set_folder_message_mode(&mut self) {
        self.view_mode = ViewMode::FolderMessage;
        self.folders_visible = true;
        self.content_visible = false;
    }

    /// Switch to message/content view mode
    pub fn set_message_content_mode(&mut self) {
        self.view_mode = ViewMode::MessageContent;
        self.folders_visible = false;
        self.content_visible = true;
    }
}

#[derive(Debug, Default)]
pub struct SelectionState {
    pub folder_index: usize,     // Selected folder index in current view
    pub email_index: usize,      // Selected email index in current folder
    pub scroll_offset: usize,    // Scroll position for current pane
    pub attachment_index: usize, // Selected attachment when in attachment view
}

#[derive(Debug)]
pub struct App {
    pub state: AppState,
    pub active_pane: ActivePane,
    pub pane_visibility: PaneVisibility,
    pub selection: SelectionState,
    pub email_store: EmailStore,
    pub scanner: MaildirScanner,
    pub should_quit: bool,
    pub status_message: Option<String>,
    pub message_pane_visible_rows: usize, // Track visible rows in message pane for loading
    pub initial_loading_done: bool,       // Track if initial email loading has been performed
}

impl App {
    pub fn new(email_store: EmailStore, scanner: MaildirScanner) -> Self {
        let mut app = Self {
            state: AppState::FolderView,
            active_pane: ActivePane::Folders,
            pane_visibility: PaneVisibility::default(),
            selection: SelectionState::default(),
            email_store,
            scanner,
            should_quit: false,
            status_message: None,
            message_pane_visible_rows: 20, // Default estimate
            initial_loading_done: false,
        };

        // Auto-select INBOX folder on startup but defer email loading
        app.auto_select_inbox_without_loading();

        app
    }

    /// Handle state transitions
    pub fn set_state(&mut self, new_state: AppState) {
        match new_state {
            AppState::Quit => {
                self.should_quit = true;
            }
            AppState::EmailList => {
                // When entering email list, ensure we're in the list pane
                if self
                    .pane_visibility
                    .get_available_panes()
                    .contains(&ActivePane::List)
                {
                    self.active_pane = ActivePane::List;
                }
            }
            AppState::EmailContent => {
                // When viewing email content, switch to content pane if visible
                if self.pane_visibility.content_visible {
                    self.active_pane = ActivePane::Content;
                }
            }
            _ => {}
        }
        self.state = new_state;
    }

    /// Navigate between panes
    pub fn switch_pane(&mut self, direction: PaneSwitchDirection) {
        let available_panes = self.pane_visibility.get_available_panes();
        if available_panes.is_empty() {
            return;
        }

        let current_index = available_panes
            .iter()
            .position(|pane| *pane == self.active_pane)
            .unwrap_or(0);

        let new_index = match direction {
            PaneSwitchDirection::Left => {
                if current_index > 0 {
                    current_index - 1
                } else {
                    available_panes.len() - 1
                }
            }
            PaneSwitchDirection::Right => {
                if current_index < available_panes.len() - 1 {
                    current_index + 1
                } else {
                    0
                }
            }
        };

        self.active_pane = available_panes[new_index].clone();
    }

    /// Set status message
    pub fn set_status(&mut self, message: String) {
        self.status_message = Some(message);
    }

    /// Clear status message
    pub fn clear_status(&mut self) {
        self.status_message = None;
    }

    /// Handle scrolling in current pane
    pub fn scroll(&mut self, direction: ScrollDirection, amount: usize) {
        match direction {
            ScrollDirection::Up => {
                if self.selection.scroll_offset >= amount {
                    self.selection.scroll_offset -= amount;
                } else {
                    self.selection.scroll_offset = 0;
                }
            }
            ScrollDirection::Down => {
                self.selection.scroll_offset += amount;
                // TODO: Add bounds checking based on content length
            }
        }
    }

    /// Get the currently selected email for web serving
    pub fn get_current_email_for_web(&mut self) -> Option<&crate::email::Email> {
        self.email_store.get_selected_email()
    }



    /// Auto-select INBOX folder on startup without loading messages (deferred until UI is ready)
    fn auto_select_inbox_without_loading(&mut self) {
        // Find INBOX folder in the folder structure and set the selection index
        if let Some(inbox_index) = self.find_inbox_folder() {
            self.selection.folder_index = inbox_index;
            // Don't load messages yet - let UI trigger loading with proper dimensions
        }
    }

    /// Perform initial email loading with actual UI dimensions (called from UI)
    pub fn perform_initial_loading_if_needed(&mut self) {
        if !self.initial_loading_done {
            self.load_selected_folder_messages();
            self.initial_loading_done = true;
        }
    }

    /// Load messages for the currently selected folder (for folder browsing)
    pub fn load_selected_folder_messages(&mut self) {
        let root_folder = &self.email_store.root_folder;
        let folder_path = crate::input::get_folder_path_from_display_index(
            root_folder,
            self.selection.folder_index,
        );

        if let Some(path) = folder_path {
            let visible_rows = self.message_pane_visible_rows;

            if let Err(e) =
                self.email_store
                    .ensure_folder_at_path_loaded(&path, &self.scanner, visible_rows)
            {
                self.set_status(format!("Error loading folder messages: {}", e));
            }

            // Reset email selection since we're browsing folders
            self.selection.email_index = 0;
        }
    }

    /// Find the INBOX folder index
    fn find_inbox_folder(&self) -> Option<usize> {
        // Look for folders named "INBOX" or "Inbox" in the sorted order (matching UI display)
        let root_folder = &self.email_store.root_folder;
        for (index, subfolder) in root_folder.get_sorted_subfolders().iter().enumerate() {
            let name = subfolder.get_display_name();
            if name.eq_ignore_ascii_case("inbox") {
                return Some(index);
            }
        }
        // If no INBOX found, default to first folder if available
        if !root_folder.subfolders.is_empty() {
            Some(0)
        } else {
            None
        }
    }

    /// Switch to folder/message view (h key)
    pub fn switch_to_folder_message_view(&mut self) {
        self.pane_visibility.set_folder_message_mode();
        self.active_pane = ActivePane::Folders;
    }

    /// Switch to message/content view (l key)
    pub fn switch_to_message_content_view(&mut self) {
        self.pane_visibility.set_message_content_mode();
        self.active_pane = ActivePane::List;
    }

    /// Get the currently selected folder in folder view mode
    pub fn get_selected_folder(&self) -> Option<&crate::email::Folder> {
        // Get the folder path for the currently selected folder
        let root_folder = &self.email_store.root_folder;
        let folder_path = crate::input::get_folder_path_from_display_index(
            root_folder,
            self.selection.folder_index,
        );

        if let Some(path) = folder_path {
            self.email_store.get_folder_at_path(&path)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone)]
pub enum PaneSwitchDirection {
    Left,
    Right,
}

#[derive(Debug, Clone)]
pub enum ScrollDirection {
    Up,
    Down,
}

/// Thread-safe wrapper for sharing app state with web server
pub type SharedAppState = Arc<Mutex<App>>;
