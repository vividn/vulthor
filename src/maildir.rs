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
            return Err(
                format!("MailDir path does not exist: {}", self.root_path.display()).into(),
            );
        }

        if !self.root_path.is_dir() {
            return Err(format!(
                "MailDir path is not a directory: {}",
                self.root_path.display()
            )
            .into());
        }

        let mut root_folder = Folder::new("Mail".to_string(), self.root_path.clone());
        self.scan_folder_structure_only(&mut root_folder, &self.root_path)?;

        Ok(root_folder)
    }

    /// Scan folder structure only (no email loading) for fast startup
    fn scan_folder_structure_only(
        &self,
        folder: &mut Folder,
        path: &Path,
    ) -> Result<(), Box<dyn std::error::Error>> {
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

                        // Skip the maildir special directories and hidden directories
                        if matches!(dir_name, "cur" | "new" | "tmp") || dir_name.starts_with('.') {
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
    pub fn load_folder_emails(
        &self,
        folder: &mut Folder,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.load_folder_emails_with_limit(folder, None)
    }

    /// Load limited number of emails for a specific folder (for fast startup)
    pub fn load_folder_emails_with_limit(
        &self,
        folder: &mut Folder,
        limit: Option<usize>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // If already fully loaded, nothing to do
        if folder.is_loaded {
            return Ok(());
        }

        // If we have a limit and folder already has emails, only proceed if not fully loaded
        if limit.is_some() && !folder.emails.is_empty() {
            return Ok(());
        }

        let path = &folder.path;

        // Check if this is a maildir folder (contains cur/, new/, tmp/)
        let cur_path = path.join("cur");
        let new_path = path.join("new");
        let tmp_path = path.join("tmp");

        let is_maildir = cur_path.exists() && new_path.exists() && tmp_path.exists();

        if is_maildir {
            // Clear existing emails to prevent duplicates
            folder.emails.clear();
            folder.unread_count = 0;
            folder.total_count = 0;

            // This is a maildir folder, scan for emails with optional limit
            self.scan_emails_in_folder_with_limit(folder, &cur_path, limit)?;
            if limit.is_none() || folder.emails.len() < limit.unwrap() {
                let remaining_limit = limit.map(|l| l.saturating_sub(folder.emails.len()));
                self.scan_emails_in_folder_with_limit(folder, &new_path, remaining_limit)?;
            }
        }

        // Only mark as fully loaded if we didn't use a limit
        if limit.is_none() {
            folder.is_loaded = true;
        }
        Ok(())
    }

    /// Scan emails in a specific directory with optional limit (cur or new)
    fn scan_emails_in_folder_with_limit(
        &self,
        folder: &mut Folder,
        dir_path: &Path,
        limit: Option<usize>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if !dir_path.exists() || !dir_path.is_dir() {
            return Ok(()); // Skip if directory doesn't exist
        }

        let mut count = 0;
        for entry in WalkDir::new(dir_path).min_depth(1).max_depth(1) {
            // Check limit early to avoid unnecessary processing
            if let Some(limit) = limit {
                if count >= limit {
                    break;
                }
            }

            let entry = entry?;
            let path = entry.path();

            if path.is_file() {
                // Check if this looks like an email file
                if self.is_email_file(path) {
                    let is_unread = dir_path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map_or(false, |name| name == "new");

                    let mut email = Email::new(path.to_path_buf());
                    email.is_unread = is_unread;

                    // Parse only headers for fast loading
                    match email.parse_headers_only() {
                        Ok(()) => {
                            // Headers parsed successfully
                            folder.add_email(email);
                            count += 1;
                        }
                        Err(e) => {
                            // If parsing fails, create a placeholder with error info
                            email.headers.subject = format!("Parse Error: {}", e);
                            folder.add_email(email);
                            count += 1;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Check if a file looks like an email file
    fn is_email_file(&self, path: &Path) -> bool {
        path.file_name()
            .and_then(|name| name.to_str())
            .map(|filename| {
                !filename.starts_with('.')
                    && !filename.ends_with(".lock")
                    && !filename.ends_with(".tmp")
                    && path.is_file()
            })
            .unwrap_or(false)
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
        let mut result = scanner.scan().unwrap();

        assert_eq!(result.subfolders.len(), 1);
        assert_eq!(result.subfolders[0].name, "INBOX");

        // Load emails for INBOX to test the lazy loading
        scanner
            .load_folder_emails(&mut result.subfolders[0])
            .unwrap();
        assert_eq!(result.subfolders[0].emails.len(), 1);
    }
}
