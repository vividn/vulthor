use crate::app::{App, AppState, ActivePane, PaneSwitchDirection, ScrollDirection};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

pub fn handle_input(app: &mut App, event: Event) -> bool {
    match event {
        Event::Key(key_event) => handle_key_event(app, key_event),
        Event::Resize(_, _) => {
            // Terminal was resized, no action needed as ratatui handles this
            false
        }
        _ => false,
    }
}

fn handle_key_event(app: &mut App, key: KeyEvent) -> bool {
    // Global keybindings that work in any state
    match key.code {
        KeyCode::Char('q') if key.modifiers.is_empty() => {
            app.set_state(AppState::Quit);
            return true;
        }
        KeyCode::Char('?') if key.modifiers.is_empty() => {
            if matches!(app.state, AppState::Help) {
                app.set_state(AppState::FolderView);
            } else {
                app.set_state(AppState::Help);
            }
            return false;
        }
        _ => {}
    }

    // State-specific keybindings
    match app.state {
        AppState::Help => {
            // Any key exits help
            app.set_state(AppState::FolderView);
            false
        }
        AppState::AttachmentView => handle_attachment_view_input(app, key),
        _ => handle_main_view_input(app, key),
    }
}

fn handle_main_view_input(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        // Navigation
        KeyCode::Char('j') if key.modifiers.is_empty() => {
            handle_navigation(app, NavigationDirection::Down);
            false
        }
        KeyCode::Char('k') if key.modifiers.is_empty() => {
            handle_navigation(app, NavigationDirection::Up);
            false
        }
        KeyCode::Down => {
            handle_navigation(app, NavigationDirection::Down);
            false
        }
        KeyCode::Up => {
            handle_navigation(app, NavigationDirection::Up);
            false
        }

        // Pane switching
        KeyCode::Char('h') if key.modifiers == KeyModifiers::ALT => {
            app.switch_pane(PaneSwitchDirection::Left);
            false
        }
        KeyCode::Char('l') if key.modifiers == KeyModifiers::ALT => {
            app.switch_pane(PaneSwitchDirection::Right);
            false
        }

        // Pane visibility
        KeyCode::Char('e') if key.modifiers == KeyModifiers::ALT => {
            app.pane_visibility.toggle_folders();
            // Ensure we're not on an invisible pane
            if !app.pane_visibility.folders_visible && matches!(app.active_pane, ActivePane::Folders) {
                app.active_pane = ActivePane::List;
            }
            false
        }
        KeyCode::Char('c') if key.modifiers == KeyModifiers::ALT => {
            app.pane_visibility.toggle_content();
            // Ensure we're not on an invisible pane
            if !app.pane_visibility.content_visible && matches!(app.active_pane, ActivePane::Content) {
                app.active_pane = ActivePane::List;
            }
            false
        }

        // Selection and navigation
        KeyCode::Enter => {
            handle_selection(app);
            false
        }
        KeyCode::Backspace => {
            handle_back_navigation(app);
            false
        }

        // Attachments
        KeyCode::Char('a') if key.modifiers == KeyModifiers::ALT => {
            if let Some(email) = app.email_store.get_selected_email() {
                if email.has_attachments() {
                    app.set_state(AppState::AttachmentView);
                } else {
                    app.set_status("No attachments in this email".to_string());
                }
            } else {
                app.set_status("No email selected".to_string());
            }
            false
        }

        // Scrolling in content pane
        KeyCode::PageDown if matches!(app.active_pane, ActivePane::Content) => {
            app.scroll(ScrollDirection::Down, 10);
            false
        }
        KeyCode::PageUp if matches!(app.active_pane, ActivePane::Content) => {
            app.scroll(ScrollDirection::Up, 10);
            false
        }

        _ => false,
    }
}

fn handle_attachment_view_input(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Esc => {
            app.set_state(AppState::EmailContent);
            false
        }
        KeyCode::Char('j') | KeyCode::Down => {
            if let Some(email) = app.email_store.get_selected_email() {
                if app.selection.attachment_index + 1 < email.attachments.len() {
                    app.selection.attachment_index += 1;
                }
            }
            false
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if app.selection.attachment_index > 0 {
                app.selection.attachment_index -= 1;
            }
            false
        }
        KeyCode::Enter if key.modifiers == KeyModifiers::SHIFT => {
            handle_attachment_open(app, true);
            false
        }
        KeyCode::Enter => {
            handle_attachment_open(app, false);
            false
        }
        _ => false,
    }
}

fn handle_navigation(app: &mut App, direction: NavigationDirection) {
    match app.active_pane {
        ActivePane::Folders => {
            let current_folder = app.email_store.get_current_folder();
            let total_folders = count_visible_folders(current_folder);
            
            match direction {
                NavigationDirection::Down => {
                    if app.selection.folder_index + 1 < total_folders {
                        app.selection.folder_index += 1;
                    }
                }
                NavigationDirection::Up => {
                    if app.selection.folder_index > 0 {
                        app.selection.folder_index -= 1;
                    }
                }
            }
        }
        ActivePane::List => {
            let current_folder = app.email_store.get_current_folder();
            let total_emails = current_folder.emails.len();
            
            match direction {
                NavigationDirection::Down => {
                    if app.selection.email_index + 1 < total_emails {
                        app.selection.email_index += 1;
                        app.email_store.select_email(app.selection.email_index);
                        app.set_state(AppState::EmailList);
                    }
                }
                NavigationDirection::Up => {
                    if app.selection.email_index > 0 {
                        app.selection.email_index -= 1;
                        app.email_store.select_email(app.selection.email_index);
                        app.set_state(AppState::EmailList);
                    }
                }
            }
        }
        ActivePane::Content => {
            // Scroll content
            match direction {
                NavigationDirection::Down => {
                    app.scroll(ScrollDirection::Down, 1);
                }
                NavigationDirection::Up => {
                    app.scroll(ScrollDirection::Up, 1);
                }
            }
        }
    }
}

fn handle_selection(app: &mut App) {
    match app.active_pane {
        ActivePane::Folders => {
            // Navigate into selected folder
            let current_folder = app.email_store.get_current_folder();
            let folder_index = get_real_folder_index(current_folder, app.selection.folder_index);
            
            if let Some(real_index) = folder_index {
                app.email_store.enter_folder(real_index);
                app.selection.folder_index = 0;
                app.selection.email_index = 0;
                app.selection.scroll_offset = 0;
                app.set_state(AppState::EmailList);
            }
        }
        ActivePane::List => {
            // Select email and switch to content view
            let current_folder = app.email_store.get_current_folder();
            if app.selection.email_index < current_folder.emails.len() {
                app.email_store.select_email(app.selection.email_index);
                app.set_state(AppState::EmailContent);
            }
        }
        ActivePane::Content => {
            // No action for content pane selection
        }
    }
}

fn handle_back_navigation(app: &mut App) {
    match app.active_pane {
        ActivePane::Folders | ActivePane::List => {
            // Go back to parent folder
            app.email_store.exit_folder();
            app.selection.folder_index = 0;
            app.selection.email_index = 0;
            app.selection.scroll_offset = 0;
            app.set_state(AppState::FolderView);
        }
        ActivePane::Content => {
            // Switch back to email list
            app.set_state(AppState::EmailList);
        }
    }
}

fn handle_attachment_open(app: &mut App, custom_command: bool) {
    if let Some(email) = app.email_store.get_selected_email() {
        if app.selection.attachment_index < email.attachments.len() {
            let attachment = &email.attachments[app.selection.attachment_index];
            
            if custom_command {
                app.set_status(format!("Custom command for {}: Not implemented yet", attachment.filename));
            } else {
                app.set_status(format!("Opening {}: Not implemented yet", attachment.filename));
            }
            
            // TODO: Implement actual file opening with xdg-open or custom command
            // For now, just show a status message
        }
    }
}

// Helper functions

#[derive(Debug, Clone)]
enum NavigationDirection {
    Up,
    Down,
}

fn count_visible_folders(folder: &crate::email::Folder) -> usize {
    let mut count = 0;
    
    // Don't count root folder
    for subfolder in &folder.subfolders {
        count += 1 + count_visible_folders_recursive(subfolder);
    }
    
    count
}

fn count_visible_folders_recursive(folder: &crate::email::Folder) -> usize {
    let mut count = 0;
    
    for subfolder in &folder.subfolders {
        count += 1 + count_visible_folders_recursive(subfolder);
    }
    
    count
}

fn get_real_folder_index(folder: &crate::email::Folder, display_index: usize) -> Option<usize> {
    // Convert display index to actual subfolder index
    // This is a simplified version - in a real implementation you'd need to 
    // track the mapping between display order and actual folder indices
    if display_index < folder.subfolders.len() {
        Some(display_index)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::email::{Email, EmailStore, Folder};
    use std::path::PathBuf;

    #[test]
    fn test_handle_quit_key() {
        let email_store = EmailStore::new(PathBuf::from("/tmp"));
        let mut app = App::new(email_store);
        
        let key_event = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        let should_quit = handle_key_event(&mut app, key_event);
        
        assert!(should_quit);
        assert!(matches!(app.state, AppState::Quit));
    }

    #[test]
    fn test_handle_help_key() {
        let email_store = EmailStore::new(PathBuf::from("/tmp"));
        let mut app = App::new(email_store);
        
        let key_event = KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE);
        let should_quit = handle_key_event(&mut app, key_event);
        
        assert!(!should_quit);
        assert!(matches!(app.state, AppState::Help));
    }

    #[test]
    fn test_pane_visibility_toggle() {
        let email_store = EmailStore::new(PathBuf::from("/tmp"));
        let mut app = App::new(email_store);
        
        let initial_folders_visible = app.pane_visibility.folders_visible;
        
        let key_event = KeyEvent::new(KeyCode::Char('e'), KeyModifiers::ALT);
        handle_key_event(&mut app, key_event);
        
        assert_eq!(app.pane_visibility.folders_visible, !initial_folders_visible);
    }
}