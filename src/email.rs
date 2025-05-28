use std::collections::HashMap;
use std::path::PathBuf;
use std::fs;
use mailparse::{parse_mail, MailHeaderMap, ParsedMail, parse_content_disposition, DispositionType};
use html2md::parse_html;

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
    pub body_markdown: String,
    pub markdown_converted: bool,
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
            body_markdown: String::new(),
            markdown_converted: false,
            attachments: Vec::new(),
            file_path,
            is_unread: false,
            load_state: EmailLoadState::HeadersOnly,
        }
    }

    pub fn get_preview(&self) -> String {
        let preview_len = 100;
        let body = if !self.body_markdown.is_empty() {
            &self.body_markdown
        } else {
            &self.body_text
        };
        
        if body.len() <= preview_len {
            body.to_string()
        } else {
            format!("{}...", &body[..preview_len])
        }
    }

    /// Parse only headers from file (fast for folder loading)
    pub fn parse_headers_only(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let content = fs::read(&self.file_path)?;
        let parsed = parse_mail(&content)?;
        
        self.parse_headers(&parsed)?;
        self.load_state = EmailLoadState::HeadersOnly;
        
        Ok(())
    }

    /// Parse email from file (full parsing for reading)
    pub fn parse_from_file(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let content = fs::read(&self.file_path)?;
        let parsed = parse_mail(&content)?;
        
        self.parse_headers(&parsed)?;
        self.parse_body(&parsed)?;
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

    /// Parse email headers
    fn parse_headers(&mut self, parsed: &ParsedMail) -> Result<(), Box<dyn std::error::Error>> {
        self.headers.from = parsed.headers.get_first_value("From").unwrap_or_default();
        self.headers.to = parsed.headers.get_first_value("To").unwrap_or_default();
        self.headers.subject = parsed.headers.get_first_value("Subject").unwrap_or_default();
        self.headers.date = parsed.headers.get_first_value("Date").unwrap_or_default();
        self.headers.message_id = parsed.headers.get_first_value("Message-ID").unwrap_or_default();
        
        Ok(())
    }

    /// Parse email body and attachments
    fn parse_body(&mut self, parsed: &ParsedMail) -> Result<(), Box<dyn std::error::Error>> {
        self.extract_body_and_attachments(parsed)?;
        Ok(())
    }

    /// Get markdown content, converting lazily only when needed
    pub fn get_markdown_content(&mut self) -> &str {
        if !self.markdown_converted {
            // Convert HTML to markdown if we have HTML content
            if let Some(html) = &self.body_html {
                self.body_markdown = parse_html(html);
            } else if !self.body_text.is_empty() {
                // If we only have text, use it as markdown
                self.body_markdown = self.body_text.clone();
            }
            self.markdown_converted = true;
        }
        
        if !self.body_markdown.is_empty() {
            &self.body_markdown
        } else {
            &self.body_text
        }
    }

    /// Recursively extract body content and attachments
    fn extract_body_and_attachments(&mut self, parsed: &ParsedMail) -> Result<(), Box<dyn std::error::Error>> {
        let content_type = parsed.ctype.mimetype.as_str();
        
        // Check if this is an attachment by looking at Content-Disposition header
        let is_attachment = if let Some(disposition_header) = parsed.headers.get_first_value("Content-Disposition") {
            let disposition = parse_content_disposition(&disposition_header);
            matches!(disposition.disposition, DispositionType::Attachment)
        } else {
            false
        };
        
        if is_attachment {
            // This is an attachment
            let filename = parsed.ctype.params.get("name")
                .or_else(|| parsed.ctype.params.get("filename"))
                .map(|s| s.as_str())
                .unwrap_or("unknown")
                .to_string();
            
            let attachment = Attachment {
                filename,
                content_type: content_type.to_string(),
                size: parsed.get_body_raw()?.len(),
            };
            
            self.attachments.push(attachment);
        } else {
            // This is body content
            match content_type {
                "text/plain" => {
                    if self.body_text.is_empty() {
                        self.body_text = parsed.get_body()?;
                    }
                }
                "text/html" => {
                    if self.body_html.is_none() {
                        self.body_html = Some(parsed.get_body()?);
                    }
                }
                "multipart/alternative" | "multipart/mixed" | "multipart/related" => {
                    // Process subparts recursively
                    for subpart in &parsed.subparts {
                        self.extract_body_and_attachments(subpart)?;
                    }
                }
                _ => {
                    // Unknown content type, try to extract as text if not an attachment
                    if !is_attachment {
                        if let Ok(body) = parsed.get_body() {
                            if self.body_text.is_empty() {
                                self.body_text = body;
                            }
                        }
                    }
                }
            }
        }
        
        // Process subparts even for non-multipart types (just in case)
        for subpart in &parsed.subparts {
            self.extract_body_and_attachments(subpart)?;
        }
        
        Ok(())
    }

    /// Get formatted header display
    pub fn get_header_display(&self) -> String {
        format!(
            "From: {}\nTo: {}\nSubject: {}\nDate: {}",
            self.headers.from,
            self.headers.to,
            self.headers.subject,
            self.headers.date
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

    /// Load emails for current folder if not already loaded
    pub fn ensure_current_folder_loaded(&mut self, scanner: &crate::maildir::MaildirScanner) -> Result<(), Box<dyn std::error::Error>> {
        let folder = self.get_current_folder_mut();
        if !folder.is_loaded {
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
            Some(email.get_markdown_content().to_string())
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
