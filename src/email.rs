use mail_parser::{Message, MessageParser, MimeHeaders};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct EmailHeaders {
    pub from: String,
    pub to: String,
    pub subject: String,
    pub date: String,
    pub message_id: String,
}

#[derive(Debug, Clone)]
pub struct Attachment {
    pub filename: String,
    pub content_type: String,
    pub size: usize,
}

#[derive(Debug, Clone)]
pub enum EmailLoadState {
    HeadersOnly,
    FullyLoaded,
}

#[derive(Debug, Clone)]
pub struct Email {
    pub headers: EmailHeaders,
    pub body_text: String,
    pub body_html: Option<String>,
    pub attachments: Vec<Attachment>,
    pub file_path: PathBuf,
    pub is_unread: bool,
    pub load_state: EmailLoadState,
}

impl Email {
    pub fn new(file_path: PathBuf) -> Self {
        Self {
            headers: EmailHeaders {
                from: String::new(),
                to: String::new(),
                subject: String::new(),
                date: String::new(),
                message_id: String::new(),
            },
            body_text: String::new(),
            body_html: None,
            attachments: Vec::new(),
            file_path,
            is_unread: false,
            load_state: EmailLoadState::HeadersOnly,
        }
    }


    /// Parse only headers from file (fast for folder loading)
    pub fn parse_headers_only(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let content = fs::read(&self.file_path)?;
        let message = MessageParser::default()
            .parse(&content)
            .ok_or("Failed to parse email headers")?;

        self.parse_headers(&message)?;
        self.load_state = EmailLoadState::HeadersOnly;

        Ok(())
    }

    /// Parse email from file (full parsing for reading)
    pub fn parse_from_file(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let content = fs::read(&self.file_path)?;
        let message = MessageParser::default()
            .parse(&content)
            .ok_or("Failed to parse email")?;

        self.parse_headers(&message)?;
        self.parse_body(&message)?;
        self.load_state = EmailLoadState::FullyLoaded;

        Ok(())
    }

    /// Ensure email is fully loaded
    pub fn ensure_fully_loaded(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        match self.load_state {
            EmailLoadState::HeadersOnly => self.parse_from_file(),
            EmailLoadState::FullyLoaded => Ok(()),
        }
    }

    /// Parse email headers elegantly using mail-parser
    fn parse_headers(&mut self, message: &Message) -> Result<(), Box<dyn std::error::Error>> {
        // Extract from address with elegant formatting
        self.headers.from = message
            .from()
            .and_then(|addr| addr.first())
            .map(|addr| match (addr.name(), addr.address()) {
                (Some(name), Some(email)) => format!("{} <{}>", name, email),
                (None, Some(email)) => email.to_string(),
                (Some(name), None) => name.to_string(),
                _ => "Unknown".to_string(),
            })
            .unwrap_or_default();

        // Extract to address with same elegant formatting
        self.headers.to = message
            .to()
            .and_then(|addr| addr.first())
            .map(|addr| match (addr.name(), addr.address()) {
                (Some(name), Some(email)) => format!("{} <{}>", name, email),
                (None, Some(email)) => email.to_string(),
                (Some(name), None) => name.to_string(),
                _ => "Unknown".to_string(),
            })
            .unwrap_or_default();

        // Subject, date, and message-id are straightforward
        self.headers.subject = message.subject().unwrap_or("(no subject)").to_string();
        self.headers.date = message.date().map(|d| d.to_rfc3339()).unwrap_or_default();
        self.headers.message_id = message.message_id().unwrap_or_default().to_string();

        Ok(())
    }

    /// Parse email body elegantly using mail-parser's built-in text conversion
    fn parse_body(&mut self, message: &Message) -> Result<(), Box<dyn std::error::Error>> {
        // mail-parser automatically converts HTML to plain text when needed
        // Try to get plain text first (index 0 = first text part)
        if let Some(text_body) = message.body_text(0) {
            self.body_text = text_body.to_string();
        }

        // Store HTML if available (for web serving)
        if let Some(html_body) = message.body_html(0) {
            self.body_html = Some(html_body.to_string());
        }

        // Extract attachments
        self.extract_attachments(message)?;

        Ok(())
    }

    /// Extract attachments elegantly using mail-parser
    fn extract_attachments(&mut self, message: &Message) -> Result<(), Box<dyn std::error::Error>> {
        // Iterate through all attachments using mail-parser's clean API
        let mut index = 0;
        while let Some(attachment_part) = message.attachment(index) {
            let filename = attachment_part
                .attachment_name()
                .map(|s| s.to_string())
                .unwrap_or_else(|| "unnamed_attachment".to_string());

            let content_type = attachment_part
                .content_type()
                .map(|ct| format!("{}/{}", ct.c_type, ct.subtype().unwrap_or("*")))
                .unwrap_or_else(|| "application/octet-stream".to_string());

            let size = attachment_part.len();

            let attachment = Attachment {
                filename,
                content_type,
                size,
            };

            self.attachments.push(attachment);
            index += 1;
        }

        Ok(())
    }

    /// Get formatted header display
    pub fn get_header_display(&self) -> String {
        format!(
            "From: {}\nTo: {}\nSubject: {}\nDate: {}",
            self.headers.from, self.headers.to, self.headers.subject, self.headers.date
        )
    }

    /// Check if email has attachments
    pub fn has_attachments(&self) -> bool {
        !self.attachments.is_empty()
    }

    /// Get attachment count
    pub fn attachment_count(&self) -> usize {
        self.attachments.len()
    }
}

#[derive(Debug, Clone)]
pub struct Folder {
    pub name: String,
    pub path: PathBuf,
    pub emails: Vec<Email>,
    pub subfolders: Vec<Folder>,
    pub unread_count: usize,
    pub total_count: usize,
    pub is_loaded: bool, // Track if emails have been loaded for this folder
}

impl Folder {
    pub fn new(name: String, path: PathBuf) -> Self {
        Self {
            name,
            path,
            emails: Vec::new(),
            subfolders: Vec::new(),
            unread_count: 0,
            total_count: 0,
            is_loaded: false,
        }
    }

    pub fn add_email(&mut self, email: Email) {
        if email.is_unread {
            self.unread_count += 1;
        }
        self.total_count += 1;
        self.emails.push(email);
    }

    pub fn add_subfolder(&mut self, folder: Folder) {
        self.subfolders.push(folder);
    }

    pub fn get_sorted_subfolders(&self) -> Vec<&Folder> {
        let mut sorted: Vec<&Folder> = self.subfolders.iter().collect();
        sorted.sort_by(|a, b| match (&a.name[..], &b.name[..]) {
            ("INBOX", _) => std::cmp::Ordering::Less,
            (_, "INBOX") => std::cmp::Ordering::Greater,
            (a_name, b_name) => a_name.cmp(b_name),
        });
        sorted
    }

    pub fn get_display_name(&self) -> String {
        match self.unread_count {
            0 => self.name.clone(),
            count => format!("{} ({})", self.name, count),
        }
    }

}

#[derive(Debug)]
pub struct EmailStore {
    pub root_folder: Folder,
    pub current_folder: Vec<usize>, // Path to current folder (indices in subfolder arrays)
    pub selected_email: Option<usize>, // Index of selected email in current folder
}

impl EmailStore {
    pub fn new(maildir_path: PathBuf) -> Self {
        Self {
            root_folder: Folder::new("Mail".to_string(), maildir_path),
            current_folder: Vec::new(),
            selected_email: None,
        }
    }

    /// Get reference to current folder based on current_folder path
    pub fn get_current_folder(&self) -> &Folder {
        let mut folder = &self.root_folder;
        for &index in &self.current_folder {
            if index < folder.subfolders.len() {
                folder = &folder.subfolders[index];
            }
        }
        folder
    }

    /// Get mutable reference to current folder
    pub fn get_current_folder_mut(&mut self) -> &mut Folder {
        let mut folder = &mut self.root_folder;
        for &index in &self.current_folder {
            if index < folder.subfolders.len() {
                folder = &mut folder.subfolders[index];
            }
        }
        folder
    }


    /// Navigate to a folder by following a path of indices
    pub fn enter_folder_by_path(&mut self, path: &[usize]) {
        self.current_folder.extend_from_slice(path);
        self.selected_email = None; // Reset email selection
    }



    /// Load emails for a folder at a specific path with visible row limit
    pub fn ensure_folder_at_path_loaded(
        &mut self,
        path: &[usize],
        scanner: &crate::maildir::MaildirScanner,
        visible_rows: usize,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut folder = &mut self.root_folder;
        for &index in path {
            if index < folder.subfolders.len() {
                folder = &mut folder.subfolders[index];
            } else {
                return Err("Invalid folder path".into());
            }
        }

        // Only load if the folder has no emails or is not loaded
        if !folder.is_loaded && folder.emails.is_empty() {
            let load_count = (visible_rows + 5).max(10); // At least 10, but typically visible + 5
            scanner.load_folder_emails_with_limit(folder, Some(load_count))?;
        }
        Ok(())
    }

    /// Get folder at a specific path (read-only)
    pub fn get_folder_at_path(&self, path: &[usize]) -> Option<&Folder> {
        let mut folder = &self.root_folder;
        for &index in path {
            if index < folder.subfolders.len() {
                folder = &folder.subfolders[index];
            } else {
                return None;
            }
        }
        Some(folder)
    }

    /// Load limited number of emails for current folder (for fast startup)
    pub fn ensure_current_folder_loaded_with_limit(
        &mut self,
        scanner: &crate::maildir::MaildirScanner,
        limit: usize,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let folder = self.get_current_folder_mut();
        // Only load if the folder has no emails or is not loaded
        if !folder.is_loaded && folder.emails.is_empty() {
            scanner.load_folder_emails_with_limit(folder, Some(limit))?;
        }
        Ok(())
    }

    /// Load more messages if current folder is not fully loaded
    pub fn load_more_messages_if_needed(
        &mut self,
        scanner: &crate::maildir::MaildirScanner,
        index: usize,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let folder = self.get_current_folder_mut();
        // If user is near the end of loaded messages and folder is not fully loaded, load more
        if !folder.is_loaded && index + 5 >= folder.emails.len() {
            // Load the full folder
            scanner.load_folder_emails(folder)?;
        }
        Ok(())
    }

    /// Navigate back to parent folder
    pub fn exit_folder(&mut self) {
        if self.current_folder.pop().is_some() {
            self.selected_email = None; // Reset email selection
        }
    }

    /// Select an email by index in current folder
    pub fn select_email(&mut self, email_index: usize) {
        let current = self.get_current_folder();
        if email_index < current.emails.len() {
            self.selected_email = Some(email_index);
        }
    }

    /// Get currently selected email (ensures it's fully loaded)
    pub fn get_selected_email(&mut self) -> Option<&Email> {
        if let Some(index) = self.selected_email {
            let current = self.get_current_folder_mut();
            if let Some(email) = current.emails.get_mut(index) {
                // Ensure email is fully loaded when accessed for reading
                email.ensure_fully_loaded().ok()?;
                return current.emails.get(index);
            }
        }
        None
    }

    /// Get currently selected email (read-only, may be headers-only)
    pub fn get_selected_email_headers(&self) -> Option<&Email> {
        let current = self.get_current_folder();
        self.selected_email
            .and_then(|index| current.emails.get(index))
    }

    /// Get currently selected email mutably
    pub fn get_selected_email_mut(&mut self) -> Option<&mut Email> {
        let selected = self.selected_email;
        let current = self.get_current_folder_mut();
        selected.and_then(move |index| current.emails.get_mut(index))
    }

    /// Get markdown content for the currently selected email (lazy conversion)
    pub fn get_selected_email_markdown(&mut self) -> Option<String> {
        if let Some(email) = self.get_selected_email_mut() {
            // Ensure email is fully loaded
            email.ensure_fully_loaded().ok()?;
            Some(email.body_text.clone())
        } else {
            None
        }
    }

    /// Get folder path as breadcrumb string
    pub fn get_folder_path(&self) -> String {
        let mut path = vec!["Mail".to_string()];
        let mut folder = &self.root_folder;

        for &index in &self.current_folder {
            if index < folder.subfolders.len() {
                folder = &folder.subfolders[index];
                path.push(folder.name.clone());
            }
        }

        path.join(" > ")
    }

    /// Get folder path for a specific folder path
    pub fn get_folder_path_for_indices(&self, indices: &[usize]) -> String {
        let mut path = vec!["Mail".to_string()];
        let mut folder = &self.root_folder;

        for &index in indices {
            if index < folder.subfolders.len() {
                folder = &folder.subfolders[index];
                path.push(folder.name.clone());
            }
        }

        path.join(" > ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures::TestMailDir;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    #[test]
    fn test_email_new() {
        let email_path = PathBuf::from("/tmp/test_email");
        let email = Email::new(email_path.clone());
        
        assert_eq!(email.file_path, email_path);
        assert_eq!(email.headers.subject, "");
        assert_eq!(email.headers.from, "");
        assert_eq!(email.headers.to, "");
        assert_eq!(email.body_text, "");
        assert!(email.body_html.is_none());
        assert!(email.attachments.is_empty());
        assert!(!email.is_unread);
        assert!(matches!(email.load_state, EmailLoadState::HeadersOnly));
    }

    #[test]
    fn test_email_parse_headers_only() {
        let test_maildir = TestMailDir::new();
        let inbox_path = test_maildir.get_folder_path("INBOX").join("cur");
        
        // Find the first email file
        let email_files: Vec<_> = fs::read_dir(&inbox_path).unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().is_file())
            .collect();
        
        assert!(!email_files.is_empty(), "No email files found in test INBOX");
        
        let email_path = email_files[0].path();
        let mut email = Email::new(email_path);
        
        // Parse headers only
        let result = email.parse_headers_only();
        assert!(result.is_ok(), "Failed to parse email headers: {:?}", result);
        
        // Verify headers were parsed
        assert!(!email.headers.subject.is_empty());
        assert!(!email.headers.from.is_empty());
        assert!(matches!(email.load_state, EmailLoadState::HeadersOnly));
        
        // Body should still be empty since we only parsed headers
        assert_eq!(email.body_text, "");
    }

    #[test]
    fn test_email_parse_from_file_complete() {
        let test_maildir = TestMailDir::new();
        let inbox_path = test_maildir.get_folder_path("INBOX").join("cur");
        
        // Find the first email file
        let email_files: Vec<_> = fs::read_dir(&inbox_path).unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().is_file())
            .collect();
        
        let email_path = email_files[0].path();
        let mut email = Email::new(email_path);
        
        // Parse complete email
        let result = email.parse_from_file();
        assert!(result.is_ok(), "Failed to parse email: {:?}", result);
        
        // Verify headers were parsed
        assert!(!email.headers.subject.is_empty());
        assert!(!email.headers.from.is_empty());
        assert!(!email.headers.to.is_empty());
        
        // Verify body was parsed
        assert!(!email.body_text.is_empty());
        assert!(matches!(email.load_state, EmailLoadState::FullyLoaded));
    }

    #[test]
    fn test_email_ensure_fully_loaded() {
        let test_maildir = TestMailDir::new();
        let inbox_path = test_maildir.get_folder_path("INBOX").join("cur");
        
        let email_files: Vec<_> = fs::read_dir(&inbox_path).unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().is_file())
            .collect();
        
        let email_path = email_files[0].path();
        let mut email = Email::new(email_path);
        
        // Start with headers only
        email.parse_headers_only().unwrap();
        assert!(matches!(email.load_state, EmailLoadState::HeadersOnly));
        assert_eq!(email.body_text, "");
        
        // Ensure fully loaded
        let result = email.ensure_fully_loaded();
        assert!(result.is_ok());
        assert!(matches!(email.load_state, EmailLoadState::FullyLoaded));
        assert!(!email.body_text.is_empty());
    }

    #[test]
    fn test_email_with_attachments() {
        let test_maildir = TestMailDir::new();
        let inbox_path = test_maildir.get_folder_path("INBOX").join("cur");
        
        // Find the attachment email (should be the 5th email based on our fixture)
        let email_files: Vec<_> = fs::read_dir(&inbox_path).unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().is_file())
            .collect();
        
        // Find the email with attachments by checking content
        let mut attachment_email = None;
        for file in email_files {
            let content = fs::read_to_string(file.path()).unwrap();
            if content.contains("Content-Disposition: attachment") {
                attachment_email = Some(file.path());
                break;
            }
        }
        
        assert!(attachment_email.is_some(), "No attachment email found in test data");
        
        let mut email = Email::new(attachment_email.unwrap());
        email.parse_from_file().unwrap();
        
        assert!(email.has_attachments());
        assert!(email.attachment_count() > 0);
        
        // Verify attachment details
        let attachment = &email.attachments[0];
        assert!(!attachment.filename.is_empty());
        assert!(!attachment.content_type.is_empty());
        assert!(attachment.size > 0);
    }

    #[test]
    fn test_email_html_content() {
        let test_maildir = TestMailDir::new();
        let inbox_path = test_maildir.get_folder_path("INBOX").join("cur");
        
        // Find the newsletter email (contains HTML)
        let email_files: Vec<_> = fs::read_dir(&inbox_path).unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().is_file())
            .collect();
        
        let mut html_email = None;
        for file in email_files {
            let content = fs::read_to_string(file.path()).unwrap();
            if content.contains("Content-Type: text/html") {
                html_email = Some(file.path());
                break;
            }
        }
        
        if let Some(email_path) = html_email {
            let mut email = Email::new(email_path);
            email.parse_from_file().unwrap();
            
            assert!(email.body_html.is_some());
            let html_content = email.body_html.as_ref().unwrap();
            assert!(html_content.contains("<html>") || html_content.contains("<h1>"));
        }
    }


    #[test]
    fn test_email_get_header_display() {
        let test_maildir = TestMailDir::new();
        let inbox_path = test_maildir.get_folder_path("INBOX").join("cur");
        
        let email_files: Vec<_> = fs::read_dir(&inbox_path).unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().is_file())
            .collect();
        
        let email_path = email_files[0].path();
        let mut email = Email::new(email_path);
        email.parse_from_file().unwrap();
        
        let header_display = email.get_header_display();
        assert!(header_display.contains("From:"));
        assert!(header_display.contains("To:"));
        assert!(header_display.contains("Subject:"));
        assert!(header_display.contains("Date:"));
    }

    #[test]
    fn test_folder_creation_and_management() {
        let temp_dir = TempDir::new().unwrap();
        let folder_path = temp_dir.path().to_path_buf();
        let folder = Folder::new("Test Folder".to_string(), folder_path.clone());
        
        assert_eq!(folder.name, "Test Folder");
        assert_eq!(folder.path, folder_path);
        assert_eq!(folder.emails.len(), 0);
        assert_eq!(folder.subfolders.len(), 0);
        assert_eq!(folder.unread_count, 0);
        assert_eq!(folder.total_count, 0);
        assert!(!folder.is_loaded);
    }

    #[test]
    fn test_folder_add_email() {
        let temp_dir = TempDir::new().unwrap();
        let mut folder = Folder::new("Test".to_string(), temp_dir.path().to_path_buf());
        
        let mut email = Email::new(PathBuf::from("/tmp/test"));
        email.is_unread = true;
        
        folder.add_email(email);
        
        assert_eq!(folder.emails.len(), 1);
        assert_eq!(folder.unread_count, 1);
        assert_eq!(folder.total_count, 1);
    }

    #[test]
    fn test_folder_add_subfolder() {
        let temp_dir = TempDir::new().unwrap();
        let mut parent_folder = Folder::new("Parent".to_string(), temp_dir.path().to_path_buf());
        let child_folder = Folder::new("Child".to_string(), temp_dir.path().join("child"));
        
        parent_folder.add_subfolder(child_folder);
        
        assert_eq!(parent_folder.subfolders.len(), 1);
        assert_eq!(parent_folder.subfolders[0].name, "Child");
    }

    #[test]
    fn test_folder_get_sorted_subfolders() {
        let temp_dir = TempDir::new().unwrap();
        let mut folder = Folder::new("Root".to_string(), temp_dir.path().to_path_buf());
        
        // Add folders in non-alphabetical order
        folder.add_subfolder(Folder::new("Zebra".to_string(), temp_dir.path().join("zebra")));
        folder.add_subfolder(Folder::new("INBOX".to_string(), temp_dir.path().join("inbox")));
        folder.add_subfolder(Folder::new("Alpha".to_string(), temp_dir.path().join("alpha")));
        
        let sorted = folder.get_sorted_subfolders();
        
        // INBOX should be first, then alphabetical
        assert_eq!(sorted[0].name, "INBOX");
        assert_eq!(sorted[1].name, "Alpha");
        assert_eq!(sorted[2].name, "Zebra");
    }

    #[test]
    fn test_folder_get_display_name() {
        let temp_dir = TempDir::new().unwrap();
        let mut folder = Folder::new("Test".to_string(), temp_dir.path().to_path_buf());
        
        // No unread emails
        assert_eq!(folder.get_display_name(), "Test");
        
        // With unread emails
        folder.unread_count = 5;
        assert_eq!(folder.get_display_name(), "Test (5)");
    }

    #[test]
    fn test_email_store_creation() {
        let temp_dir = TempDir::new().unwrap();
        let store = EmailStore::new(temp_dir.path().to_path_buf());
        
        assert_eq!(store.root_folder.name, "Mail");
        assert_eq!(store.root_folder.path, temp_dir.path());
        assert!(store.current_folder.is_empty());
        assert!(store.selected_email.is_none());
    }

    #[test]
    fn test_email_store_navigation() {
        let test_maildir = TestMailDir::new();
        let mut store = EmailStore::new(test_maildir.root_path.clone());
        
        // Set up root folder with test structure
        let scanner = crate::maildir::MaildirScanner::new(test_maildir.root_path.clone());
        store.root_folder = scanner.scan().unwrap();
        
        // Test entering a folder
        assert!(store.current_folder.is_empty());
        store.enter_folder_by_path(&[0]); // Enter first subfolder
        assert_eq!(store.current_folder.len(), 1);
        assert_eq!(store.current_folder[0], 0);
        
        // Test exiting folder
        store.exit_folder();
        assert!(store.current_folder.is_empty());
    }

    #[test]
    fn test_email_store_get_folder_path() {
        let test_maildir = TestMailDir::new();
        let mut store = EmailStore::new(test_maildir.root_path.clone());
        
        // Set up root folder with test structure
        let scanner = crate::maildir::MaildirScanner::new(test_maildir.root_path.clone());
        store.root_folder = scanner.scan().unwrap();
        
        // Test root path
        let path = store.get_folder_path();
        assert_eq!(path, "Mail");
        
        // Test with navigation - enter first folder
        if !store.root_folder.subfolders.is_empty() {
            store.enter_folder_by_path(&[0]);
            let path_with_folder = store.get_folder_path();
            assert!(path_with_folder.starts_with("Mail > "));
        }
    }

    #[test]
    fn test_email_store_email_selection() {
        let test_maildir = TestMailDir::new();
        let mut store = EmailStore::new(test_maildir.root_path.clone());
        
        // Set up root folder with test structure
        let scanner = crate::maildir::MaildirScanner::new(test_maildir.root_path.clone());
        store.root_folder = scanner.scan().unwrap();
        
        // Navigate to INBOX and load emails
        store.enter_folder_by_path(&[0]); // Enter first folder (should be INBOX)
        scanner.load_folder_emails(store.get_current_folder_mut()).unwrap();
        
        // Initially no email selected
        assert!(store.selected_email.is_none());
        
        // Select an email (if there are emails in the folder)
        let current_folder = store.get_current_folder();
        if !current_folder.emails.is_empty() {
            store.select_email(0);
            assert_eq!(store.selected_email, Some(0));
            
            // Test invalid selection
            store.select_email(999);
            assert_eq!(store.selected_email, Some(0)); // Should remain unchanged
        }
    }

    #[test]
    fn test_email_parsing_edge_cases() {
        // Test email with missing headers
        let temp_dir = TempDir::new().unwrap();
        let email_path = temp_dir.path().join("malformed_email");
        
        let malformed_content = "This is not a proper email format\nNo headers here";
        fs::write(&email_path, malformed_content).unwrap();
        
        let mut email = Email::new(email_path);
        let result = email.parse_from_file();
        
        // Should handle gracefully
        assert!(result.is_ok());
    }

    #[test]
    fn test_email_with_unicode_content() {
        let temp_dir = TempDir::new().unwrap();
        let email_path = temp_dir.path().join("unicode_email");
        
        let unicode_content = r#"From: sender@example.com
To: recipient@example.com
Subject: Unicode Test 🚀
Date: Mon, 01 Jan 2024 12:00:00 +0000
MIME-Version: 1.0
Content-Type: text/plain; charset=UTF-8

Hello 世界! This email contains unicode: 🎉 αβγ 中文"#;
        
        fs::write(&email_path, unicode_content).unwrap();
        
        let mut email = Email::new(email_path);
        let result = email.parse_from_file();
        
        assert!(result.is_ok());
        assert!(email.headers.subject.contains("🚀"));
        assert!(email.body_text.contains("世界"));
        assert!(email.body_text.contains("🎉"));
    }
}
