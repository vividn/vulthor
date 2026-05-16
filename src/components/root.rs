// `AppRoot` — main-loop driver, sole owner of the TUI state.
//
// Phase 0.2.5 (vu-7r1): the legacy `App` god object is gone. AppRoot now
// owns `Layout`, `status_message`, `should_quit`, etc. directly. The
// `EmailStore` is the only thing shared across the TUI ↔ web boundary
// (`Arc<Mutex<EmailStore>>`), and the focused pane travels via
// `Arc<AtomicU8>` so the web server can decide between serving the
// selected email and the welcome screen without locking the store.
//
// Components remain canonical for the slice of state they own:
//  - `FoldersComponent.folder_index` (mirrored into `layout.selection.folder_index`)
//  - `MessagesComponent.email_index` (mirrored into `layout.selection.email_index`)
//  - `ContentComponent.scroll_offset` (mirrored into `layout.selection.scroll_offset`)
//
// Backspace from Content/Attachments and attachment-pane navigation are
// the last bits of legacy keymap behavior; AppRoot handles them inline
// rather than introducing a tiny component for each.

use std::collections::{HashSet, VecDeque};
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::config::Config;
use crate::email::{EmailLoadState, EmailStore};
use crate::error::Result;
use crate::layout::{self, ActivePane, Layout, PaneSwitchDirection, View};
use crate::maildir::MaildirScanner;
use crate::theme::VulthorTheme;
use crate::ui::UI;

use super::{
    AccountsComponent, BodyLoader, Component, ContentComponent, Ctx, DraftComponent,
    FolderScannerHandle, FoldersComponent, HeadersLoader, LoadFolderRequest, MAX_DISPATCH_DEPTH,
    MessagesComponent, Msg,
};

pub struct AppRoot {
    /// The single shared resource — the only `Arc<Mutex<_>>` left after
    /// vu-7r1. The web server reads it; the TUI thread writes it under
    /// the same lock during dispatch.
    email_store: Arc<Mutex<EmailStore>>,
    /// Published to the web server so it can answer "serve email vs.
    /// welcome" without locking the store. Updated on every focus
    /// change via `Msg::FocusChanged`.
    focused_pane: Arc<AtomicU8>,

    scanner: MaildirScanner,
    layout: Layout,
    status_message: Option<String>,
    should_quit: bool,
    /// Replaces the legacy `AppState::Help` flag; toggled by '?'.
    help_visible: bool,
    /// Updated by the Messages pane during render; used to size
    /// off-thread header loads.
    message_pane_visible_rows: usize,

    folders: FoldersComponent,
    messages: MessagesComponent,
    content: ContentComponent,
    accounts: AccountsComponent,
    draft: DraftComponent,
    queue: VecDeque<Msg>,
    body_loader: BodyLoader,
    loading_paths: HashSet<PathBuf>,
    folder_scanner: Option<FolderScannerHandle>,
    headers_loader: HeadersLoader,
    loading_folder_paths: HashSet<PathBuf>,
}

impl AppRoot {
    pub fn new(email_store: Arc<Mutex<EmailStore>>, scanner: MaildirScanner) -> Self {
        let initial_index = {
            let store = email_store.lock().unwrap();
            FoldersComponent::auto_select_inbox(&store.root_folder)
        };
        let mut layout = Layout::new();
        layout.selection.folder_index = initial_index;

        let mut root = Self {
            email_store: email_store.clone(),
            focused_pane: Arc::new(AtomicU8::new(ActivePane::Folders.to_u8())),
            scanner: scanner.clone(),
            layout,
            status_message: None,
            should_quit: false,
            help_visible: false,
            message_pane_visible_rows: 20,
            folders: FoldersComponent::with_index(initial_index),
            messages: MessagesComponent::new(),
            content: ContentComponent::new(),
            accounts: AccountsComponent::new(),
            draft: DraftComponent::new(),
            queue: VecDeque::new(),
            body_loader: BodyLoader::spawn(),
            loading_paths: HashSet::new(),
            folder_scanner: None,
            headers_loader: HeadersLoader::spawn(scanner),
            loading_folder_paths: HashSet::new(),
        };

        // Pre-fetch the auto-selected folder's headers off-thread so the
        // first frame doesn't have to block on disk. No-op when the
        // tree is still empty (scanner has not replied yet).
        let indices = {
            let store = email_store.lock().unwrap();
            layout::get_folder_path_from_display_index(&store.root_folder, initial_index)
        };
        if let Some(indices) = indices {
            root.request_folder_load_if_needed(&indices);
        }
        root
    }

    /// Hand the root the off-thread folder scanner started in `main`.
    pub fn attach_folder_scanner(&mut self, handle: FolderScannerHandle) {
        self.folder_scanner = Some(handle);
    }

    /// Clone of the focused-pane signal the web server reads.
    pub fn focused_pane(&self) -> Arc<AtomicU8> {
        self.focused_pane.clone()
    }

    /// Clone of the body-loader request channel. The web server uses this to
    /// dispatch body parses to the same off-thread worker the TUI feeds, so
    /// no `fs::read` ever runs on an axum executor thread while holding the
    /// store lock (`vu-9ie`, D1-D3).
    pub fn body_request_sender(&self) -> std::sync::mpsc::Sender<PathBuf> {
        self.body_loader.request_sender()
    }

    /// Clone of the email-store handle. Tests and callers that want a
    /// post-dispatch peek at the store use this.
    pub fn email_store_handle(&self) -> Arc<Mutex<EmailStore>> {
        self.email_store.clone()
    }

    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    /// Enqueue a message for the next dispatch cycle.
    pub fn enqueue(&mut self, msg: Msg) {
        self.queue.push_back(msg);
    }

    pub fn queue_len(&self) -> usize {
        self.queue.len()
    }

    pub fn folders(&self) -> &FoldersComponent {
        &self.folders
    }
    pub fn messages(&self) -> &MessagesComponent {
        &self.messages
    }
    pub fn content(&self) -> &ContentComponent {
        &self.content
    }
    pub fn accounts(&self) -> &AccountsComponent {
        &self.accounts
    }
    pub fn draft(&self) -> &DraftComponent {
        &self.draft
    }

    /// Render one frame. Drains async replies first, then delegates to
    /// `ui::UI::draw` with borrowed state. Returns whether the loop
    /// should exit.
    pub fn render(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        ui: &mut UI,
    ) -> Result<bool> {
        self.drain_scanned_folders();
        self.drain_loaded_bodies();
        self.drain_loaded_folders();
        self.request_body_if_needed();

        let store_arc = self.email_store.clone();
        let mut store = store_arc.lock().unwrap();
        let folders = &self.folders;
        let messages = &self.messages;
        let content = &self.content;
        let accounts = &self.accounts;
        let draft = &self.draft;
        let layout = &self.layout;
        let status = &self.status_message;
        let help = self.help_visible;
        terminal.draw(|f| {
            ui.draw(
                f, &mut store, layout, status, help, folders, messages, content, accounts, draft,
            )
        })?;
        self.message_pane_visible_rows = self.messages.visible_rows.get();
        Ok(self.should_quit)
    }

    /// Poll for an input event and process it.
    pub fn tick(&mut self) -> Result<bool> {
        self.drain_scanned_folders();
        self.drain_loaded_bodies();
        self.drain_loaded_folders();
        if !event::poll(Duration::from_millis(100))? {
            return Ok(false);
        }
        let event = event::read()?;
        self.process_event(event)
    }

    /// Apply a single input event.
    pub fn process_event(&mut self, event: Event) -> Result<bool> {
        if !matches!(event, Event::Resize(_, _)) {
            self.status_message = None;
        }

        if let Event::Key(key) = event {
            if self.help_visible {
                // Any key dismisses help.
                self.help_visible = false;
                return Ok(self.should_quit);
            }
            // 1. Global keys win unconditionally.
            if let Some(msg) = Self::handle_global_key(key, &self.layout.active_pane) {
                self.queue.push_back(msg);
                self.drain();
                return Ok(self.should_quit);
            }
            // 2. Folders-pane keys go to FoldersComponent.
            if matches!(self.layout.active_pane, ActivePane::Folders) {
                let ctx_msg = {
                    let store = self.email_store.lock().unwrap();
                    let ctx = Self::make_ctx(&store);
                    self.folders.on_key(key, &ctx)
                };
                if let Some(msg) = ctx_msg {
                    self.queue.push_back(msg);
                    self.drain();
                    return Ok(self.should_quit);
                }
            }
            // 3. Messages-pane keys go to MessagesComponent.
            if matches!(self.layout.active_pane, ActivePane::Messages) {
                let ctx_msg = {
                    let store = self.email_store.lock().unwrap();
                    let ctx = Self::make_ctx(&store);
                    self.messages.on_key(key, &ctx)
                };
                if let Some(msg) = ctx_msg {
                    self.queue.push_back(msg);
                    self.drain();
                    return Ok(self.should_quit);
                }
            }
            // 4. Content-pane keys go to ContentComponent.
            if matches!(self.layout.active_pane, ActivePane::Content) {
                let ctx_msg = {
                    let store = self.email_store.lock().unwrap();
                    let ctx = Self::make_ctx(&store);
                    self.content.on_key(key, &ctx)
                };
                if let Some(msg) = ctx_msg {
                    self.queue.push_back(msg);
                    self.drain();
                    return Ok(self.should_quit);
                }
            }
            // 5. Pane-agnostic legacy keys (Backspace, Attachments j/k/Enter).
            self.handle_residual_key(key);
            self.drain();
            self.request_body_if_needed();
        }
        Ok(self.should_quit)
    }

    /// Handle keys that didn't reach a component: Backspace, attachments
    /// pane navigation, attachment-open. Mirrors the surviving bits of
    /// the legacy `input::handle_*` family.
    fn handle_residual_key(&mut self, key: KeyEvent) {
        if !key.modifiers.is_empty() && !matches!(key.modifiers, KeyModifiers::SHIFT) {
            return;
        }
        match key.code {
            KeyCode::Backspace => {
                self.handle_back_navigation();
            }
            KeyCode::Char('j') | KeyCode::Down
                if matches!(self.layout.active_pane, ActivePane::Attachments) =>
            {
                let store = self.email_store.lock().unwrap();
                if let Some(email) = store.get_selected_email() {
                    if self.layout.selection.attachment_index + 1 < email.attachments.len() {
                        self.layout.selection.attachment_index += 1;
                    }
                }
            }
            KeyCode::Char('k') | KeyCode::Up
                if matches!(self.layout.active_pane, ActivePane::Attachments) =>
            {
                if self.layout.selection.attachment_index > 0 {
                    self.layout.selection.attachment_index -= 1;
                }
            }
            KeyCode::Enter if matches!(self.layout.active_pane, ActivePane::Attachments) => {
                self.handle_attachment_open();
            }
            _ => {}
        }
    }

    fn handle_back_navigation(&mut self) {
        match self.layout.active_pane {
            ActivePane::Folders | ActivePane::Messages => {
                self.queue.push_back(Msg::FolderExitParent);
            }
            ActivePane::Content => {
                // Switch back to email list view.
                self.layout.current_view = View::MessagesContent;
                self.layout.active_pane = ActivePane::Messages;
                self.publish_focus();
            }
            ActivePane::Attachments => {
                self.layout.current_view = View::MessagesContent;
                self.layout.active_pane = ActivePane::Messages;
                self.publish_focus();
            }
            ActivePane::Accounts | ActivePane::Draft => {}
        }
    }

    fn handle_attachment_open(&mut self) {
        let store = self.email_store.lock().unwrap();
        let filename = store.get_selected_email().and_then(|email| {
            email
                .attachments
                .get(self.layout.selection.attachment_index)
                .map(|a| a.filename.clone())
        });
        drop(store);
        if let Some(filename) = filename {
            self.status_message = Some(format!("Opening {}: Not implemented yet", filename));
        }
    }

    fn drain_scanned_folders(&mut self) {
        let Some(handle) = self.folder_scanner.as_ref() else {
            return;
        };
        match handle.try_recv() {
            Ok(Ok(root)) => {
                let mut store = self.email_store.lock().unwrap();
                store.root_folder = root;
                store.scanning_folders = false;
                let new_index = FoldersComponent::auto_select_inbox(&store.root_folder);
                self.folders.folder_index = new_index;
                self.layout.selection.folder_index = new_index;
                let indices =
                    layout::get_folder_path_from_display_index(&store.root_folder, new_index);
                drop(store);
                if let Some(indices) = indices {
                    self.request_folder_load_if_needed(&indices);
                }
                self.folder_scanner = None;
            }
            Ok(Err(e)) => {
                self.email_store.lock().unwrap().scanning_folders = false;
                self.status_message = Some(format!("Error scanning MailDir: {}", e));
                self.folder_scanner = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.email_store.lock().unwrap().scanning_folders = false;
                self.status_message = Some("Folder scanner thread died before replying".into());
                self.folder_scanner = None;
            }
        }
    }

    fn drain_loaded_bodies(&mut self) {
        let mut store = self.email_store.lock().unwrap();
        while let Ok(loaded) = self.body_loader.try_recv() {
            self.loading_paths.remove(&loaded.path);
            if let Some(parsed) = loaded.parsed {
                store.apply_loaded_body(
                    &loaded.path,
                    parsed.body_text,
                    parsed.body_html,
                    parsed.attachments,
                );
            }
        }
    }

    fn drain_loaded_folders(&mut self) {
        let mut store = self.email_store.lock().unwrap();
        while let Ok(loaded) = self.headers_loader.try_recv() {
            self.loading_folder_paths.remove(&loaded.fs_path);
            store.apply_loaded_folder(&loaded.fs_path, loaded.emails, loaded.fully_loaded);
        }
    }

    fn request_folder_load_if_needed(&mut self, indices: &[usize]) {
        let store = self.email_store.lock().unwrap();
        let Some(folder) = store.get_folder_at_path(indices) else {
            return;
        };
        if folder.is_loaded || !folder.emails.is_empty() {
            return;
        }
        let fs_path = folder.path.clone();
        drop(store);
        if !self.loading_folder_paths.insert(fs_path.clone()) {
            return;
        }
        let limit = (self.message_pane_visible_rows + 5).max(10);
        self.headers_loader.request(LoadFolderRequest {
            fs_path,
            limit: Some(limit),
        });
    }

    /// Switch into the currently-selected folder without blocking on the
    /// headers load. Mirrors the synchronous-side-effects half of the
    /// legacy `input::handle_folder_selection_and_switch_view`, but defers
    /// disk I/O to the off-thread headers worker.
    fn enter_selected_folder_async(&mut self) {
        let path = {
            let store = self.email_store.lock().unwrap();
            layout::get_folder_path_from_display_index(
                &store.root_folder,
                self.folders.folder_index,
            )
        };
        let Some(path) = path else { return };

        {
            let mut store = self.email_store.lock().unwrap();
            store.current_folder.clear();
            store.enter_folder_by_path(&path);
        }

        self.request_folder_load_if_needed(&path);

        self.layout.selection.email_index = 0;
        self.layout.selection.scroll_offset = 0;
        self.layout.selection.remembered_email_index = None;

        {
            let mut store = self.email_store.lock().unwrap();
            if !store.get_current_folder().emails.is_empty() {
                store.select_email(0);
            }
        }

        self.layout.current_view = if self.layout.content_pane_hidden {
            View::Messages
        } else {
            View::MessagesContent
        };
        self.layout.active_pane = ActivePane::Messages;
        self.publish_focus();
    }

    fn request_body_if_needed(&mut self) {
        let store = self.email_store.lock().unwrap();
        let Some(email) = store.get_selected_email() else {
            return;
        };
        if !matches!(email.load_state, EmailLoadState::HeadersOnly) {
            return;
        }
        let path = email.file_path.clone();
        drop(store);
        if self.loading_paths.insert(path.clone()) {
            self.body_loader.request(path);
        }
    }

    fn handle_global_key(key: KeyEvent, active_pane: &ActivePane) -> Option<Msg> {
        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), m) if m.is_empty() => Some(Msg::Quit),
            (KeyCode::Char('?'), m) if m.is_empty() => Some(Msg::ToggleHelp),
            (KeyCode::Char('c'), KeyModifiers::ALT) => Some(Msg::ToggleContentPane),
            (KeyCode::Tab, _) => Some(Msg::FocusNext),
            (KeyCode::BackTab, _) => Some(Msg::FocusPrev),
            (KeyCode::Char('h'), m) if m.is_empty() => Some(Msg::ViewPrev),
            (KeyCode::Char('l'), m) if m.is_empty() => {
                if matches!(active_pane, ActivePane::Folders) {
                    None
                } else {
                    Some(Msg::ViewNext)
                }
            }
            _ => None,
        }
    }

    /// Drain the message queue. Bounded by `MAX_DISPATCH_DEPTH`.
    pub fn drain(&mut self) -> bool {
        let mut steps = 0usize;
        while let Some(msg) = self.queue.pop_front() {
            steps += 1;
            if steps > MAX_DISPATCH_DEPTH {
                return false;
            }
            let follow_ups = {
                let store = self.email_store.lock().unwrap();
                let ctx = Self::make_ctx(&store);
                let mut fu = self.folders.handle_msg(&msg, &ctx);
                fu.extend(self.messages.handle_msg(&msg, &ctx));
                fu.extend(self.content.handle_msg(&msg, &ctx));
                fu.extend(self.accounts.handle_msg(&msg, &ctx));
                fu.extend(self.draft.handle_msg(&msg, &ctx));
                fu
            };
            self.queue.extend(follow_ups);
            self.apply_root(&msg);
        }
        true
    }

    /// Republish the focused pane to the web server and enqueue a
    /// `FocusChanged` message for any in-process subscribers.
    fn publish_focus(&mut self) {
        self.focused_pane
            .store(self.layout.active_pane.to_u8(), Ordering::Relaxed);
        self.queue
            .push_back(Msg::FocusChanged(self.layout.active_pane));
    }

    fn apply_root(&mut self, msg: &Msg) {
        match msg {
            Msg::Quit => {
                self.should_quit = true;
            }
            Msg::ToggleHelp => {
                self.help_visible = !self.help_visible;
            }
            Msg::ToggleContentPane => {
                self.layout.toggle_content_pane();
                self.publish_focus();
            }
            Msg::FocusNext => {
                let (old, new) = self.layout.switch_pane(PaneSwitchDirection::Right);
                self.on_focus_change(old, new);
            }
            Msg::FocusPrev => {
                let (old, new) = self.layout.switch_pane(PaneSwitchDirection::Left);
                self.on_focus_change(old, new);
            }
            Msg::ViewNext => {
                let old = self.layout.active_pane;
                self.layout.next_view();
                let new = self.layout.active_pane;
                self.on_focus_change(old, new);
            }
            Msg::ViewPrev => {
                let old = self.layout.active_pane;
                self.layout.prev_view();
                let new = self.layout.active_pane;
                self.on_focus_change(old, new);
            }
            Msg::FolderMove(_) => {
                self.layout.selection.folder_index = self.folders.folder_index;
                self.layout.selection.email_index = self.messages.email_index;
                self.layout.selection.remembered_email_index = self.messages.remembered_email_index;
                self.layout.selection.scroll_offset = self.content.scroll_offset;

                let indices = {
                    let store = self.email_store.lock().unwrap();
                    layout::get_folder_path_from_display_index(
                        &store.root_folder,
                        self.folders.folder_index,
                    )
                };
                if let Some(indices) = indices {
                    self.request_folder_load_if_needed(&indices);
                    let path = {
                        let store = self.email_store.lock().unwrap();
                        store.get_folder_at_path(&indices).map(|f| f.path.clone())
                    };
                    if let Some(path) = path {
                        self.queue.push_back(Msg::FolderLoaded(path));
                    }
                }
            }
            Msg::FolderEnter => {
                self.enter_selected_folder_async();
                self.layout.selection.email_index = self.messages.email_index;
                self.layout.selection.remembered_email_index = self.messages.remembered_email_index;
                self.layout.selection.scroll_offset = self.content.scroll_offset;
            }
            Msg::FolderExitParent => {
                self.email_store.lock().unwrap().exit_folder();
                self.layout.selection.folder_index = self.folders.folder_index;
                self.layout.selection.email_index = self.messages.email_index;
                self.layout.selection.scroll_offset = self.content.scroll_offset;
                self.layout.selection.remembered_email_index = self.messages.remembered_email_index;
                self.layout.active_pane = ActivePane::Folders;
                self.layout.current_view = if self.layout.content_pane_hidden {
                    View::Messages
                } else {
                    View::FolderMessages
                };
                self.publish_focus();
            }
            Msg::MessageMove(_) => {
                let idx = self.messages.email_index;
                self.layout.selection.email_index = idx;
                self.email_store.lock().unwrap().select_email(idx);
            }
            Msg::MessageOpen(_) => {
                let idx = self.messages.email_index;
                let mut store = self.email_store.lock().unwrap();
                let folder = store.get_current_folder();
                if idx < folder.emails.len() {
                    store.select_email(idx);
                    drop(store);
                    self.layout.current_view = if self.layout.content_pane_hidden {
                        View::Messages
                    } else {
                        View::MessagesContent
                    };
                    self.layout.active_pane = ActivePane::Messages;
                    self.layout.selection.email_index = idx;
                    self.publish_focus();
                }
            }
            Msg::ContentScroll(_, _) => {
                self.layout.selection.scroll_offset = self.content.scroll_offset;
            }
            Msg::StoreLoadMore(idx) => {
                let mut store = self.email_store.lock().unwrap();
                if let Err(e) = store.load_more_messages_if_needed(&self.scanner, *idx) {
                    drop(store);
                    self.status_message = Some(format!("Error loading more messages: {}", e));
                }
            }
            Msg::FoldersBlur | Msg::MessagesBlur => {
                let idx = self.messages.email_index;
                let mut store = self.email_store.lock().unwrap();
                let folder = store.get_current_folder();
                if idx < folder.emails.len() {
                    store.select_email(idx);
                }
                drop(store);
                self.layout.selection.email_index = idx;
                self.layout.selection.remembered_email_index = self.messages.remembered_email_index;
            }
            Msg::StatusSet(s) => {
                self.status_message = Some(s.clone());
            }
            Msg::StatusClear => {
                self.status_message = None;
            }
            _ => {}
        }
    }

    fn on_focus_change(&mut self, old: ActivePane, new: ActivePane) {
        if old != new {
            self.publish_focus();
        }
        match (old, new) {
            (ActivePane::Folders, ActivePane::Messages) => {
                self.queue.push_back(Msg::FoldersBlur);
            }
            (ActivePane::Messages, ActivePane::Folders) => {
                self.queue.push_back(Msg::MessagesBlur);
            }
            _ => {}
        }
    }

    /// Read-only handles for ui.rs.
    pub fn layout(&self) -> &Layout {
        &self.layout
    }
    pub fn status_message(&self) -> &Option<String> {
        &self.status_message
    }
    pub fn help_visible(&self) -> bool {
        self.help_visible
    }

    fn make_ctx(store: &EmailStore) -> Ctx<'_> {
        Ctx {
            theme: &THEME,
            config: &CONFIG,
            store,
        }
    }
}

static THEME: VulthorTheme = VulthorTheme;
static CONFIG: std::sync::LazyLock<Config> = std::sync::LazyLock::new(Config::default);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::email::{Email, EmailStore, Folder};
    use crate::maildir::MaildirScanner;
    use std::path::PathBuf;

    fn make_root() -> AppRoot {
        let store = EmailStore::new(PathBuf::from("/tmp"));
        let scanner = MaildirScanner::new(PathBuf::from("/tmp"));
        AppRoot::new(Arc::new(Mutex::new(store)), scanner)
    }

    fn make_root_with_folders(names: &[&str]) -> AppRoot {
        let mut store = EmailStore::new(PathBuf::from("/tmp"));
        for name in names {
            let mut folder = Folder::new(name.to_string(), PathBuf::from(format!("/tmp/{}", name)));
            folder.add_email(Email::new(PathBuf::from(format!("/tmp/{}/m1", name))));
            folder.add_email(Email::new(PathBuf::from(format!("/tmp/{}/m2", name))));
            folder.is_loaded = true;
            store.root_folder.add_subfolder(folder);
        }
        let scanner = MaildirScanner::new(PathBuf::from("/tmp"));
        AppRoot::new(Arc::new(Mutex::new(store)), scanner)
    }

    #[test]
    fn approot_dispatches_quit_msg() {
        let mut root = make_root();
        root.enqueue(Msg::Quit);
        assert!(root.drain());
        assert!(root.should_quit);
    }

    #[test]
    fn approot_toggles_help() {
        let mut root = make_root();
        root.enqueue(Msg::ToggleHelp);
        root.drain();
        assert!(root.help_visible);
        root.enqueue(Msg::ToggleHelp);
        root.drain();
        assert!(!root.help_visible);
    }

    #[test]
    fn handle_global_key_maps_q_to_quit() {
        let key = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        assert_eq!(
            AppRoot::handle_global_key(key, &ActivePane::Messages),
            Some(Msg::Quit)
        );
    }

    #[test]
    fn handle_global_key_l_from_folders_defers() {
        let key = KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE);
        assert!(AppRoot::handle_global_key(key, &ActivePane::Folders).is_none());
        assert_eq!(
            AppRoot::handle_global_key(key, &ActivePane::Messages),
            Some(Msg::ViewNext)
        );
    }

    #[test]
    fn handle_global_key_alt_c_toggles_content_pane() {
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::ALT);
        assert_eq!(
            AppRoot::handle_global_key(key, &ActivePane::Folders),
            Some(Msg::ToggleContentPane)
        );
        let plain = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE);
        assert!(AppRoot::handle_global_key(plain, &ActivePane::Folders).is_none());
    }

    #[test]
    fn key_sequence_jj_selects_third_folder() {
        let mut root = make_root_with_folders(&["A", "B", "C", "D"]);
        let j = Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        root.process_event(j.clone()).unwrap();
        root.process_event(j).unwrap();
        assert_eq!(root.folders.folder_index, 2);
        assert_eq!(root.layout.selection.folder_index, 2);
    }

    #[test]
    fn key_k_at_top_clamps() {
        let mut root = make_root_with_folders(&["A", "B"]);
        root.folders.folder_index = 0;
        root.layout.selection.folder_index = 0;
        let k = Event::Key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
        root.process_event(k).unwrap();
        assert_eq!(root.folders.folder_index, 0);
    }

    #[test]
    fn key_j_at_bottom_clamps() {
        let mut root = make_root_with_folders(&["A", "B"]);
        root.folders.folder_index = 1;
        root.layout.selection.folder_index = 1;
        let j = Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        root.process_event(j).unwrap();
        assert_eq!(root.folders.folder_index, 1);
    }

    #[test]
    fn approot_new_auto_selects_inbox() {
        let root = make_root_with_folders(&["Drafts", "Sent", "INBOX", "Archive"]);
        assert_eq!(root.folders.folder_index, 0);
    }

    #[test]
    fn selection_change_dispatches_body_load_without_blocking() {
        let mut store = EmailStore::new(PathBuf::from("/tmp"));
        let mut inbox = Folder::new("INBOX".to_string(), PathBuf::from("/tmp/INBOX"));
        let phantom_path = PathBuf::from("/definitely/does/not/exist/for/vu-6td.eml");
        inbox.add_email(Email::new(phantom_path.clone()));
        inbox.is_loaded = true;
        store.root_folder.add_subfolder(inbox);
        store.enter_folder_by_path(&[0]);
        store.select_email(0);

        let scanner = MaildirScanner::new(PathBuf::from("/tmp"));
        let store = Arc::new(Mutex::new(store));
        let mut root = AppRoot::new(store.clone(), scanner);
        root.layout.active_pane = ActivePane::Messages;

        let x = Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        root.process_event(x).unwrap();

        assert!(
            root.loading_paths.contains(&phantom_path),
            "selection must enqueue an off-thread body-load request",
        );

        let store = store.lock().unwrap();
        let email = store.get_selected_email().expect("email is selected");
        assert!(matches!(
            email.load_state,
            crate::email::EmailLoadState::HeadersOnly
        ));
        assert!(email.body_text.is_empty());
    }

    #[test]
    fn duplicate_body_load_requests_are_deduped() {
        let mut store = EmailStore::new(PathBuf::from("/tmp"));
        let mut inbox = Folder::new("INBOX".to_string(), PathBuf::from("/tmp/INBOX"));
        let path = PathBuf::from("/nonexistent/dedup.eml");
        inbox.add_email(Email::new(path.clone()));
        inbox.is_loaded = true;
        store.root_folder.add_subfolder(inbox);
        store.enter_folder_by_path(&[0]);
        store.select_email(0);

        let scanner = MaildirScanner::new(PathBuf::from("/tmp"));
        let mut root = AppRoot::new(Arc::new(Mutex::new(store)), scanner);

        root.request_body_if_needed();
        let before = root.loading_paths.len();
        root.request_body_if_needed();
        assert_eq!(before, root.loading_paths.len());
        assert!(root.loading_paths.contains(&path));
    }

    #[test]
    fn drain_scanned_folders_swaps_in_scan_and_resets_loading() {
        use crate::components::FolderScannerHandle;
        use std::fs;
        use std::time::{Duration, Instant};

        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path();
        for name in &["Archive", "Drafts", "INBOX", "Sent"] {
            fs::create_dir_all(root.join(name).join("cur")).unwrap();
            fs::create_dir_all(root.join(name).join("new")).unwrap();
            fs::create_dir_all(root.join(name).join("tmp")).unwrap();
        }

        let mut store = EmailStore::new(root.to_path_buf());
        store.scanning_folders = true;
        let scanner = MaildirScanner::new(root.to_path_buf());
        let shared = Arc::new(Mutex::new(store));
        let mut approot = AppRoot::new(shared.clone(), scanner);
        approot.attach_folder_scanner(FolderScannerHandle::spawn(root.to_path_buf()));

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            approot.drain_scanned_folders();
            if !shared.lock().unwrap().scanning_folders {
                break;
            }
            if Instant::now() > deadline {
                panic!("folder scan never landed");
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        let store = shared.lock().unwrap();
        assert!(!store.scanning_folders);
        assert_eq!(store.root_folder.subfolders.len(), 4);
        let sorted = store.root_folder.get_sorted_subfolders();
        let inbox_idx = sorted
            .iter()
            .position(|f| f.get_display_name().eq_ignore_ascii_case("INBOX"))
            .expect("INBOX is in the fixture");
        assert_eq!(approot.folders.folder_index, inbox_idx);
        assert_eq!(approot.layout.selection.folder_index, inbox_idx);
    }

    #[test]
    fn folder_move_dispatches_headers_load_off_thread() {
        use std::fs;
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let root_path = temp.path().to_path_buf();
        for name in &["INBOX", "Archive"] {
            fs::create_dir_all(root_path.join(name).join("cur")).unwrap();
            fs::create_dir_all(root_path.join(name).join("new")).unwrap();
            fs::create_dir_all(root_path.join(name).join("tmp")).unwrap();
            let body = "From: a@b.test\r\nTo: c@d.test\r\nSubject: hi\r\nDate: Mon, 01 Jan 2024 00:00:00 +0000\r\nMessage-ID: <1@b.test>\r\n\r\nbody\r\n";
            fs::write(root_path.join(name).join("cur/m1.eml"), body).unwrap();
        }

        let scanner = MaildirScanner::new(root_path.clone());
        let mut store = EmailStore::new(root_path.clone());
        store.root_folder = scanner.scan().unwrap();
        let shared = Arc::new(Mutex::new(store));
        let mut root = AppRoot::new(shared.clone(), scanner);

        let j = Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        let start = std::time::Instant::now();
        root.process_event(j).unwrap();
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(100),
            "keystroke must be near-instant on the TUI thread, took {:?}",
            elapsed,
        );

        std::thread::sleep(std::time::Duration::from_millis(200));
        root.drain_loaded_folders();
        let store = shared.lock().unwrap();
        let archive = store
            .root_folder
            .subfolders
            .iter()
            .find(|f| f.name == "Archive")
            .expect("Archive subfolder exists");
        assert!(!archive.emails.is_empty());
    }

    #[test]
    fn folder_navigation_does_not_block_on_disk_io() {
        use std::fs;
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let root_path = temp.path().to_path_buf();
        for i in 0..100 {
            let name = format!("folder_{:03}", i);
            fs::create_dir_all(root_path.join(&name).join("cur")).unwrap();
            fs::create_dir_all(root_path.join(&name).join("new")).unwrap();
            fs::create_dir_all(root_path.join(&name).join("tmp")).unwrap();
            let body = format!(
                "From: a@b.test\r\nTo: c@d.test\r\nSubject: f{}\r\nDate: Mon, 01 Jan 2024 00:00:00 +0000\r\nMessage-ID: <{}@b.test>\r\n\r\nbody\r\n",
                i, i
            );
            fs::write(root_path.join(&name).join("cur/m1.eml"), body).unwrap();
        }

        let scanner = MaildirScanner::new(root_path.clone());
        let mut store = EmailStore::new(root_path.clone());
        store.root_folder = scanner.scan().unwrap();
        let mut root = AppRoot::new(Arc::new(Mutex::new(store)), scanner);

        let start = std::time::Instant::now();
        for _ in 0..100 {
            let j = Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
            root.process_event(j).unwrap();
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "100 folder-move keystrokes must not block, took {:?}",
            elapsed,
        );
    }

    #[test]
    fn key_j_in_messages_pane_advances_via_component() {
        let mut store = EmailStore::new(PathBuf::from("/tmp"));
        let mut inbox = Folder::new("INBOX".to_string(), PathBuf::from("/tmp/INBOX"));
        for i in 0..3 {
            inbox.add_email(Email::new(PathBuf::from(format!("/tmp/INBOX/m{}", i))));
        }
        inbox.is_loaded = true;
        store.root_folder.add_subfolder(inbox);
        store.current_folder = vec![0];
        store.select_email(0);

        let scanner = crate::maildir::MaildirScanner::new(PathBuf::from("/tmp"));
        let shared = Arc::new(Mutex::new(store));
        let mut root = AppRoot::new(shared.clone(), scanner);
        root.layout.active_pane = ActivePane::Messages;

        let j = Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        root.process_event(j).unwrap();

        assert_eq!(root.messages.email_index, 1);
        assert_eq!(root.layout.selection.email_index, 1);
        assert_eq!(shared.lock().unwrap().selected_email, Some(1));
    }

    #[test]
    fn tab_folders_to_messages_restores_remembered_email() {
        let mut store = EmailStore::new(PathBuf::from("/tmp"));
        let mut inbox = Folder::new("INBOX".to_string(), PathBuf::from("/tmp/INBOX"));
        for i in 0..4 {
            inbox.add_email(Email::new(PathBuf::from(format!("/tmp/INBOX/m{}", i))));
        }
        inbox.is_loaded = true;
        store.root_folder.add_subfolder(inbox);
        store.current_folder = vec![0];
        store.select_email(0);

        let scanner = crate::maildir::MaildirScanner::new(PathBuf::from("/tmp"));
        let shared = Arc::new(Mutex::new(store));
        let mut root = AppRoot::new(shared.clone(), scanner);
        root.layout.active_pane = ActivePane::Messages;
        root.layout.current_view = View::FolderMessages;

        root.messages.email_index = 2;
        root.layout.selection.email_index = 2;
        shared.lock().unwrap().select_email(2);

        let back = Event::Key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE));
        root.process_event(back).unwrap();
        assert_eq!(root.layout.active_pane, ActivePane::Folders);
        assert_eq!(root.messages.remembered_email_index, Some(2));

        let fwd = Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        root.process_event(fwd).unwrap();
        assert_eq!(root.layout.active_pane, ActivePane::Messages);
        assert_eq!(root.messages.email_index, 2);
    }

    #[test]
    fn backspace_in_messages_pane_routes_through_folder_exit_parent() {
        let mut store = EmailStore::new(PathBuf::from("/tmp"));
        let mut inbox = Folder::new("INBOX".to_string(), PathBuf::from("/tmp/INBOX"));
        inbox.add_email(Email::new(PathBuf::from("/tmp/INBOX/m0")));
        inbox.is_loaded = true;
        store.root_folder.add_subfolder(inbox);
        store.current_folder = vec![0];
        store.select_email(0);

        let scanner = crate::maildir::MaildirScanner::new(PathBuf::from("/tmp"));
        let shared = Arc::new(Mutex::new(store));
        let mut root = AppRoot::new(shared.clone(), scanner);
        root.layout.active_pane = ActivePane::Messages;
        root.folders.folder_index = 0;
        root.messages.email_index = 0;

        let bksp = Event::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        root.process_event(bksp).unwrap();

        let store = shared.lock().unwrap();
        assert!(store.current_folder.is_empty());
        assert_eq!(root.layout.selection.folder_index, 0);
        assert_eq!(root.layout.selection.email_index, 0);
    }

    #[test]
    fn key_j_in_content_pane_scrolls_via_component() {
        let mut store = EmailStore::new(PathBuf::from("/tmp"));
        let mut inbox = Folder::new("INBOX".to_string(), PathBuf::from("/tmp/INBOX"));
        inbox.add_email(Email::new(PathBuf::from("/tmp/INBOX/m0")));
        inbox.is_loaded = true;
        store.root_folder.add_subfolder(inbox);
        store.current_folder = vec![0];
        store.select_email(0);

        let scanner = crate::maildir::MaildirScanner::new(PathBuf::from("/tmp"));
        let mut root = AppRoot::new(Arc::new(Mutex::new(store)), scanner);
        root.layout.active_pane = ActivePane::Content;

        let j = Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        root.process_event(j).unwrap();

        assert_eq!(root.content.scroll_offset, 1);
        assert_eq!(root.layout.selection.scroll_offset, 1);
    }

    #[test]
    fn key_pagedown_in_content_pane_scrolls_by_ten() {
        let mut store = EmailStore::new(PathBuf::from("/tmp"));
        let mut inbox = Folder::new("INBOX".to_string(), PathBuf::from("/tmp/INBOX"));
        inbox.add_email(Email::new(PathBuf::from("/tmp/INBOX/m0")));
        inbox.is_loaded = true;
        store.root_folder.add_subfolder(inbox);
        store.current_folder = vec![0];
        store.select_email(0);

        let scanner = crate::maildir::MaildirScanner::new(PathBuf::from("/tmp"));
        let mut root = AppRoot::new(Arc::new(Mutex::new(store)), scanner);
        root.layout.active_pane = ActivePane::Content;

        let pd = Event::Key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        root.process_event(pd).unwrap();
        assert_eq!(root.content.scroll_offset, 10);
        assert_eq!(root.layout.selection.scroll_offset, 10);
    }

    #[test]
    fn key_pageup_in_content_pane_saturates_at_zero() {
        let mut store = EmailStore::new(PathBuf::from("/tmp"));
        let mut inbox = Folder::new("INBOX".to_string(), PathBuf::from("/tmp/INBOX"));
        inbox.add_email(Email::new(PathBuf::from("/tmp/INBOX/m0")));
        inbox.is_loaded = true;
        store.root_folder.add_subfolder(inbox);
        store.current_folder = vec![0];
        store.select_email(0);

        let scanner = crate::maildir::MaildirScanner::new(PathBuf::from("/tmp"));
        let mut root = AppRoot::new(Arc::new(Mutex::new(store)), scanner);
        root.layout.active_pane = ActivePane::Content;

        let pu = Event::Key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
        root.process_event(pu).unwrap();
        assert_eq!(root.content.scroll_offset, 0);
        assert_eq!(root.layout.selection.scroll_offset, 0);
    }

    #[test]
    fn folder_enter_resets_content_scroll_offset() {
        let mut store = EmailStore::new(PathBuf::from("/tmp"));
        let mut inbox = Folder::new("INBOX".to_string(), PathBuf::from("/tmp/INBOX"));
        inbox.add_email(Email::new(PathBuf::from("/tmp/INBOX/m0")));
        inbox.is_loaded = true;
        store.root_folder.add_subfolder(inbox);

        let scanner = crate::maildir::MaildirScanner::new(PathBuf::from("/tmp"));
        let mut root = AppRoot::new(Arc::new(Mutex::new(store)), scanner);

        root.content.scroll_offset = 42;
        root.layout.selection.scroll_offset = 42;
        root.layout.active_pane = ActivePane::Folders;

        let enter = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        root.process_event(enter).unwrap();
        assert_eq!(root.content.scroll_offset, 0);
        assert_eq!(root.layout.selection.scroll_offset, 0);
    }

    #[test]
    fn focus_change_publishes_to_atomic() {
        let mut store = EmailStore::new(PathBuf::from("/tmp"));
        let mut inbox = Folder::new("INBOX".to_string(), PathBuf::from("/tmp/INBOX"));
        inbox.add_email(Email::new(PathBuf::from("/tmp/INBOX/m0")));
        inbox.is_loaded = true;
        store.root_folder.add_subfolder(inbox);
        let scanner = MaildirScanner::new(PathBuf::from("/tmp"));
        let mut root = AppRoot::new(Arc::new(Mutex::new(store)), scanner);
        let focus = root.focused_pane();
        assert_eq!(focus.load(Ordering::Relaxed), ActivePane::Folders.to_u8());

        let tab = Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        root.process_event(tab).unwrap();
        assert_eq!(focus.load(Ordering::Relaxed), ActivePane::Messages.to_u8());
    }
}
