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
pub enum View {
    FolderMessages,      // [*Folder, Messages]
    MessagesContent,     // [*Messages, Content]
    Content,             // [*Content]
    Messages,            // [*Messages] (when content hidden)
    MessagesAttachments, // [Messages, *Attachments] (when content hidden)
}

impl View {
    /// Get the available panes for this view
    pub fn get_available_panes(&self, content_hidden: bool) -> Vec<ActivePane> {
        if content_hidden {
            match self {
                View::FolderMessages => vec![ActivePane::Folders, ActivePane::List],
                View::Messages => vec![ActivePane::List],
                View::MessagesAttachments => vec![ActivePane::List, ActivePane::Attachments],
                _ => vec![ActivePane::List], // Fallback for invalid states
            }
        } else {
            match self {
                View::FolderMessages => vec![ActivePane::Folders, ActivePane::List],
                View::MessagesContent => vec![ActivePane::List, ActivePane::Content],
                View::Content => vec![ActivePane::Content],
                _ => vec![ActivePane::List], // Fallback for invalid states
            }
        }
    }

    /// Get the default active pane for this view
    pub fn get_default_active_pane(&self, content_hidden: bool) -> ActivePane {
        if content_hidden {
            match self {
                View::FolderMessages => ActivePane::Folders,
                View::Messages => ActivePane::List,
                View::MessagesAttachments => ActivePane::Attachments,
                _ => ActivePane::List,
            }
        } else {
            match self {
                View::FolderMessages => ActivePane::Folders,
                View::MessagesContent => ActivePane::List,
                View::Content => ActivePane::Content,
                _ => ActivePane::List,
            }
        }
    }

    /// Get the next view when pressing 'l' (right)
    pub fn next_view(&self, content_hidden: bool) -> Option<View> {
        if content_hidden {
            match self {
                View::FolderMessages => Some(View::Messages),
                View::Messages => Some(View::MessagesAttachments),
                View::MessagesAttachments => None, // No wraparound
                _ => None,
            }
        } else {
            match self {
                View::FolderMessages => Some(View::MessagesContent),
                View::MessagesContent => Some(View::Content),
                View::Content => None, // No wraparound
                _ => None,
            }
        }
    }

    /// Get the previous view when pressing 'h' (left)
    pub fn prev_view(&self, content_hidden: bool) -> Option<View> {
        if content_hidden {
            match self {
                View::MessagesAttachments => Some(View::Messages),
                View::Messages => Some(View::FolderMessages),
                View::FolderMessages => None, // No wraparound
                _ => None,
            }
        } else {
            match self {
                View::Content => Some(View::MessagesContent),
                View::MessagesContent => Some(View::FolderMessages),
                View::FolderMessages => None, // No wraparound
                _ => None,
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ActivePane {
    Folders,     // Left pane
    List,        // Center pane (messages)
    Content,     // Right pane
    Attachments, // Attachment pane
}

#[derive(Debug, Default)]
pub struct SelectionState {
    pub folder_index: usize,     // Selected folder index in current view
    pub email_index: usize,      // Selected email index in current folder
    pub scroll_offset: usize,    // Scroll position for current pane
    pub attachment_index: usize, // Selected attachment when in attachment view
    pub remembered_email_index: Option<usize>, // Remembered email selection when switching views
}

#[derive(Debug)]
pub struct App {
    pub state: AppState,
    pub active_pane: ActivePane,
    pub current_view: View,
    pub content_pane_hidden: bool,
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
            current_view: View::FolderMessages,
            content_pane_hidden: false,
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
                let available_panes = self.current_view.get_available_panes(self.content_pane_hidden);
                if available_panes.contains(&ActivePane::List) {
                    self.active_pane = ActivePane::List;
                }
            }
            AppState::EmailContent => {
                // When viewing email content, switch to content pane if visible
                let available_panes = self.current_view.get_available_panes(self.content_pane_hidden);
                if available_panes.contains(&ActivePane::Content) {
                    self.active_pane = ActivePane::Content;
                }
            }
            _ => {}
        }
        self.state = new_state;
    }

    /// Navigate between panes using Tab
    pub fn switch_pane(&mut self, direction: PaneSwitchDirection) {
        let available_panes = self.current_view.get_available_panes(self.content_pane_hidden);
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

        let old_pane = self.active_pane.clone();
        let new_pane = available_panes[new_index].clone();

        // Handle selection memory when switching between Folders and List panes
        match (&old_pane, &new_pane) {
            (ActivePane::List, ActivePane::Folders) => {
                // Moving from List to Folders - remember current email selection
                if self.email_store.selected_email.is_some() {
                    self.selection.remembered_email_index = Some(self.selection.email_index);
                }
                // Deselect the email to show welcome screen
                self.email_store.selected_email = None;
            }
            (ActivePane::Folders, ActivePane::List) => {
                // Moving from Folders to List - restore remembered selection
                if let Some(remembered_index) = self.selection.remembered_email_index {
                    self.selection.email_index = remembered_index;
                    self.email_store.select_email(remembered_index);

                    // Reload the email content
                    if let Some(_email) = self.email_store.get_selected_email() {
                        // Email content will be loaded automatically by get_selected_email
                        self.set_state(AppState::EmailContent);
                    }
                }
            }
            _ => {}
        }

        self.active_pane = new_pane;
    }

    /// Navigate to next view (l key)
    pub fn next_view(&mut self) {
        if let Some(new_view) = self.current_view.next_view(self.content_pane_hidden) {
            // Handle memory logic when transitioning between views
            match (&self.current_view, &new_view) {
                (View::FolderMessages, View::MessagesContent) => {
                    // Moving from folder view to messages view - restore remembered selection
                    if let Some(remembered_index) = self.selection.remembered_email_index {
                        self.selection.email_index = remembered_index;
                        self.email_store.select_email(remembered_index);

                        // Reload the email content
                        if let Some(_email) = self.email_store.get_selected_email() {
                            self.set_state(AppState::EmailContent);
                        }
                    }
                }
                _ => {}
            }
            
            self.current_view = new_view;
            self.active_pane = self.current_view.get_default_active_pane(self.content_pane_hidden);
        }
    }

    /// Navigate to previous view (h key) 
    pub fn prev_view(&mut self) {
        if let Some(new_view) = self.current_view.prev_view(self.content_pane_hidden) {
            // Handle memory logic when transitioning between views
            match (&self.current_view, &new_view) {
                (View::MessagesContent, View::FolderMessages) => {
                    // Moving from messages view to folder view - remember current selection
                    if self.email_store.selected_email.is_some() {
                        self.selection.remembered_email_index = Some(self.selection.email_index);
                    }
                    // Deselect the email to show welcome screen
                    self.email_store.selected_email = None;
                }
                _ => {}
            }
            
            self.current_view = new_view;
            self.active_pane = self.current_view.get_default_active_pane(self.content_pane_hidden);
        }
    }

    /// Toggle content pane visibility (M-c key)
    pub fn toggle_content_pane(&mut self) {
        self.content_pane_hidden = !self.content_pane_hidden;
        
        // Adjust current view based on new content visibility
        if self.content_pane_hidden {
            // Content is now hidden - switch to appropriate hidden-content view
            match self.current_view {
                View::MessagesContent => {
                    self.current_view = View::Messages;
                }
                View::Content => {
                    self.current_view = View::Messages;
                }
                View::FolderMessages => {
                    // Already compatible with hidden content
                }
                _ => {
                    // For other views, default to Messages
                    self.current_view = View::Messages;
                }
            }
        } else {
            // Content is now shown - switch to appropriate full-content view
            match self.current_view {
                View::Messages => {
                    self.current_view = View::MessagesContent;
                }
                View::MessagesAttachments => {
                    self.current_view = View::MessagesContent;
                }
                View::FolderMessages => {
                    // Already compatible with shown content
                }
                _ => {
                    // For other views, default to MessagesContent
                    self.current_view = View::MessagesContent;
                }
            }
        }
        
        // Reset to appropriate pane for the new view
        self.active_pane = self.current_view.get_default_active_pane(self.content_pane_hidden);
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
    /// Returns None when in folder/message view to show welcome screen
    pub fn get_current_email_for_web(&mut self) -> Option<&crate::email::Email> {
        // Only show email content when in views that include content
        match self.current_view {
            View::MessagesContent | View::Content => self.email_store.get_selected_email(),
            _ => None, // Show welcome screen in other views
        }
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

            // Reset email selection and remembered index since we're browsing folders
            self.selection.email_index = 0;
            self.selection.remembered_email_index = None;
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
