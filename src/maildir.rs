use crate::email::{Email, Folder};
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Debug)]
pub struct MaildirScanner {
    root_path: PathBuf,
}

impl MaildirScanner {
    pub fn new(root_path: PathBuf) -> Self {
        Self { root_path }
    }

    /// Scan the MailDir structure and build folder hierarchy (fast startup - structure only)
    pub fn scan(&self) -> Result<Folder, Box<dyn std::error::Error>> {
        if !self.root_path.exists() {
            return Err(format!("MailDir path does not exist: {}", self.root_path.display()).into());
        }

        if !self.root_path.is_dir() {
            return Err(format!("MailDir path is not a directory: {}", self.root_path.display()).into());
        }

        let mut root_folder = Folder::new("Mail".to_string(), self.root_path.clone());
        self.scan_folder_structure_only(&mut root_folder, &self.root_path)?;
        
        // Immediately load INBOX if it exists
        if let Some(inbox_index) = root_folder.subfolders.iter().position(|f| f.name == "INBOX") {
            self.load_folder_emails(&mut root_folder.subfolders[inbox_index])?;
        }
        
        Ok(root_folder)
    }

    /// Scan folder structure only (no email loading) for fast startup
    fn scan_folder_structure_only(&self, folder: &mut Folder, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        // Look for subfolders only
        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries {
                if let Ok(entry) = entry {
                    let entry_path = entry.path();
                    if entry_path.is_dir() {
                        let dir_name = entry_path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("Unknown");

                        // Skip the maildir special directories
                        if dir_name == "cur" || dir_name == "new" || dir_name == "tmp" {
                            continue;
                        }

                        // Skip hidden directories
                        if dir_name.starts_with('.') {
                            continue;
                        }

                        // Create subfolder and recursively scan its structure only
                        let mut subfolder = Folder::new(dir_name.to_string(), entry_path.clone());
                        self.scan_folder_structure_only(&mut subfolder, &entry_path)?;
                        folder.add_subfolder(subfolder);
                    }
                }
            }
        }

        Ok(())
    }

    /// Load emails for a specific folder (lazy loading)
    pub fn load_folder_emails(&self, folder: &mut Folder) -> Result<(), Box<dyn std::error::Error>> {
        // Only load if not already loaded
        if folder.is_loaded {
            return Ok(());
        }

        let path = &folder.path;
        
        // Check if this is a maildir folder (contains cur/, new/, tmp/)
        let cur_path = path.join("cur");
        let new_path = path.join("new");
        let tmp_path = path.join("tmp");

        let is_maildir = cur_path.exists() && new_path.exists() && tmp_path.exists();

        if is_maildir {
            // This is a maildir folder, scan for emails
            self.scan_emails_in_folder(folder, &cur_path)?;
            self.scan_emails_in_folder(folder, &new_path)?;
        }

        folder.is_loaded = true;
        Ok(())
    }


    /// Scan emails in a specific directory (cur or new)
    fn scan_emails_in_folder(&self, folder: &mut Folder, dir_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        if !dir_path.exists() || !dir_path.is_dir() {
            return Ok(()); // Skip if directory doesn't exist
        }

        for entry in WalkDir::new(dir_path).min_depth(1).max_depth(1) {
            let entry = entry?;
            let path = entry.path();

            if path.is_file() {
                // Check if this looks like an email file
                if self.is_email_file(path) {
                    let is_unread = dir_path.file_name()
                        .and_then(|name| name.to_str())
                        .map(|name| name == "new")
                        .unwrap_or(false);

                    let mut email = Email::new(path.to_path_buf());
                    email.is_unread = is_unread;
                    
                    // Parse the email content
                    match email.parse_from_file() {
                        Ok(()) => {
                            // Email parsed successfully
                            folder.add_email(email);
                        }
                        Err(e) => {
                            // If parsing fails, create a placeholder with error info
                            email.headers.subject = format!("Parse Error: {}", e);
                            email.body_text = format!("Failed to parse email from {}: {}", 
                                path.display(), e);
                            email.body_markdown = email.body_text.clone();
                            folder.add_email(email);
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Check if a file looks like an email file
    fn is_email_file(&self, path: &Path) -> bool {
        // Skip temporary files and other non-email files
        if let Some(filename) = path.file_name().and_then(|name| name.to_str()) {
            // Skip files that start with a dot (hidden files)
            if filename.starts_with('.') {
                return false;
            }
            
            // Skip common non-email files
            if filename.ends_with(".lock") || filename.ends_with(".tmp") {
                return false;
            }
            
            // If it's a regular file and not excluded, assume it's an email
            path.is_file()
        } else {
            false
        }
    }

    /// Get folder statistics
    pub fn get_folder_stats(&self, folder: &Folder) -> FolderStats {
        let mut stats = FolderStats {
            total_emails: folder.emails.len(),
            unread_emails: folder.emails.iter().filter(|e| e.is_unread).count(),
            total_folders: 1, // Count this folder
            total_size: 0,
        };

        // Add stats from subfolders recursively
        for subfolder in &folder.subfolders {
            let subfolder_stats = self.get_folder_stats(subfolder);
            stats.total_emails += subfolder_stats.total_emails;
            stats.unread_emails += subfolder_stats.unread_emails;
            stats.total_folders += subfolder_stats.total_folders;
            stats.total_size += subfolder_stats.total_size;
        }

        // Calculate size (this is expensive, so we might want to cache it)
        for email in &folder.emails {
            if let Ok(metadata) = fs::metadata(&email.file_path) {
                stats.total_size += metadata.len();
            }
        }

        stats
    }
}

#[derive(Debug, Clone)]
pub struct FolderStats {
    pub total_emails: usize,
    pub unread_emails: usize,
    pub total_folders: usize,
    pub total_size: u64, // Size in bytes
}

impl FolderStats {
    pub fn format_size(&self) -> String {
        let size = self.total_size as f64;
        if size < 1024.0 {
            format!("{} B", size)
        } else if size < 1024.0 * 1024.0 {
            format!("{:.1} KB", size / 1024.0)
        } else if size < 1024.0 * 1024.0 * 1024.0 {
            format!("{:.1} MB", size / (1024.0 * 1024.0))
        } else {
            format!("{:.1} GB", size / (1024.0 * 1024.0 * 1024.0))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_maildir_scanner_empty_dir() {
        let temp_dir = TempDir::new().unwrap();
        let scanner = MaildirScanner::new(temp_dir.path().to_path_buf());
        
        let result = scanner.scan().unwrap();
        assert_eq!(result.name, "Mail");
        assert!(result.emails.is_empty());
        assert!(result.subfolders.is_empty());
    }

    #[test]
    fn test_maildir_scanner_basic_structure() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();
        
        // Create basic maildir structure
        fs::create_dir_all(root.join("INBOX/cur")).unwrap();
        fs::create_dir_all(root.join("INBOX/new")).unwrap();
        fs::create_dir_all(root.join("INBOX/tmp")).unwrap();
        
        // Create a test email file
        fs::write(root.join("INBOX/cur/test_email"), "Test email content").unwrap();
        
        let scanner = MaildirScanner::new(root.to_path_buf());
        let result = scanner.scan().unwrap();
        
        assert_eq!(result.subfolders.len(), 1);
        assert_eq!(result.subfolders[0].name, "INBOX");
        assert_eq!(result.subfolders[0].emails.len(), 1);
    }
}