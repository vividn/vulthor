use crate::app::{ActivePane, App, AppState, PaneSwitchDirection, ScrollDirection, View};
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

        // Pane switching with Tab
        KeyCode::Tab => {
            app.switch_pane(PaneSwitchDirection::Right);
            false
        }
        KeyCode::BackTab => {
            app.switch_pane(PaneSwitchDirection::Left);
            false
        }

        // View navigation with h/l keys
        KeyCode::Char('h') if key.modifiers.is_empty() => {
            app.prev_view();
            false
        }
        KeyCode::Char('l') if key.modifiers.is_empty() => {
            // If we're in folder pane and at the root level, enter the selected folder
            // Otherwise, do view navigation
            if matches!(app.active_pane, ActivePane::Folders)
                && app.email_store.current_folder.is_empty()
            {
                handle_folder_selection_and_switch_view(app);
            } else {
                app.next_view();
            }
            false
        }

        // Content pane toggle
        KeyCode::Char('c') if key.modifiers == KeyModifiers::ALT => {
            app.toggle_content_pane();
            false
        }

        // Selection and navigation
        KeyCode::Enter => {
            // If we're in folder pane, navigate into selected folder AND switch view
            if matches!(app.active_pane, ActivePane::Folders) {
                handle_folder_selection_and_switch_view(app);
            } else {
                handle_selection(app);
            }
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
            // For folder navigation, always use the root folder structure (displayed in folder pane)
            let root_folder = &app.email_store.root_folder;
            let total_folders = count_visible_folders(root_folder);

            let old_folder_index = app.selection.folder_index;
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

            // If folder selection changed, automatically load messages from the new folder
            if app.selection.folder_index != old_folder_index {
                app.load_selected_folder_messages();
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

                        // Check if we need to load more messages
                        if let Err(e) = app
                            .email_store
                            .load_more_messages_if_needed(&app.scanner, app.selection.email_index)
                        {
                            app.set_status(format!("Error loading more messages: {}", e));
                        }

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
        ActivePane::Attachments => {
            // Navigate attachments list
            match direction {
                NavigationDirection::Down => {
                    if let Some(email) = app.email_store.get_selected_email() {
                        if app.selection.attachment_index + 1 < email.attachments.len() {
                            app.selection.attachment_index += 1;
                        }
                    }
                }
                NavigationDirection::Up => {
                    if app.selection.attachment_index > 0 {
                        app.selection.attachment_index -= 1;
                    }
                }
            }
        }
    }
}

fn handle_folder_selection_and_switch_view(app: &mut App) {
    // Navigate into selected folder and switch to message/content view
    let root_folder = &app.email_store.root_folder;
    let folder_path = get_folder_path_from_display_index(root_folder, app.selection.folder_index);

    if let Some(path) = folder_path {
        // Reset current folder path to root first
        app.email_store.current_folder.clear();

        // Navigate through the full path to handle subfolders correctly
        app.email_store.enter_folder_by_path(&path);

        // Load emails with visible row limit
        let estimated_visible_rows = 20; // TODO: This should be passed from UI context
        let load_count = (estimated_visible_rows + 5).max(10);

        match app
            .email_store
            .ensure_current_folder_loaded_with_limit(&app.scanner, load_count)
        {
            Ok(()) => {
                app.selection.email_index = 0;
                app.selection.scroll_offset = 0;
                app.selection.remembered_email_index = None; // Clear remembered selection for new folder

                // Select the first email if folder has emails
                let current_folder = app.email_store.get_current_folder();
                if !current_folder.emails.is_empty() {
                    app.email_store.select_email(0);
                }

                // Choose appropriate view based on content visibility
                if app.content_pane_hidden {
                    app.current_view = View::Messages;
                } else {
                    app.current_view = View::MessagesContent;
                }
                app.active_pane = ActivePane::List;
                app.set_state(AppState::EmailList);
            }
            Err(e) => {
                app.set_status(format!("Error loading folder: {}", e));
            }
        }
    }
}

fn handle_selection(app: &mut App) {
    match app.active_pane {
        ActivePane::Folders => {
            // This case should now be handled by handle_folder_selection_and_switch_view
            // But keeping the logic here for backward compatibility
            let root_folder = &app.email_store.root_folder;
            let folder_path =
                get_folder_path_from_display_index(root_folder, app.selection.folder_index);

            if let Some(path) = folder_path {
                // Reset current folder path to root first
                app.email_store.current_folder.clear();

                // Navigate through the full path to handle subfolders correctly
                app.email_store.enter_folder_by_path(&path);

                // Load emails with visible row limit
                let estimated_visible_rows = 20;
                let load_count = (estimated_visible_rows + 5).max(10);

                match app
                    .email_store
                    .ensure_current_folder_loaded_with_limit(&app.scanner, load_count)
                {
                    Ok(()) => {
                        app.selection.email_index = 0;
                        app.selection.scroll_offset = 0;
                        app.selection.remembered_email_index = None; // Clear remembered selection for new folder

                        // Select the first email if folder has emails
                        let current_folder = app.email_store.get_current_folder();
                        if !current_folder.emails.is_empty() {
                            app.email_store.select_email(0);
                        }

                        app.set_state(AppState::EmailList);
                    }
                    Err(e) => {
                        app.set_status(format!("Error loading folder: {}", e));
                    }
                }
            }
        }
        ActivePane::List => {
            // Select email and switch to content view
            let current_folder = app.email_store.get_current_folder();
            if app.selection.email_index < current_folder.emails.len() {
                app.email_store.select_email(app.selection.email_index);
                // Choose appropriate view based on content visibility
                if app.content_pane_hidden {
                    app.current_view = View::Messages;
                } else {
                    app.current_view = View::MessagesContent;
                }
                app.active_pane = ActivePane::List;
                app.set_state(AppState::EmailContent);
            }
        }
        ActivePane::Content => {
            // No action for content pane selection
        }
        ActivePane::Attachments => {
            // Open selected attachment
            handle_attachment_open(app, false);
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
        ActivePane::Attachments => {
            // Switch back to messages view
            app.current_view = View::MessagesContent;
            app.active_pane = ActivePane::List;
            app.set_state(AppState::EmailList);
        }
    }
}

fn handle_attachment_open(app: &mut App, custom_command: bool) {
    let filename = if let Some(email) = app.email_store.get_selected_email() {
        if app.selection.attachment_index < email.attachments.len() {
            Some(
                email.attachments[app.selection.attachment_index]
                    .filename
                    .clone(),
            )
        } else {
            None
        }
    } else {
        None
    };

    if let Some(filename) = filename {
        if custom_command {
            app.set_status(format!(
                "Custom command for {}: Not implemented yet",
                filename
            ));
        } else {
            app.set_status(format!("Opening {}: Not implemented yet", filename));
        }

        // TODO: Implement actual file opening with xdg-open or custom command
        // For now, just show a status message
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

pub fn get_folder_path_from_display_index(
    folder: &crate::email::Folder,
    display_index: usize,
) -> Option<Vec<usize>> {
    // Convert display index to a path of indices leading to the target folder
    let flat_folders = build_flat_folder_list(folder, 0);

    if display_index < flat_folders.len() {
        let (target_folder, _depth) = &flat_folders[display_index];
        return find_folder_path(folder, target_folder);
    }
    None
}

fn find_folder_path(
    current: &crate::email::Folder,
    target: &crate::email::Folder,
) -> Option<Vec<usize>> {
    // Direct match
    if std::ptr::eq(current, target) {
        return Some(Vec::new());
    }

    // Search in subfolders
    for (i, subfolder) in current.subfolders.iter().enumerate() {
        if let Some(mut path) = find_folder_path(subfolder, target) {
            path.insert(0, i);
            return Some(path);
        }
    }

    None
}

fn build_flat_folder_list(
    folder: &crate::email::Folder,
    depth: usize,
) -> Vec<(&crate::email::Folder, usize)> {
    let mut result = Vec::new();

    // Add current folder if not root
    if depth > 0 {
        result.push((folder, depth));
    }

    // Add subfolders in sorted order (matching UI display)
    for subfolder in folder.get_sorted_subfolders() {
        result.extend(build_flat_folder_list(subfolder, depth + 1));
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::email::{Email, EmailStore, Folder};
    use std::path::PathBuf;

    fn create_test_app_with_emails() -> App {
        let mut email_store = EmailStore::new(PathBuf::from("/tmp"));

        // Add a test folder with some emails
        let mut inbox = Folder::new("INBOX".to_string(), PathBuf::from("/tmp/INBOX"));
        inbox.add_email(Email::new(PathBuf::from("/tmp/email1")));
        inbox.add_email(Email::new(PathBuf::from("/tmp/email2")));
        inbox.add_email(Email::new(PathBuf::from("/tmp/email3")));
        inbox.is_loaded = true;

        email_store.root_folder.add_subfolder(inbox);

        let scanner = crate::maildir::MaildirScanner::new(PathBuf::from("/tmp"));
        App::new(email_store, scanner)
    }

    #[test]
    fn test_handle_quit_key() {
        let email_store = EmailStore::new(PathBuf::from("/tmp"));
        let scanner = crate::maildir::MaildirScanner::new(PathBuf::from("/tmp"));
        let mut app = App::new(email_store, scanner);

        let key_event = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        let should_quit = handle_key_event(&mut app, key_event);

        assert!(should_quit);
        assert!(matches!(app.state, AppState::Quit));
    }

    #[test]
    fn test_handle_help_key() {
        let email_store = EmailStore::new(PathBuf::from("/tmp"));
        let scanner = crate::maildir::MaildirScanner::new(PathBuf::from("/tmp"));
        let mut app = App::new(email_store, scanner);

        let key_event = KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE);
        let should_quit = handle_key_event(&mut app, key_event);

        assert!(!should_quit);
        assert!(matches!(app.state, AppState::Help));
    }

    #[test]
    fn test_content_toggle() {
        let email_store = EmailStore::new(PathBuf::from("/tmp"));
        let scanner = crate::maildir::MaildirScanner::new(PathBuf::from("/tmp"));
        let mut app = App::new(email_store, scanner);

        // Test toggle content with Alt+c
        assert!(!app.content_pane_hidden);

        let key_event = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::ALT);
        handle_key_event(&mut app, key_event);

        assert!(app.content_pane_hidden);

        // Test toggle back
        let key_event = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::ALT);
        handle_key_event(&mut app, key_event);

        assert!(!app.content_pane_hidden);
    }

    #[test]
    fn test_message_selection_memory_with_h_l_navigation() {
        let mut app = create_test_app_with_emails();

        // Navigate into INBOX folder
        app.email_store.current_folder = vec![0]; // Enter INBOX
        app.current_view = View::MessagesContent;
        app.active_pane = ActivePane::List;

        // Select the third email (index 2)
        app.selection.email_index = 2;
        app.email_store.select_email(2);

        // Verify email is selected
        assert_eq!(app.selection.email_index, 2);
        assert_eq!(app.email_store.selected_email, Some(2));

        // Switch to folder view using 'h'
        handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE),
        );

        // Should remember the selection but deselect the email
        assert_eq!(app.selection.remembered_email_index, Some(2));
        assert_eq!(app.email_store.selected_email, None);
        assert_eq!(app.current_view, View::FolderMessages);

        // Switch back to message view using 'l' from List pane
        app.active_pane = ActivePane::List; // Simulate being in List pane
        handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE),
        );

        // Should restore the selection
        assert_eq!(app.current_view, View::MessagesContent);
        assert_eq!(app.selection.email_index, 2);
        assert_eq!(app.email_store.selected_email, Some(2));
    }

    #[test]
    fn test_realistic_navigation_scenario() {
        let mut app = create_test_app_with_emails();

        // Start in default state (FolderMessages view, Folders pane)
        assert_eq!(app.current_view, View::FolderMessages);
        assert_eq!(app.active_pane, ActivePane::Folders);

        // Enter INBOX folder using 'l' from folder pane (should switch to MessagesContent view)
        handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE),
        );

        // Navigate to the second email using 'j'
        handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        );

        // Now we should have the second email selected
        assert_eq!(app.selection.email_index, 1);
        assert_eq!(app.email_store.selected_email, Some(1));

        // Go back to folder view using 'h'
        handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE),
        );

        // Should remember selection and deselect
        assert_eq!(app.selection.remembered_email_index, Some(1));
        assert_eq!(app.email_store.selected_email, None);

        // Go forward again using 'l'
        handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE),
        );

        // Should restore the selection
        assert_eq!(app.selection.email_index, 1);
        assert_eq!(app.email_store.selected_email, Some(1));
    }

    #[test]
    fn test_folder_change_clears_memory() {
        let mut app = create_test_app_with_emails();

        // Add another folder
        let mut sent = Folder::new("Sent".to_string(), PathBuf::from("/tmp/Sent"));
        sent.add_email(Email::new(PathBuf::from("/tmp/sent1")));
        sent.is_loaded = true;
        app.email_store.root_folder.add_subfolder(sent);

        // Navigate into INBOX and select an email
        app.email_store.current_folder = vec![0]; // Enter INBOX
        app.selection.email_index = 1;
        app.email_store.select_email(1);
        app.selection.remembered_email_index = Some(1);

        // Navigate to different folder using j/k
        app.active_pane = ActivePane::Folders;
        app.selection.folder_index = 1; // Move to Sent folder

        // Trigger folder loading (simulating what happens in handle_navigation)
        app.load_selected_folder_messages();

        // Should clear the remembered selection
        assert_eq!(app.selection.remembered_email_index, None);
        assert_eq!(app.selection.email_index, 0);
    }

    #[test]
    fn test_h_l_navigation_multiple_times() {
        let mut app = create_test_app_with_emails();

        // Enter INBOX folder using 'l' from folder pane
        handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE),
        );
        assert_eq!(app.current_view, View::MessagesContent);
        assert_eq!(app.active_pane, ActivePane::List);

        // Navigate to second email
        handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        );
        assert_eq!(app.selection.email_index, 1);

        // Go back to folder view using 'h'
        handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE),
        );
        assert_eq!(app.current_view, View::FolderMessages);
        assert_eq!(app.active_pane, ActivePane::Folders);

        // Go back to message view using 'l'
        handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE),
        );
        assert_eq!(app.current_view, View::MessagesContent);
        assert_eq!(app.active_pane, ActivePane::List);
        assert_eq!(app.selection.email_index, 1); // Should restore selection

        // Go back to folder view again using 'h'
        handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE),
        );
        assert_eq!(app.current_view, View::FolderMessages);
        assert_eq!(app.active_pane, ActivePane::Folders);

        // Go back to message view again using 'l'
        handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE),
        );
        assert_eq!(app.current_view, View::MessagesContent);
        assert_eq!(app.active_pane, ActivePane::List);
        assert_eq!(app.selection.email_index, 1); // Should still restore selection
    }

    #[test]
    fn test_l_key_enters_selected_folder_not_inbox() {
        let mut app = create_test_app_with_emails();

        // Add another folder (Sent) so we have INBOX (index 0) and Sent (index 1)
        let mut sent = Folder::new("Sent".to_string(), PathBuf::from("/tmp/Sent"));
        sent.add_email(Email::new(PathBuf::from("/tmp/sent1")));
        sent.is_loaded = true;
        app.email_store.root_folder.add_subfolder(sent);

        // Simulate real application startup sequence
        app.perform_initial_loading_if_needed(); // This is called in real app

        // Start in default state (FolderMessage view, Folders pane)
        // By default, INBOX should be selected (folder_index = 0)
        assert_eq!(app.selection.folder_index, 0); // Should be INBOX
        assert!(
            app.email_store.current_folder.is_empty(),
            "current_folder should be empty after initial loading, but is: {:?}",
            app.email_store.current_folder
        ); // Should not be inside any folder yet

        // Navigate down to Sent folder (index 1) using 'j' key
        handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        );

        // Should now have Sent folder selected
        assert_eq!(app.selection.folder_index, 1); // Should be Sent folder
        assert!(
            app.email_store.current_folder.is_empty(),
            "current_folder should be empty before entering folder, but is: {:?}",
            app.email_store.current_folder
        ); // Still not inside any folder

        // Now press 'l' to enter the selected folder (should be Sent, not INBOX)
        handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE),
        );

        // Should have entered Sent folder, not INBOX
        assert_eq!(app.email_store.current_folder, vec![1]); // Should be in Sent folder (index 1)
        assert_eq!(app.current_view, View::MessagesContent);

        // Verify we're actually in the Sent folder by checking the folder path
        let folder_path = app.email_store.get_folder_path();
        assert!(
            folder_path.contains("Sent"),
            "Should be in Sent folder, but path is: {}",
            folder_path
        );
    }

    #[test]
    fn test_h_l_navigation_when_content_pane_active() {
        let mut app = create_test_app_with_emails();

        // Enter INBOX folder and select an email
        app.email_store.current_folder = vec![0];
        app.current_view = View::MessagesContent;
        app.active_pane = ActivePane::List;
        app.selection.email_index = 1;
        app.email_store.select_email(1);

        // Simulate selecting an email and switching to content pane (like pressing Enter)
        app.active_pane = ActivePane::Content;
        app.set_state(crate::app::AppState::EmailContent);

        // Now press 'h' - should go to folder view
        handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE),
        );

        // And press 'l' - should go back to message view, not skip to content
        handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE),
        );

        // Should be in List pane, not Content pane
        assert_eq!(app.active_pane, ActivePane::List);

        // Do it again to make sure it doesn't skip
        handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE),
        );
        assert_eq!(app.active_pane, ActivePane::Folders);

        handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE),
        );
        assert_eq!(app.active_pane, ActivePane::List);

        // And one more time
        handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE),
        );
        assert_eq!(app.active_pane, ActivePane::Folders);

        handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE),
        );
        assert_eq!(app.active_pane, ActivePane::List);
    }
}
