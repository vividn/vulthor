use crate::error::{Result, VulthorError};
use mail_parser::{Message, MessageParser, MimeHeaders};
use std::fs;
use std::path::{Path, PathBuf};

/// True when the MailDir info-flags suffix (`:2,…`) of the path's
/// filename contains `flag`. Returns false on non-UTF-8 names or paths
/// without a `:2,` suffix — both are safe defaults for "not flagged".
pub fn maildir_flag_in_filename(path: &Path, flag: char) -> bool {
    path.file_name()
        .and_then(|s| s.to_str())
        .and_then(|name| name.split_once(":2,").map(|(_, flags)| flags.to_string()))
        .map(|flags| flags.contains(flag))
        .unwrap_or(false)
}

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

/// Source and destination paths for a `new/`→`cur/` mark-read move
/// (vu-rxi). Built by `EmailStore::plan_mark_read`; consumed by the
/// AppRoot handler for `Msg::MessageMarkRead`, which performs the
/// `fs::rename` and then calls `update_email_read_state` so the
/// in-memory store stays consistent with disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkReadPlan {
    pub from: PathBuf,
    pub to: PathBuf,
}

/// Translate a Maildir `…/<folder>/new/<name>` path into its
/// `…/<folder>/cur/<name>` sibling. Returns `None` when the parent
/// directory is not literally `new`, so callers naturally no-op for
/// emails that are already in `cur/`.
fn derive_cur_path(p: &std::path::Path) -> Option<PathBuf> {
    let name = p.file_name()?;
    let parent = p.parent()?;
    if parent.file_name()? != "new" {
        return None;
    }
    Some(parent.parent()?.join("cur").join(name))
}

#[derive(Debug, Clone)]
pub struct Email {
    pub headers: EmailHeaders,
    pub body_text: String,
    pub body_html: Option<String>,
    pub attachments: Vec<Attachment>,
    pub file_path: PathBuf,
    pub is_unread: bool,
    /// MailDir `F` (Flagged) info flag — `s` toggles this, undo reverses
    /// it. Mirror of the on-disk filename's `:2,…F…` suffix; the
    /// MailDir scanner seeds it, the action-key handler updates it in
    /// lockstep with the file rename. See VISION.md § "Action
    /// Keybindings" (`s` / `F`) and `crate::undo::Mutation::ToggleStar`.
    pub is_flagged: bool,
    pub load_state: EmailLoadState,
}

impl Email {
    pub fn new(file_path: PathBuf) -> Self {
        let is_flagged = maildir_flag_in_filename(&file_path, 'F');
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
            is_flagged,
            load_state: EmailLoadState::HeadersOnly,
        }
    }

    /// Parse only headers from file (fast for folder loading)
    pub fn parse_headers_only(&mut self) -> Result<()> {
        let content = fs::read(&self.file_path)?;
        let message = MessageParser::default()
            .parse(&content)
            .ok_or(VulthorError::MailParser)?;

        self.parse_headers(&message)?;
        self.load_state = EmailLoadState::HeadersOnly;

        Ok(())
    }

    /// Parse email from file (full parsing for reading)
    pub fn parse_from_file(&mut self) -> Result<()> {
        let content = fs::read(&self.file_path)?;
        let message = MessageParser::default()
            .parse(&content)
            .ok_or(VulthorError::MailParser)?;

        self.parse_headers(&message)?;
        self.parse_body(&message)?;
        self.load_state = EmailLoadState::FullyLoaded;

        Ok(())
    }

    /// Ensure email is fully loaded
    pub fn ensure_fully_loaded(&mut self) -> Result<()> {
        match self.load_state {
            EmailLoadState::HeadersOnly => self.parse_from_file(),
            EmailLoadState::FullyLoaded => Ok(()),
        }
    }

    /// Parse email headers elegantly using mail-parser
    fn parse_headers(&mut self, message: &Message) -> Result<()> {
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
    fn parse_body(&mut self, message: &Message) -> Result<()> {
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
    fn extract_attachments(&mut self, message: &Message) -> Result<()> {
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
    /// True while the initial off-thread folder-structure scan is in
    /// flight (Phase 0.3.4, vu-w9i). The folder pane uses this to
    /// render a "Scanning folders…" splash instead of an empty list.
    /// Flips to false when `AppRoot` reaps the scanner reply.
    pub scanning_folders: bool,
}

impl EmailStore {
    pub fn new(maildir_path: PathBuf) -> Self {
        Self {
            root_folder: Folder::new("Mail".to_string(), maildir_path),
            current_folder: Vec::new(),
            selected_email: None,
            scanning_folders: false,
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
    ) -> Result<()> {
        let mut folder = &mut self.root_folder;
        for &index in path {
            if index < folder.subfolders.len() {
                folder = &mut folder.subfolders[index];
            } else {
                return Err(VulthorError::InvalidFolderPath);
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
    ) -> Result<()> {
        let folder = self.get_current_folder_mut();
        // Only load if the folder has no emails or is not loaded
        if !folder.is_loaded && folder.emails.is_empty() {
            scanner.load_folder_emails_with_limit(folder, Some(limit))?;
        }
        Ok(())
    }

    /// Load more messages if current folder is not fully loaded.
    ///
    /// Called on each `j` scroll in the Messages pane. The previous
    /// implementation called `scanner.load_folder_emails(folder)` with no
    /// limit, which on a 50k-message archive folder could freeze the TUI
    /// for tens of seconds (vu-5jt / AUDIT-BLOCKING-IO.md §B2).
    ///
    /// The paged loader loads at most `SCROLL_LOAD_CHUNK` headers per
    /// call, so per-scroll latency is bounded by `chunk × per-message
    /// parse cost`, not by total folder size. Repeated scrolls within
    /// the unloaded tail trigger repeated chunks; when the folder is
    /// exhausted, `load_more_folder_emails` flips `is_loaded` and this
    /// becomes a cheap branch.
    pub fn load_more_messages_if_needed(
        &mut self,
        scanner: &crate::maildir::MaildirScanner,
        index: usize,
    ) -> Result<()> {
        const SCROLL_LOAD_CHUNK: usize = 50;
        let folder = self.get_current_folder_mut();
        if !folder.is_loaded && index + 5 >= folder.emails.len() {
            scanner.load_more_folder_emails(folder, SCROLL_LOAD_CHUNK)?;
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

    /// Get currently selected email (non-blocking — returns whatever state the email is in).
    /// Body-loading happens off the render thread via the body-loader worker; callers that
    /// need a guaranteed-loaded email use `get_selected_email_mut().ensure_fully_loaded()`.
    pub fn get_selected_email(&self) -> Option<&Email> {
        let current = self.get_current_folder();
        self.selected_email
            .and_then(|index| current.emails.get(index))
    }

    /// Alias retained for clarity at call sites that explicitly want "headers only".
    pub fn get_selected_email_headers(&self) -> Option<&Email> {
        self.get_selected_email()
    }

    /// Resolve the email the web pane should serve given the TUI's currently
    /// focused pane. Returns `None` for top-level browse panes (Folders,
    /// Accounts) so the web pane shows the welcome screen instead.
    ///
    /// `vu-9ie` (Phase 0.3.5, D1-D3): non-blocking. Previously this called
    /// `ensure_fully_loaded` on the selected email while holding the
    /// `Mutex<EmailStore>`, performing `fs::read` + full MIME parse on the
    /// axum executor thread *and* stalling the TUI render loop for the
    /// duration. Body loading is now the `BodyLoader` worker's job; the web
    /// handler observes whatever state the email is in and (separately)
    /// kicks the loader. SSE refires on `load_state` change so the client
    /// refetches once the body lands.
    pub fn current_email_for_web(&self, pane: crate::layout::ActivePane) -> Option<&Email> {
        if !pane.serves_email() {
            return None;
        }
        self.get_selected_email()
    }

    /// Get currently selected email mutably
    pub fn get_selected_email_mut(&mut self) -> Option<&mut Email> {
        let selected = self.selected_email;
        let current = self.get_current_folder_mut();
        selected.and_then(move |index| current.emails.get_mut(index))
    }

    /// Get markdown content for the currently selected email (non-blocking).
    /// Returns the in-memory `body_text`; empty while the email is still
    /// `HeadersOnly`. The UI checks `load_state` to show a "Loading body…"
    /// placeholder in that window.
    pub fn get_selected_email_markdown(&self) -> Option<String> {
        self.get_selected_email().map(|e| e.body_text.clone())
    }

    /// Apply a body load result (from the off-thread body loader) to the
    /// matching email anywhere in the folder tree. Returns true if an email
    /// matched and was updated. The match key is `file_path` so late
    /// responses still find the email after the user navigates away.
    pub fn apply_loaded_body(
        &mut self,
        path: &std::path::Path,
        body_text: String,
        body_html: Option<String>,
        attachments: Vec<Attachment>,
    ) -> bool {
        let mut payload = Some((body_text, body_html, attachments));
        Self::apply_loaded_body_to_folder(&mut self.root_folder, path, &mut payload)
    }

    /// Apply a folder-headers load result (from the off-thread headers loader)
    /// to the folder anywhere in the tree whose filesystem path matches
    /// `fs_path`. Returns true if a folder was found.
    ///
    /// Late replies that arrive after the user has already loaded the folder
    /// some other way are dropped (no overwrite of `is_loaded` or `emails`).
    /// `fully_loaded = true` flips `Folder::is_loaded` so AppRoot stops
    /// re-requesting on every selection change.
    pub fn apply_loaded_folder(
        &mut self,
        fs_path: &std::path::Path,
        emails: Vec<Email>,
        fully_loaded: bool,
    ) -> bool {
        let mut payload = Some(emails);
        Self::apply_loaded_folder_to(&mut self.root_folder, fs_path, &mut payload, fully_loaded)
    }

    fn apply_loaded_folder_to(
        folder: &mut Folder,
        fs_path: &std::path::Path,
        payload: &mut Option<Vec<Email>>,
        fully_loaded: bool,
    ) -> bool {
        if folder.path == fs_path {
            if let Some(emails) = payload.take() {
                // Drop the reply when the folder already has headers — either
                // a synchronous fallback path loaded it, or a previous reply
                // already landed. Either way, we don't want to clobber it.
                if !folder.is_loaded && folder.emails.is_empty() {
                    let unread = emails.iter().filter(|e| e.is_unread).count();
                    let total = emails.len();
                    folder.emails = emails;
                    folder.unread_count = unread;
                    folder.total_count = total;
                }
                if fully_loaded {
                    folder.is_loaded = true;
                }
            }
            return true;
        }
        for sub in &mut folder.subfolders {
            if Self::apply_loaded_folder_to(sub, fs_path, payload, fully_loaded) {
                return true;
            }
        }
        false
    }

    /// Rewrite a single email's `file_path` after the file has moved on
    /// disk (action-key handlers + `Msg::Undo`, vu-pas). Walks the
    /// folder tree and updates the first matching email; returns true
    /// if an email was found. Counts are not touched — moves between
    /// folders should also update each folder's `total_count` /
    /// `unread_count`, but that's the responsibility of the higher-level
    /// "move email across folders" path that Phase 1.b–1.e will add.
    pub fn swap_email_path(&mut self, old: &std::path::Path, new: &std::path::Path) -> bool {
        Self::swap_email_path_in_folder(&mut self.root_folder, old, new)
    }

    /// Plan a mark-read transition for the email at `email_index` in the
    /// current folder (vu-rxi, Phase 1.b). Returns `None` when the index
    /// is out of range, the email is already read, or the file path is
    /// not under a `new/` directory — making this method the single
    /// idempotency gate for `Enter (auto mark-read)`.
    pub fn plan_mark_read(&self, email_index: usize) -> Option<MarkReadPlan> {
        let folder = self.get_current_folder();
        let email = folder.emails.get(email_index)?;
        if !email.is_unread {
            return None;
        }
        let to = derive_cur_path(&email.file_path)?;
        if to == email.file_path {
            return None;
        }
        Some(MarkReadPlan {
            from: email.file_path.clone(),
            to,
        })
    }

    /// Apply the in-memory side of a read-state flip after the
    /// filesystem rename has succeeded. Finds the email by its
    /// `current_path`, rewrites it to `new_path`, sets `is_unread`, and
    /// adjusts the containing folder's `unread_count` only when the
    /// flag actually changes. Returns true on match.
    pub fn update_email_read_state(
        &mut self,
        current_path: &std::path::Path,
        new_path: &std::path::Path,
        new_is_unread: bool,
    ) -> bool {
        Self::update_email_read_state_in_folder(
            &mut self.root_folder,
            current_path,
            new_path,
            new_is_unread,
        )
    }

    fn update_email_read_state_in_folder(
        folder: &mut Folder,
        current_path: &std::path::Path,
        new_path: &std::path::Path,
        new_is_unread: bool,
    ) -> bool {
        for email in &mut folder.emails {
            if email.file_path == current_path {
                if email.is_unread != new_is_unread {
                    if new_is_unread {
                        folder.unread_count += 1;
                    } else {
                        folder.unread_count = folder.unread_count.saturating_sub(1);
                    }
                    email.is_unread = new_is_unread;
                }
                email.file_path = new_path.to_path_buf();
                return true;
            }
        }
        for sub in &mut folder.subfolders {
            if Self::update_email_read_state_in_folder(sub, current_path, new_path, new_is_unread) {
                return true;
            }
        }
        false
    }

    fn swap_email_path_in_folder(
        folder: &mut Folder,
        old: &std::path::Path,
        new: &std::path::Path,
    ) -> bool {
        for email in &mut folder.emails {
            if email.file_path == old {
                email.file_path = new.to_path_buf();
                // `is_flagged` is the in-memory mirror of the on-disk
                // `:2,…F…` flag; refresh it so Phase 1.c star-toggle
                // and its undo stay coherent without a separate writer.
                email.is_flagged = maildir_flag_in_filename(new, 'F');
                return true;
            }
        }
        for sub in &mut folder.subfolders {
            if Self::swap_email_path_in_folder(sub, old, new) {
                return true;
            }
        }
        false
    }

    fn apply_loaded_body_to_folder(
        folder: &mut Folder,
        path: &std::path::Path,
        payload: &mut Option<(String, Option<String>, Vec<Attachment>)>,
    ) -> bool {
        for email in &mut folder.emails {
            if email.file_path == path {
                if let Some((body_text, body_html, attachments)) = payload.take() {
                    email.body_text = body_text;
                    email.body_html = body_html;
                    email.attachments = attachments;
                    email.load_state = EmailLoadState::FullyLoaded;
                }
                return true;
            }
        }
        for sub in &mut folder.subfolders {
            if Self::apply_loaded_body_to_folder(sub, path, payload) {
                return true;
            }
        }
        false
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
        let email_files: Vec<_> = fs::read_dir(&inbox_path)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().is_file())
            .collect();

        assert!(
            !email_files.is_empty(),
            "No email files found in test INBOX"
        );

        let email_path = email_files[0].path();
        let mut email = Email::new(email_path);

        // Parse headers only
        let result = email.parse_headers_only();
        assert!(
            result.is_ok(),
            "Failed to parse email headers: {:?}",
            result
        );

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
        let email_files: Vec<_> = fs::read_dir(&inbox_path)
            .unwrap()
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

        let email_files: Vec<_> = fs::read_dir(&inbox_path)
            .unwrap()
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
        let email_files: Vec<_> = fs::read_dir(&inbox_path)
            .unwrap()
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

        assert!(
            attachment_email.is_some(),
            "No attachment email found in test data"
        );

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
        let email_files: Vec<_> = fs::read_dir(&inbox_path)
            .unwrap()
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

        let email_files: Vec<_> = fs::read_dir(&inbox_path)
            .unwrap()
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
        folder.add_subfolder(Folder::new(
            "Zebra".to_string(),
            temp_dir.path().join("zebra"),
        ));
        folder.add_subfolder(Folder::new(
            "INBOX".to_string(),
            temp_dir.path().join("inbox"),
        ));
        folder.add_subfolder(Folder::new(
            "Alpha".to_string(),
            temp_dir.path().join("alpha"),
        ));

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
        // `None` limit = explicit "load every message" mode for test
        // setup; production scroll path uses paged loads (vu-5jt).
        scanner
            .load_folder_emails_with_limit(store.get_current_folder_mut(), None)
            .unwrap();

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

    /// vu-5jt acceptance: a single `load_more_messages_if_needed` call
    /// must NOT fully load a large folder. Pre-fix this was unbounded
    /// and froze the TUI on big archives (AUDIT-BLOCKING-IO.md §B2).
    /// After the fix, one j-scroll trigger loads at most one chunk
    /// (SCROLL_LOAD_CHUNK = 50) of additional headers.
    #[test]
    fn load_more_messages_if_needed_is_bounded_per_call() {
        use crate::maildir::MaildirScanner;

        // Build a 200-message INBOX directly so we don't rely on the
        // test fixture's curated content.
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join("INBOX/cur")).unwrap();
        fs::create_dir_all(root.join("INBOX/new")).unwrap();
        fs::create_dir_all(root.join("INBOX/tmp")).unwrap();
        for i in 0..200 {
            let body = format!(
                "From: a@b.test\r\nTo: c@d.test\r\nSubject: msg {}\r\nDate: Mon, 01 Jan 2024 00:00:00 +0000\r\nMessage-ID: <{}@b.test>\r\n\r\nb\r\n",
                i, i
            );
            fs::write(root.join(format!("INBOX/cur/{:06}.eml", i)), body).unwrap();
        }

        let scanner = MaildirScanner::new(root.to_path_buf());
        let mut store = EmailStore::new(root.to_path_buf());
        store.root_folder = scanner.scan().unwrap();
        store.enter_folder_by_path(&[0]); // INBOX

        // Seed initial bounded load (mirrors the Enter-into-folder path).
        store
            .ensure_current_folder_loaded_with_limit(&scanner, 10)
            .unwrap();
        let initial = store.get_current_folder().emails.len();
        assert_eq!(initial, 10);

        // Simulate one j-scroll at the loaded tail: index near
        // emails.len() triggers load_more_messages_if_needed.
        store
            .load_more_messages_if_needed(&scanner, initial - 1)
            .unwrap();

        let after = store.get_current_folder().emails.len();
        assert!(
            after > initial,
            "should have loaded at least one chunk past the seed",
        );
        assert!(
            after < 200,
            "must NOT have fully loaded the folder (was {}, total 200)",
            after,
        );
        assert!(
            after <= initial + 50,
            "one scroll trigger must load at most SCROLL_LOAD_CHUNK=50 more (got {} new)",
            after - initial,
        );
        assert!(
            !store.get_current_folder().is_loaded,
            "partial load must not mark folder fully loaded",
        );
    }

    /// Phase 0.3.2 (vu-6td) acceptance: the render-path getters never touch
    /// the disk. We point an `Email` at a path that does not exist; if the
    /// getters still called `parse_from_file`, the email would either error
    /// or, in the old code, the call would block on a filesystem stat. Here
    /// we verify the `load_state` stays `HeadersOnly` and the returned body
    /// is empty (since nothing has parsed it yet).
    #[test]
    fn render_path_getters_do_not_load_body() {
        let mut store = EmailStore::new(PathBuf::from("/nonexistent_root"));
        let mut inbox = Folder::new(
            "INBOX".to_string(),
            PathBuf::from("/nonexistent_root/INBOX"),
        );
        inbox.add_email(Email::new(PathBuf::from(
            "/definitely/does/not/exist/email.eml",
        )));
        inbox.is_loaded = true;
        store.root_folder.add_subfolder(inbox);

        store.enter_folder_by_path(&[0]);
        store.select_email(0);

        // Render-path: get_selected_email_markdown. Must return Some("") and
        // must NOT transition the email to FullyLoaded.
        let markdown = store.get_selected_email_markdown();
        assert_eq!(markdown.as_deref(), Some(""));
        let email = store.get_selected_email().expect("email is selected");
        assert!(
            matches!(email.load_state, EmailLoadState::HeadersOnly),
            "render-path getter must not transition load_state",
        );

        // Render-path: get_selected_email (attachments pane). Same contract.
        let email = store.get_selected_email().expect("email is selected");
        assert!(email.attachments.is_empty());
        assert!(matches!(email.load_state, EmailLoadState::HeadersOnly));
    }

    /// Phase 0.3.2 (vu-6td): `apply_loaded_body` writes a parsed body back
    /// into the store and transitions the email to FullyLoaded. The match is
    /// by file path so late responses still land after the user navigates.
    #[test]
    fn apply_loaded_body_updates_email_state() {
        let mut store = EmailStore::new(PathBuf::from("/tmp"));
        let path = PathBuf::from("/tmp/some/email.eml");
        let mut inbox = Folder::new("INBOX".to_string(), PathBuf::from("/tmp/INBOX"));
        inbox.add_email(Email::new(path.clone()));
        inbox.is_loaded = true;
        store.root_folder.add_subfolder(inbox);

        store.enter_folder_by_path(&[0]);
        store.select_email(0);

        let attachments = vec![Attachment {
            filename: "doc.pdf".to_string(),
            content_type: "application/pdf".to_string(),
            size: 1024,
        }];
        let applied = store.apply_loaded_body(
            &path,
            "body text".to_string(),
            Some("<p>body</p>".to_string()),
            attachments,
        );
        assert!(applied, "apply_loaded_body must find the email by path");

        let email = store.get_selected_email().unwrap();
        assert_eq!(email.body_text, "body text");
        assert_eq!(email.body_html.as_deref(), Some("<p>body</p>"));
        assert_eq!(email.attachments.len(), 1);
        assert!(matches!(email.load_state, EmailLoadState::FullyLoaded));
    }

    /// Phase 0.3.3 (vu-kx9): `apply_loaded_folder` writes the worker's
    /// header batch into the matching subfolder and flips `is_loaded` when
    /// the worker reported a fully-scanned folder. Counts (`unread_count`,
    /// `total_count`) are derived from the new email list.
    #[test]
    fn apply_loaded_folder_writes_emails_to_matching_subfolder() {
        let mut store = EmailStore::new(PathBuf::from("/tmp/mail"));
        let inbox_path = PathBuf::from("/tmp/mail/INBOX");
        let inbox = Folder::new("INBOX".to_string(), inbox_path.clone());
        store.root_folder.add_subfolder(inbox);

        let mut a = Email::new(PathBuf::from("/tmp/mail/INBOX/cur/a"));
        a.is_unread = true;
        let b = Email::new(PathBuf::from("/tmp/mail/INBOX/cur/b"));
        let applied = store.apply_loaded_folder(&inbox_path, vec![a, b], false);
        assert!(applied);

        let inbox = &store.root_folder.subfolders[0];
        assert_eq!(inbox.emails.len(), 2);
        assert_eq!(inbox.unread_count, 1);
        assert_eq!(inbox.total_count, 2);
        assert!(
            !inbox.is_loaded,
            "fully_loaded=false must leave is_loaded untouched (partial load)",
        );

        // A second reply with fully_loaded=true must flip is_loaded without
        // overwriting the existing email list (the "already populated"
        // short-circuit prevents lost state on a stale late reply).
        let applied2 = store.apply_loaded_folder(&inbox_path, Vec::new(), true);
        assert!(applied2);
        let inbox = &store.root_folder.subfolders[0];
        assert_eq!(inbox.emails.len(), 2, "must not clobber existing headers");
        assert!(inbox.is_loaded);
    }

    /// Phase 0.3.3 (vu-kx9): replies for unknown filesystem paths (e.g. the
    /// folder was removed while loading) return `false` so AppRoot can drop
    /// the reply without panicking.
    #[test]
    fn apply_loaded_folder_returns_false_for_unknown_path() {
        let mut store = EmailStore::new(PathBuf::from("/tmp"));
        store
            .root_folder
            .add_subfolder(Folder::new("a".to_string(), PathBuf::from("/tmp/a")));
        let applied = store.apply_loaded_folder(&PathBuf::from("/tmp/b"), Vec::new(), true);
        assert!(!applied);
    }

    /// Phase 0.3.2 (vu-6td): `apply_loaded_body` returns `false` when no
    /// email matches the path — covers the "user navigated and the email is
    /// gone" race so the worker's reply is dropped cleanly.
    #[test]
    fn apply_loaded_body_returns_false_for_unknown_path() {
        let mut store = EmailStore::new(PathBuf::from("/tmp"));
        let mut inbox = Folder::new("INBOX".to_string(), PathBuf::from("/tmp/INBOX"));
        inbox.add_email(Email::new(PathBuf::from("/tmp/a")));
        store.root_folder.add_subfolder(inbox);

        let applied =
            store.apply_loaded_body(&PathBuf::from("/tmp/b"), "x".to_string(), None, Vec::new());
        assert!(!applied);
    }

    // --- vu-rxi (Phase 1.b): mark-read planning + in-memory state. ---

    fn store_with_unread_in_new(root: PathBuf) -> EmailStore {
        let mut store = EmailStore::new(root.clone());
        let mut inbox = Folder::new("INBOX".to_string(), root.join("INBOX"));
        let mut email = Email::new(root.join("INBOX/new/msg1"));
        email.is_unread = true;
        inbox.add_email(email);
        inbox.is_loaded = true;
        store.root_folder.add_subfolder(inbox);
        store.enter_folder_by_path(&[0]);
        store
    }

    #[test]
    fn plan_mark_read_returns_some_for_unread_email_in_new_dir() {
        let store = store_with_unread_in_new(PathBuf::from("/tmp/mr"));
        let plan = store.plan_mark_read(0).expect("plan exists");
        assert_eq!(plan.from, PathBuf::from("/tmp/mr/INBOX/new/msg1"));
        assert_eq!(plan.to, PathBuf::from("/tmp/mr/INBOX/cur/msg1"));
    }

    #[test]
    fn plan_mark_read_returns_none_when_already_read() {
        let mut store = store_with_unread_in_new(PathBuf::from("/tmp/mr"));
        store.get_current_folder_mut().emails[0].is_unread = false;
        assert!(store.plan_mark_read(0).is_none());
    }

    #[test]
    fn plan_mark_read_returns_none_when_index_out_of_range() {
        let store = store_with_unread_in_new(PathBuf::from("/tmp/mr"));
        assert!(store.plan_mark_read(99).is_none());
    }

    #[test]
    fn plan_mark_read_returns_none_for_email_not_in_new_dir() {
        // is_unread=true but file_path is already under cur/. Defensive:
        // mbsync may set seen-state via flags only; we still gate on the
        // directory because the move is the operation we know how to do.
        let mut store = EmailStore::new(PathBuf::from("/tmp/mr"));
        let mut inbox = Folder::new("INBOX".to_string(), PathBuf::from("/tmp/mr/INBOX"));
        let mut email = Email::new(PathBuf::from("/tmp/mr/INBOX/cur/msg1"));
        email.is_unread = true;
        inbox.add_email(email);
        store.root_folder.add_subfolder(inbox);
        store.enter_folder_by_path(&[0]);
        assert!(store.plan_mark_read(0).is_none());
    }

    #[test]
    fn update_email_read_state_flips_unread_and_swaps_path_and_decrements_count() {
        let mut store = store_with_unread_in_new(PathBuf::from("/tmp/mr"));
        let before = store.get_current_folder().unread_count;
        let found = store.update_email_read_state(
            &PathBuf::from("/tmp/mr/INBOX/new/msg1"),
            &PathBuf::from("/tmp/mr/INBOX/cur/msg1"),
            false,
        );
        assert!(found);
        let folder = store.get_current_folder();
        assert_eq!(folder.unread_count, before - 1);
        assert!(!folder.emails[0].is_unread);
        assert_eq!(
            folder.emails[0].file_path,
            PathBuf::from("/tmp/mr/INBOX/cur/msg1")
        );
    }

    #[test]
    fn update_email_read_state_to_unread_increments_count_and_swaps_path() {
        // Inverse direction used by undo: file went cur/→new/, restore
        // is_unread and bump the count.
        let mut store = EmailStore::new(PathBuf::from("/tmp/mr"));
        let mut inbox = Folder::new("INBOX".to_string(), PathBuf::from("/tmp/mr/INBOX"));
        inbox.add_email(Email::new(PathBuf::from("/tmp/mr/INBOX/cur/msg1")));
        store.root_folder.add_subfolder(inbox);
        store.enter_folder_by_path(&[0]);

        let found = store.update_email_read_state(
            &PathBuf::from("/tmp/mr/INBOX/cur/msg1"),
            &PathBuf::from("/tmp/mr/INBOX/new/msg1"),
            true,
        );
        assert!(found);
        let folder = store.get_current_folder();
        assert_eq!(folder.unread_count, 1);
        assert!(folder.emails[0].is_unread);
        assert_eq!(
            folder.emails[0].file_path,
            PathBuf::from("/tmp/mr/INBOX/new/msg1"),
        );
    }

    #[test]
    fn update_email_read_state_idempotent_when_flag_unchanged() {
        // Calling twice with the same `new_is_unread` must not
        // double-decrement the unread count (would underflow without
        // the saturating sub guard).
        let mut store = store_with_unread_in_new(PathBuf::from("/tmp/mr"));
        let first = PathBuf::from("/tmp/mr/INBOX/new/msg1");
        let second = PathBuf::from("/tmp/mr/INBOX/cur/msg1");
        store.update_email_read_state(&first, &second, false);
        let after_first = store.get_current_folder().unread_count;
        // Repeat with the same target state — only the path-swap is real work.
        store.update_email_read_state(&second, &second, false);
        assert_eq!(store.get_current_folder().unread_count, after_first);
    }

    #[test]
    fn update_email_read_state_returns_false_for_unknown_path() {
        let mut store = store_with_unread_in_new(PathBuf::from("/tmp/mr"));
        assert!(!store.update_email_read_state(
            &PathBuf::from("/no/such/msg"),
            &PathBuf::from("/no/such/elsewhere"),
            false,
        ));
    }
}
