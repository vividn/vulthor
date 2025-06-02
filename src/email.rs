use mail_parser::{Message, MessageParser, MimeHeaders};
use std::collections::HashMap;
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

    pub fn get_preview(&self) -> String {
        let preview_len = 100;
        let body = &self.body_text;

        if body.len() <= preview_len {
            body.to_string()
        } else {
            format!("{}...", &body[..preview_len])
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
        if matches!(self.load_state, EmailLoadState::HeadersOnly) {
            self.parse_from_file()?;
        }
        Ok(())
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
                .unwrap_or("unnamed_attachment")
                .to_string();

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
        sorted.sort_by(|a, b| {
            // Keep INBOX at the top
            match (&a.name[..], &b.name[..]) {
                ("INBOX", _) => std::cmp::Ordering::Less,
                (_, "INBOX") => std::cmp::Ordering::Greater,
                (a_name, b_name) => a_name.cmp(b_name),
            }
        });
        sorted
    }

    pub fn get_display_name(&self) -> String {
        if self.unread_count > 0 {
            format!("{} ({})", self.name, self.unread_count)
        } else {
            self.name.clone()
        }
    }

    /// Get all emails recursively from this folder and its subfolders
    pub fn get_all_emails(&self) -> Vec<&Email> {
        let mut emails = Vec::new();

        // Add emails from this folder
        emails.extend(self.emails.iter());

        // Add emails from subfolders recursively
        for subfolder in &self.subfolders {
            emails.extend(subfolder.get_all_emails());
        }

        emails
    }
}

#[derive(Debug)]
pub struct EmailStore {
    pub root_folder: Folder,
    pub current_folder: Vec<usize>, // Path to current folder (indices in subfolder arrays)
    pub selected_email: Option<usize>, // Index of selected email in current folder
    email_cache: HashMap<PathBuf, Email>, // Cache for parsed emails
}

impl EmailStore {
    pub fn new(maildir_path: PathBuf) -> Self {
        Self {
            root_folder: Folder::new("Mail".to_string(), maildir_path),
            current_folder: Vec::new(),
            selected_email: None,
            email_cache: HashMap::new(),
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

    /// Navigate to a subfolder by index and load emails if needed
    pub fn enter_folder(&mut self, folder_index: usize) {
        let current = self.get_current_folder();
        if folder_index < current.subfolders.len() {
            self.current_folder.push(folder_index);
            self.selected_email = None; // Reset email selection
        }
    }

    /// Navigate to a folder by following a path of indices
    pub fn enter_folder_by_path(&mut self, path: &[usize]) {
        self.current_folder.extend_from_slice(path);
        self.selected_email = None; // Reset email selection
    }

    /// Load emails for current folder if not already loaded
    pub fn ensure_current_folder_loaded(
        &mut self,
        scanner: &crate::maildir::MaildirScanner,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let folder = self.get_current_folder_mut();
        if !folder.is_loaded {
            scanner.load_folder_emails(folder)?;
        }
        Ok(())
    }

    /// Load emails for a specific folder by index if not already loaded
    pub fn ensure_specific_folder_loaded(
        &mut self,
        folder_index: usize,
        scanner: &crate::maildir::MaildirScanner,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let current = self.get_current_folder_mut();
        if folder_index < current.subfolders.len() {
            let folder = &mut current.subfolders[folder_index];
            if !folder.is_loaded {
                scanner.load_folder_emails(folder)?;
            }
        }
        Ok(())
    }

    /// Load limited number of emails for current folder (for fast startup)
    pub fn ensure_current_folder_loaded_with_limit(
        &mut self,
        scanner: &crate::maildir::MaildirScanner,
        limit: usize,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let folder = self.get_current_folder_mut();
        if !folder.is_loaded {
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
        if !self.current_folder.is_empty() {
            self.current_folder.pop();
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
                if let Err(_) = email.ensure_fully_loaded() {
                    // If loading fails, return None
                    return None;
                }
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
            if let Err(_) = email.ensure_fully_loaded() {
                return None;
            }
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
}
