use crate::email::EmailStore;
use crate::maildir::MaildirScanner;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq)]
pub enum AppState {
    FolderView,     // Navigating folders
    EmailList,      // Viewing emails in current folder  
    EmailContent,   // Reading an email
    AttachmentView, // Viewing attachments popup
    Help,          // Help screen
    Quit,          // Application should quit
}

#[derive(Debug, Clone, PartialEq)]
pub enum ActivePane {
    Folders,   // Left pane
    List,      // Center pane  
    Content,   // Right pane
}

#[derive(Debug)]
pub struct PaneVisibility {
    pub folders_visible: bool,
    pub content_visible: bool,
}

impl Default for PaneVisibility {
    fn default() -> Self {
        Self {
            folders_visible: true,
            content_visible: true,
        }
    }
}

impl PaneVisibility {
    pub fn toggle_folders(&mut self) {
        self.folders_visible = !self.folders_visible;
    }

    pub fn toggle_content(&mut self) {
        self.content_visible = !self.content_visible;
    }

    /// Get available panes based on visibility
    pub fn get_available_panes(&self) -> Vec<ActivePane> {
        let mut panes = vec![ActivePane::List]; // List pane is always visible
        
        if self.folders_visible {
            panes.insert(0, ActivePane::Folders);
        }
        
        if self.content_visible {
            panes.push(ActivePane::Content);
        }
        
        panes
    }
}

#[derive(Debug)]
pub struct SelectionState {
    pub folder_index: usize,     // Selected folder index in current view
    pub email_index: usize,      // Selected email index in current folder
    pub scroll_offset: usize,    // Scroll position for current pane
    pub attachment_index: usize, // Selected attachment when in attachment view
}

impl Default for SelectionState {
    fn default() -> Self {
        Self {
            folder_index: 0,
            email_index: 0,
            scroll_offset: 0,
            attachment_index: 0,
        }
    }
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
    pub search_query: String,
    pub search_mode: bool,
}

impl App {
    pub fn new(email_store: EmailStore, scanner: MaildirScanner) -> Self {
        Self {
            state: AppState::FolderView,
            active_pane: ActivePane::Folders,
            pane_visibility: PaneVisibility::default(),
            selection: SelectionState::default(),
            email_store,
            scanner,
            should_quit: false,
            status_message: None,
            search_query: String::new(),
            search_mode: false,
        }
    }

    /// Handle state transitions
    pub fn set_state(&mut self, new_state: AppState) {
        match new_state {
            AppState::Quit => {
                self.should_quit = true;
            }
            AppState::EmailList => {
                // When entering email list, ensure we're in the list pane
                if self.pane_visibility.get_available_panes().contains(&ActivePane::List) {
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
    pub fn get_current_email_for_web(&self) -> Option<&crate::email::Email> {
        self.email_store.get_selected_email()
    }

    /// Enter a folder and trigger lazy loading if needed
    pub fn enter_folder_with_loading(&mut self, folder_index: usize) -> Result<(), Box<dyn std::error::Error>> {
        self.email_store.enter_folder(folder_index);
        self.email_store.ensure_current_folder_loaded(&self.scanner)?;
        Ok(())
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