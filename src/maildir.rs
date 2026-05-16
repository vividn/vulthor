use crate::email::{Email, Folder};
use crate::error::{Result, VulthorError};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Debug, Clone)]
pub struct MaildirScanner {
    root_path: PathBuf,
}

impl MaildirScanner {
    pub fn new(root_path: PathBuf) -> Self {
        Self { root_path }
    }

    /// Scan the MailDir structure and build folder hierarchy (fast startup - structure only)
    pub fn scan(&self) -> Result<Folder> {
        if !self.root_path.exists() {
            return Err(VulthorError::MaildirPathNotFound(self.root_path.clone()));
        }

        if !self.root_path.is_dir() {
            return Err(VulthorError::MaildirPathNotDirectory(
                self.root_path.clone(),
            ));
        }

        let mut root_folder = Folder::new("Mail".to_string(), self.root_path.clone());
        self.scan_folder_structure_only(&mut root_folder, &self.root_path)?;

        Ok(root_folder)
    }

    /// Scan folder structure only (no email loading) for fast startup
    fn scan_folder_structure_only(&self, folder: &mut Folder, path: &Path) -> Result<()> {
        // Look for subfolders only
        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.flatten() {
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

        Ok(())
    }

    /// Load up to `chunk_size` additional emails into a folder that is
    /// already partially loaded. Bounded paged loader for B2 (vu-5jt):
    /// replaces the unbounded `load_folder_emails` call from the scroll-
    /// triggered code path, which used to freeze the TUI on large folders.
    ///
    /// Behavior:
    /// - No-op (returns `Ok(0)`) if the folder is already fully loaded or
    ///   is not a maildir directory.
    /// - Walks `cur/` then `new/`, skipping files already present in
    ///   `folder.emails` (dedup by path).
    /// - Parses headers for at most `chunk_size` new emails per call.
    /// - When a full pass adds zero new emails, the folder is exhausted,
    ///   so we mark `is_loaded = true` to short-circuit future calls.
    ///
    /// Cost is O(dir_size) per call (one WalkDir pass with `is_file`
    /// stats) plus O(chunk_size) parses. For large folders the per-call
    /// latency is bounded by the parse work, not by total folder size.
    pub fn load_more_folder_emails(&self, folder: &mut Folder, chunk_size: usize) -> Result<usize> {
        if folder.is_loaded || chunk_size == 0 {
            return Ok(0);
        }

        let path = &folder.path;
        let cur_path = path.join("cur");
        let new_path = path.join("new");
        let tmp_path = path.join("tmp");
        if !(cur_path.exists() && new_path.exists() && tmp_path.exists()) {
            return Ok(0);
        }

        let loaded: HashSet<PathBuf> = folder.emails.iter().map(|e| e.file_path.clone()).collect();

        let mut budget = chunk_size;
        let cur_added = self.scan_more_in_dir(folder, &cur_path, &loaded, budget)?;
        budget = budget.saturating_sub(cur_added);
        let new_added = if budget > 0 {
            self.scan_more_in_dir(folder, &new_path, &loaded, budget)?
        } else {
            0
        };

        let added = cur_added + new_added;
        // No new emails despite a non-zero budget => folder is exhausted.
        if added == 0 {
            folder.is_loaded = true;
        }
        Ok(added)
    }

    /// Helper for `load_more_folder_emails`: walk `dir_path`, skip entries
    /// whose path is already in `loaded`, parse headers for up to `budget`
    /// new emails, and append them to `folder.emails`. Returns the number
    /// added.
    fn scan_more_in_dir(
        &self,
        folder: &mut Folder,
        dir_path: &Path,
        loaded: &HashSet<PathBuf>,
        budget: usize,
    ) -> Result<usize> {
        if !dir_path.exists() || !dir_path.is_dir() {
            return Ok(0);
        }
        let is_new = dir_path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == "new");

        let mut added = 0;
        for entry in WalkDir::new(dir_path).min_depth(1).max_depth(1) {
            if added >= budget {
                break;
            }
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() || !self.is_email_file(path) {
                continue;
            }
            if loaded.contains(path) {
                continue;
            }

            let mut email = Email::new(path.to_path_buf());
            email.is_unread = is_new;
            match email.parse_headers_only() {
                Ok(()) => {
                    folder.add_email(email);
                    added += 1;
                }
                Err(e) => {
                    email.headers.subject = format!("Parse Error: {}", e);
                    folder.add_email(email);
                    added += 1;
                }
            }
        }
        Ok(added)
    }

    /// Load limited number of emails for a specific folder (for fast startup)
    pub fn load_folder_emails_with_limit(
        &self,
        folder: &mut Folder,
        limit: Option<usize>,
    ) -> Result<()> {
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
    ) -> Result<()> {
        if !dir_path.exists() || !dir_path.is_dir() {
            return Ok(()); // Skip if directory doesn't exist
        }

        let mut count = 0;
        for entry in WalkDir::new(dir_path).min_depth(1).max_depth(1) {
            // Check limit early to avoid unnecessary processing
            if let Some(limit) = limit
                && count >= limit
            {
                break;
            }

            let entry = entry?;
            let path = entry.path();

            if path.is_file() {
                // Check if this looks like an email file
                if self.is_email_file(path) {
                    let is_unread =
                        dir_path.file_name().and_then(|name| name.to_str()) == Some("new");

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
    fn test_maildir_scanner_missing_path_returns_typed_error() {
        let missing = PathBuf::from("/definitely/does/not/exist/maildir");
        let scanner = MaildirScanner::new(missing.clone());

        let err = scanner
            .scan()
            .expect_err("scan of missing path should fail");
        match err {
            VulthorError::MaildirPathNotFound(p) => assert_eq!(p, missing),
            other => panic!("expected MaildirPathNotFound, got {:?}", other),
        }
    }

    #[test]
    fn test_maildir_scanner_path_is_file_returns_typed_error() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("not_a_dir");
        fs::write(&file_path, b"i am a file").unwrap();
        let scanner = MaildirScanner::new(file_path.clone());

        let err = scanner
            .scan()
            .expect_err("scan of non-directory path should fail");
        match err {
            VulthorError::MaildirPathNotDirectory(p) => assert_eq!(p, file_path),
            other => panic!("expected MaildirPathNotDirectory, got {:?}", other),
        }
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

        // Load emails for INBOX to test the lazy loading.
        // `None` limit is the explicit "load every message" mode used
        // here for setup; production code paths now go through the
        // paged `load_more_folder_emails` instead (vu-5jt).
        scanner
            .load_folder_emails_with_limit(&mut result.subfolders[0], None)
            .unwrap();
        assert_eq!(result.subfolders[0].emails.len(), 1);
    }

    /// Build a `cur/`-only INBOX with `n` minimal RFC-822 messages and
    /// return (TempDir, scanner, root Folder). Used by the paged-loader
    /// regression tests below.
    fn build_folder_with_n_emails(n: usize) -> (TempDir, MaildirScanner, Folder) {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join("INBOX/cur")).unwrap();
        fs::create_dir_all(root.join("INBOX/new")).unwrap();
        fs::create_dir_all(root.join("INBOX/tmp")).unwrap();
        for i in 0..n {
            let body = format!(
                "From: a@b.test\r\nTo: c@d.test\r\nSubject: msg {}\r\nDate: Mon, 01 Jan 2024 00:00:00 +0000\r\nMessage-ID: <{}@b.test>\r\n\r\nbody {}\r\n",
                i, i, i
            );
            fs::write(root.join(format!("INBOX/cur/{:06}.eml", i)), body).unwrap();
        }
        let scanner = MaildirScanner::new(root.to_path_buf());
        let result = scanner.scan().unwrap();
        (temp, scanner, result)
    }

    /// vu-5jt acceptance: one call to `load_more_folder_emails` must add
    /// at most `chunk_size` emails, regardless of how many remain in the
    /// folder. Proves the unbounded-load mode is gone from the scroll path.
    #[test]
    fn load_more_folder_emails_is_bounded_by_chunk_size() {
        let (_temp, scanner, mut root) = build_folder_with_n_emails(200);
        let folder = &mut root.subfolders[0];

        // Seed an initial 10 (mirrors what the Enter-into-folder path does).
        scanner
            .load_folder_emails_with_limit(folder, Some(10))
            .unwrap();
        assert_eq!(folder.emails.len(), 10);
        assert!(!folder.is_loaded);

        let added = scanner.load_more_folder_emails(folder, 50).unwrap();
        assert_eq!(added, 50, "must load exactly chunk_size when more remain");
        assert_eq!(folder.emails.len(), 60);
        assert!(
            !folder.is_loaded,
            "folder still has 140 more emails — must not be marked fully loaded",
        );
    }

    /// Repeated paged calls eventually exhaust the folder and flip
    /// `is_loaded = true`, after which further calls are no-ops.
    #[test]
    fn load_more_folder_emails_exhausts_and_marks_loaded() {
        let (_temp, scanner, mut root) = build_folder_with_n_emails(35);
        let folder = &mut root.subfolders[0];
        scanner
            .load_folder_emails_with_limit(folder, Some(10))
            .unwrap();
        assert_eq!(folder.emails.len(), 10);

        // Two chunks of 20 covers the remaining 25 with one short tail.
        let a = scanner.load_more_folder_emails(folder, 20).unwrap();
        let b = scanner.load_more_folder_emails(folder, 20).unwrap();
        let c = scanner.load_more_folder_emails(folder, 20).unwrap();
        assert_eq!(a, 20);
        assert_eq!(b, 5, "last partial chunk = remaining emails");
        assert_eq!(c, 0, "no more emails, returns 0");
        assert_eq!(folder.emails.len(), 35);
        assert!(folder.is_loaded, "exhaustion flips is_loaded");

        // Once is_loaded is true, subsequent calls short-circuit.
        let d = scanner.load_more_folder_emails(folder, 20).unwrap();
        assert_eq!(d, 0);
    }

    /// Dedup: if the same path is already in `folder.emails`, the paged
    /// loader does not double-count it. (Defends against the WalkDir
    /// returning the same entries we already loaded in the seed call.)
    #[test]
    fn load_more_folder_emails_skips_already_loaded_paths() {
        let (_temp, scanner, mut root) = build_folder_with_n_emails(30);
        let folder = &mut root.subfolders[0];

        // Seed with the first 10 (the initial bounded scan).
        scanner
            .load_folder_emails_with_limit(folder, Some(10))
            .unwrap();
        assert_eq!(folder.emails.len(), 10);

        // Request a chunk that exceeds remaining. We must add exactly
        // 20 (the remaining), not 30 (re-adding the seed).
        let added = scanner.load_more_folder_emails(folder, 100).unwrap();
        assert_eq!(added, 20);
        assert_eq!(folder.emails.len(), 30);

        // No duplicates: every file_path appears once.
        let mut paths: Vec<_> = folder.emails.iter().map(|e| e.file_path.clone()).collect();
        paths.sort();
        let before = paths.len();
        paths.dedup();
        assert_eq!(paths.len(), before, "no duplicate paths after paged load");
    }
}
