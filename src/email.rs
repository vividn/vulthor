use std::collections::HashMap;
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
pub struct Email {
    pub headers: EmailHeaders,
    pub body_text: String,
    pub body_html: Option<String>,
    pub body_markdown: String,
    pub attachments: Vec<Attachment>,
    pub file_path: PathBuf,
    pub is_unread: bool,
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
            attachments: Vec::new(),
            file_path,
            is_unread: false,
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
}

#[derive(Debug, Clone)]
pub struct Folder {
    pub name: String,
    pub path: PathBuf,
    pub emails: Vec<Email>,
    pub subfolders: Vec<Folder>,
    pub unread_count: usize,
    pub total_count: usize,
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

    /// Navigate to a subfolder by index
    pub fn enter_folder(&mut self, folder_index: usize) {
        let current = self.get_current_folder();
        if folder_index < current.subfolders.len() {
            self.current_folder.push(folder_index);
            self.selected_email = None; // Reset email selection
        }
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

    /// Get currently selected email
    pub fn get_selected_email(&self) -> Option<&Email> {
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