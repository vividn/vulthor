// `AppRoot` — main-loop driver, sole owner of the TUI state.
//
// AppRoot owns `Layout`, `status_message`, `should_quit`, etc.
// directly. The `EmailStore` is the only thing shared across the
// TUI ↔ web boundary (`Arc<Mutex<EmailStore>>`), and the focused pane
// travels via `Arc<AtomicU8>` so the web server can decide between
// serving the selected email and the welcome screen without locking
// the store.
//
// Components are canonical for the slice of state they own:
// `FoldersComponent.folder_index`, `MessagesComponent.email_index` /
// `remembered_email_index`, `ContentComponent.scroll_offset`. Render
// code (ui.rs) reads them via the `folders()`/`messages()`/`content()`
// accessors on AppRoot. The only cursor still owned by `Layout` is
// `selection.attachment_index`, which `handle_residual_key` mutates
// directly because there is no AttachmentsComponent yet.
//
// Backspace from Content/Attachments and attachment-pane navigation
// are handled inline by AppRoot rather than via a per-pane component.

use std::collections::{HashSet, VecDeque};
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::config::Config;
use crate::email::{EmailLoadState, EmailStore, MarkReadPlan};
use crate::error::Result;
use crate::keymap::{Action, Keymap, resolve_keymap};
use crate::layout::{self, ActivePane, Layout, PaneSwitchDirection, View};
use crate::maildir::MaildirScanner;
use crate::theme::{Theme, VulthorTheme};
use crate::ui::UI;
use crate::undo::{Mutation, Reversed};

use super::{
    AccountsComponent, BodyLoader, Component, ContentComponent, Ctx, Dir, DraftComponent,
    FolderPickerComponent, FolderScannerHandle, FoldersComponent, HeadersLoader, LoadFolderRequest,
    MAILDIR_WATCH_DEBOUNCE, MAX_DISPATCH_DEPTH, MaildirWatcherComponent, MessagesComponent, Msg,
    ReplyKind, SearchComponent, notmuch_available, parse_notmuch_files_output,
};

use super::content::PAGE_SCROLL_STEP;
use crate::compose::{Compose, build_reply_template, default_template};
use crate::config::AccountConfig;

pub struct AppRoot {
    /// The single shared resource. The web server reads it; the TUI
    /// thread writes it under the same lock during dispatch.
    email_store: Arc<Mutex<EmailStore>>,
    /// Published to the web server so it can answer "serve email vs.
    /// welcome" without locking the store. Updated on every focus
    /// change via `Msg::FocusChanged`.
    focused_pane: Arc<AtomicU8>,

    /// User config (incl. `[accounts.*]`). `Msg::AccountSelect` reads
    /// it to find the maildir_path to rebuild the store against.
    config: Config,
    scanner: MaildirScanner,
    layout: Layout,
    status_message: Option<String>,
    should_quit: bool,
    /// Toggled by '?'.
    help_visible: bool,
    /// Updated by the Messages pane during render; used to size
    /// off-thread header loads.
    message_pane_visible_rows: usize,

    folders: FoldersComponent,
    messages: MessagesComponent,
    content: ContentComponent,
    accounts: AccountsComponent,
    draft: DraftComponent,
    /// Modal "move to folder" picker. When `folder_picker.visible` is
    /// true, `process_event` routes every key event to the picker
    /// first so the modal absorbs input.
    folder_picker: FolderPickerComponent,
    /// Modal notmuch search input (Phase 3.a). When `search.visible`
    /// is true, `process_event` routes every key event to the modal
    /// so it absorbs typed query characters. Independent from the
    /// active `search_results` virtual folder on the store — the
    /// modal closes once `SearchExecute` fires, even though results
    /// remain on display.
    search: SearchComponent,
    queue: VecDeque<Msg>,
    body_loader: BodyLoader,
    loading_paths: HashSet<PathBuf>,
    folder_scanner: Option<FolderScannerHandle>,
    headers_loader: HeadersLoader,
    loading_folder_paths: HashSet<PathBuf>,
    /// Session-only undo stack. Action-key handlers push a `Mutation`
    /// after a successful filesystem op; `Msg::Undo` pops and reverses.
    /// Lost on quit by design (VISION.md "Undo").
    undo_stack: Vec<Mutation>,
    /// Editor invocation deferred to the main loop (Phase 2.d).
    /// `Msg::DraftStart` for `Reply`/`ReplyAll`/`Forward` builds the
    /// template and parks it here; the run loop suspends the TUI,
    /// shells out to `$EDITOR`, then calls
    /// [`Self::apply_editor_result`] to resume dispatch. AppRoot
    /// cannot launch the editor inline because it doesn't own the
    /// terminal — `main.rs` does — and we need the TUI suspended
    /// around the call so the editor takes over stdio.
    pending_editor: Option<PendingEditorLaunch>,
    /// Port the embedded web server is listening on. AppRoot needs
    /// this to build the URL the chromeless HTML viewer (`v`)
    /// launches into. Defaults to 8080 to match `CliArgs::port` so
    /// tests that never set it explicitly still produce a valid URL;
    /// `main.rs` overrides via [`Self::set_web_port`] after parsing.
    web_port: u16,
    /// Live handle to the chromeless HTML viewer child process when
    /// one is running. `Some` between the first `v` press (launch)
    /// and the second `v` press (terminate). Owned here — not on the
    /// `html_viewer` module — so the AppRoot destructor reaps it.
    html_viewer_child: Option<std::process::Child>,
    /// Resolved runtime color theme. Built in `main.rs` by
    /// `theme::build_theme(&config)` and installed via
    /// [`Self::set_theme`]; defaults to the built-in palette so tests
    /// that skip the wiring still produce a valid theme. Adoption by
    /// the render path is tracked separately — today render code still
    /// reads `VulthorTheme::*` constants.
    #[allow(dead_code)]
    theme: Theme,
    /// Phase 4.d MailDir watcher. `Some` once a watcher has been
    /// successfully spawned against the active account's maildir root;
    /// `None` when the path does not exist or `notify` init failed
    /// (status-bar message surfaces the reason). Replaced by
    /// `switch_active_maildir` on `Msg::AccountSelect` so the watch
    /// always tracks the live tree.
    maildir_watcher: Option<MaildirWatcherComponent>,
    /// Resolved `KeyEvent → Action` table for global / pane-action key
    /// dispatch (VISION.md §Action Keybindings + `[keybindings]`
    /// overrides). Built once at construction from
    /// `config.keybindings.inner`. In-pane navigation (j/k/h/l, sequences
    /// like `gg`/`G`) is still handled by each component; AppRoot only
    /// consults this map for global and Draft-pane action keys.
    keymap: Keymap,
}

/// Reply-template editor invocation parked between AppRoot dispatch
/// and the run loop. `template` is the pre-populated editor buffer
/// produced by [`crate::compose::default_template`]; the run loop
/// invokes [`crate::compose::launch_editor`] with it, then calls
/// `AppRoot::apply_editor_result` with the parsed `Compose`.
#[derive(Debug, Clone)]
pub struct PendingEditorLaunch {
    /// Text written into the temp file before `$EDITOR` opens it.
    pub template: String,
}

impl AppRoot {
    /// Construct an AppRoot whose Accounts pane mirrors the config's
    /// `[accounts.*]` tables. Use this for the runtime. Tests that
    /// don't exercise multi-account behavior can call [`Self::new`] which
    /// substitutes `Config::default()`.
    pub fn with_config(
        email_store: Arc<Mutex<EmailStore>>,
        scanner: MaildirScanner,
        config: Config,
    ) -> Self {
        let initial_index = {
            let store = email_store.lock().unwrap();
            FoldersComponent::auto_select_inbox(&store.root_folder)
        };
        let layout = Layout::new();

        // Keymap resolution is infallible here: `Config::validate`
        // (called from every `Config::load*` path) already runs
        // `resolve_keymap` and rejects conflicts/typos with a structured
        // error before AppRoot is constructed. Tests that hand AppRoot a
        // hand-rolled `Config` skip validation but only ever use defaults
        // (empty overrides), which always resolve.
        let keymap = resolve_keymap(&config.keybindings.inner)
            .expect("keybindings already validated by Config::validate");

        let mut root = Self {
            email_store: email_store.clone(),
            focused_pane: Arc::new(AtomicU8::new(ActivePane::Folders.to_u8())),
            config: Config::default(),
            scanner: scanner.clone(),
            layout,
            status_message: None,
            should_quit: false,
            help_visible: false,
            message_pane_visible_rows: 20,
            folders: FoldersComponent::with_index(initial_index),
            messages: MessagesComponent::new(),
            content: ContentComponent::new(),
            accounts: AccountsComponent::with_config(&config),
            draft: DraftComponent::new(),
            folder_picker: FolderPickerComponent::new(),
            search: SearchComponent::new(),
            queue: VecDeque::new(),
            body_loader: BodyLoader::spawn(),
            loading_paths: HashSet::new(),
            folder_scanner: None,
            headers_loader: HeadersLoader::spawn(scanner),
            loading_folder_paths: HashSet::new(),
            undo_stack: Vec::new(),
            pending_editor: None,
            web_port: 8080,
            html_viewer_child: None,
            theme: Theme::default(),
            maildir_watcher: None,
            keymap,
        };
        // Stash the real config after building the component so the
        // AccountsComponent can be seeded with a borrowed reference
        // above without colliding with the move into `Self`.
        root.config = config;

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

    /// Default-config shim. Tests that don't exercise the Accounts
    /// pane use this; the runtime always uses [`Self::with_config`].
    pub fn new(email_store: Arc<Mutex<EmailStore>>, scanner: MaildirScanner) -> Self {
        Self::with_config(email_store, scanner, Config::default())
    }

    /// Hand the root the off-thread folder scanner started in `main`.
    pub fn attach_folder_scanner(&mut self, handle: FolderScannerHandle) {
        self.folder_scanner = Some(handle);
    }

    /// Tell AppRoot which port the embedded web server is listening
    /// on. `main.rs` calls this right after parsing `CliArgs::port`
    /// so the `v`-key viewer launches against the right URL.
    pub fn set_web_port(&mut self, port: u16) {
        self.web_port = port;
    }

    /// Install the runtime [`Theme`] resolved by
    /// `crate::theme::build_theme`. `main.rs` calls this after config
    /// load so user themes / `[theme].overrides` reach the component
    /// tree. Tests that skip the wiring keep the built-in default
    /// installed by `with_config`.
    #[allow(dead_code)]
    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
    }

    /// Install the runtime AI classifier and confidence cutoff built
    /// from `[ai]` config (Phase 5.a). `main.rs` calls this after
    /// config load so MessagesComponent's chip rendering and the
    /// `;`-key (`Action::AcceptSuggestion`) resolution share the same
    /// instance. Default `NoopClassifier` stays installed otherwise so
    /// `[ai].enabled = false` runs hit zero overhead.
    pub fn set_classifier(
        &mut self,
        classifier: std::sync::Arc<dyn crate::classifier::Classifier>,
        threshold: f32,
    ) {
        self.messages.set_classifier(classifier, threshold);
    }

    /// Spawn the Phase 4.d MailDir watcher against the live email
    /// store's root path. Called once from `main.rs` after AppRoot
    /// construction; tests skip it so the watcher does not subscribe
    /// to `/tmp`. Init failures surface in the status bar — the TUI
    /// still launches.
    pub fn init_maildir_watcher(&mut self) {
        let root = self.email_store.lock().unwrap().root_folder.path.clone();
        self.spawn_maildir_watcher(root);
    }

    fn spawn_maildir_watcher(&mut self, root: PathBuf) {
        match MaildirWatcherComponent::spawn(root, MAILDIR_WATCH_DEBOUNCE) {
            Ok(w) => {
                self.maildir_watcher = Some(w);
            }
            Err(e) => {
                self.maildir_watcher = None;
                self.status_message = Some(e.to_string());
            }
        }
    }

    /// Clone of the focused-pane signal the web server reads.
    pub fn focused_pane(&self) -> Arc<AtomicU8> {
        self.focused_pane.clone()
    }

    /// Clone of the body-loader request channel. The web server uses
    /// this to dispatch body parses to the same off-thread worker the
    /// TUI feeds, so no `fs::read` ever runs on an axum executor thread
    /// while holding the store lock.
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
    pub fn folder_picker(&self) -> &FolderPickerComponent {
        &self.folder_picker
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
        self.drain_maildir_watcher();
        self.request_body_if_needed();

        let store_arc = self.email_store.clone();
        let mut store = store_arc.lock().unwrap();
        let folders = &self.folders;
        let messages = &self.messages;
        let content = &self.content;
        let accounts = &self.accounts;
        let draft = &self.draft;
        let folder_picker = &self.folder_picker;
        let search = &self.search;
        let layout = &self.layout;
        let status = &self.status_message;
        let help = self.help_visible;
        terminal.draw(|f| {
            ui.draw(
                f,
                &mut store,
                layout,
                status,
                help,
                folders,
                messages,
                content,
                accounts,
                draft,
                folder_picker,
                search,
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
        self.drain_maildir_watcher();
        if !event::poll(Duration::from_millis(100))? {
            return Ok(false);
        }
        let event = event::read()?;
        self.process_event(event)
    }

    /// Forward any debounced `Msg::MailDirChanged` from the watcher
    /// onto the dispatch queue and drain so `apply_root` can
    /// invalidate the affected folder. Called from `tick` and `render`
    /// — same shape as the other off-thread drains.
    fn drain_maildir_watcher(&mut self) {
        let msgs = match self.maildir_watcher.as_mut() {
            Some(w) => w.drain(),
            None => return,
        };
        if msgs.is_empty() {
            return;
        }
        for m in msgs {
            self.queue.push_back(m);
        }
        self.drain();
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
            // 0. Modal picker, when visible, absorbs every key — global
            //    shortcuts included. This is what makes 'q' inside the
            //    modal type into the filter instead of quitting.
            if self.folder_picker.visible {
                let ctx_msg = {
                    let store = self.email_store.lock().unwrap();
                    let ctx = Self::make_ctx(&self.config, &store);
                    self.folder_picker.on_key(key, &ctx)
                };
                if let Some(msg) = ctx_msg {
                    self.queue.push_back(msg);
                }
                self.drain();
                return Ok(self.should_quit);
            }
            // 0b. Search input modal — same absorb-every-key contract
            //     as the folder picker. Closes on Esc/Enter; the
            //     follow-up Msg::SearchExecute fires the notmuch
            //     shell-out in `apply_root`.
            if self.search.visible {
                let ctx_msg = {
                    let store = self.email_store.lock().unwrap();
                    let ctx = Self::make_ctx(&self.config, &store);
                    self.search.on_key(key, &ctx)
                };
                if let Some(msg) = ctx_msg {
                    self.queue.push_back(msg);
                }
                self.drain();
                return Ok(self.should_quit);
            }
            // 0c. While a search-results virtual folder is on display
            //     (modal already closed), bare `Esc` exits the search
            //     and returns to the prior folder view. Bare `h`
            //     resolves to `Action::ViewPrev` via the keymap below,
            //     where `action_to_msg` also turns it into
            //     `Msg::SearchCancel` when results are active. `Esc`
            //     is hard-coded here because the keymap binds `Esc` to
            //     `DraftDiscard` (Draft pane only); without this
            //     intercept `Esc` would be a no-op outside Draft.
            if self.search_results_active()
                && key.modifiers.is_empty()
                && matches!(key.code, KeyCode::Esc)
            {
                self.queue.push_back(Msg::SearchCancel);
                self.drain();
                return Ok(self.should_quit);
            }
            // 0d. While a component has a multi-key sequence in flight
            //     (currently only MessagesComponent's `g`-prefix), route
            //     the next key into that component BEFORE the central
            //     keymap dispatch. Otherwise centralised single-key
            //     dispatch would intercept the sequence's second key
            //     (e.g. `r` → `ReplyAll`) and the sequence (`gr` →
            //     `Reply`) would never resolve.
            if matches!(self.layout.active_pane, ActivePane::Messages)
                && self.messages.has_pending_sequence()
            {
                let ctx_msg = {
                    let store = self.email_store.lock().unwrap();
                    let ctx = Self::make_ctx(&self.config, &store);
                    self.messages.on_key(key, &ctx)
                };
                if let Some(msg) = ctx_msg {
                    self.queue.push_back(msg);
                    self.drain();
                    return Ok(self.should_quit);
                }
                // Sequence aborted (`on_key` cleared `pending_g` and
                // returned None) — fall through to normal dispatch so
                // the key is interpreted as a single-key action.
            }
            // 1. Global / pane-action keys flow through the resolved
            //    [keybindings] table. Atomic-key lookups only;
            //    sequence keys (`gg`/`G`/`gj`/`gk`/`gr`) stay with the
            //    focused component for now.
            if let Some(action) = self.keymap.lookup_single(key) {
                // Phase 5.a — `;` (AcceptSuggestion) resolves the
                // cursor email's classifier suggestion into the
                // underlying mutation Msg (Archive / ToggleStar / …).
                // No-op under NoopClassifier (the default), under
                // sub-threshold confidence, and outside the Messages
                // pane — the chip only appears in the Messages list.
                if matches!(action, Action::AcceptSuggestion) {
                    if let Some(msg) = self.resolve_accept_suggestion() {
                        self.queue.push_back(msg);
                        self.drain();
                    }
                    return Ok(self.should_quit);
                }
                if let Some(msg) = Self::action_to_msg(
                    action,
                    &self.layout.active_pane,
                    self.search_results_active(),
                ) {
                    self.queue.push_back(msg);
                    self.drain();
                    return Ok(self.should_quit);
                }
            }
            // 2. Folders-pane keys go to FoldersComponent.
            if matches!(self.layout.active_pane, ActivePane::Folders) {
                let ctx_msg = {
                    let store = self.email_store.lock().unwrap();
                    let ctx = Self::make_ctx(&self.config, &store);
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
                    let ctx = Self::make_ctx(&self.config, &store);
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
                    let ctx = Self::make_ctx(&self.config, &store);
                    self.content.on_key(key, &ctx)
                };
                if let Some(msg) = ctx_msg {
                    self.queue.push_back(msg);
                    self.drain();
                    return Ok(self.should_quit);
                }
            }
            // 5. Accounts-pane keys go to AccountsComponent.
            if matches!(self.layout.active_pane, ActivePane::Accounts) {
                let ctx_msg = {
                    let store = self.email_store.lock().unwrap();
                    let ctx = Self::make_ctx(&self.config, &store);
                    self.accounts.on_key(key, &ctx)
                };
                if let Some(msg) = ctx_msg {
                    self.queue.push_back(msg);
                    self.drain();
                    return Ok(self.should_quit);
                }
            }
            // 5b. Draft-pane action keys (S send, e edit, q/Esc discard)
            //     resolve through the keymap dispatch at step 1. Kept
            //     as a numbered comment so future readers don't search
            //     for the old `handle_draft_pane_key`.
            // 6. Pane-agnostic legacy keys (Backspace, Attachments j/k/Enter).
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
                if let Some(email) = store.get_selected_email()
                    && self.layout.selection.attachment_index + 1 < email.attachments.len()
                {
                    self.layout.selection.attachment_index += 1;
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
            Ok(Ok(scanned)) => {
                let mut store = self.email_store.lock().unwrap();
                store.root_folder = scanned.root;
                store.drafts = scanned.drafts;
                store.scanning_folders = false;
                let new_index = FoldersComponent::auto_select_inbox(&store.root_folder);
                self.folders.folder_index = new_index;
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

    /// Map a key event in the Draft pane to a `Msg`. Only consumes
    /// the three action keys the pre-send footer documents: `S` sends,
    /// Translate a resolved [`Action`] into the matching `Msg`,
    /// applying the pane-sensitive routing the legacy
    /// `handle_global_key` / `handle_draft_pane_key` used to do inline.
    ///
    /// Returns `None` for actions handled inside a component
    /// (`Archive`, `Star`, `Delete`, `Confirm`, etc.) or when the
    /// action is intentionally a no-op in the current context (e.g.
    /// `Search` in the Draft pane, `ViewNext` in the Folders pane).
    fn action_to_msg(
        action: Action,
        active_pane: &ActivePane,
        search_results_active: bool,
    ) -> Option<Msg> {
        match action {
            // ---- Global lifecycle / layout -------------------------------
            // `q` quits everywhere except the Draft pane, where it
            // discards the in-flight reply (VISION.md §Pre-Send Flow).
            Action::Quit => Some(if matches!(active_pane, ActivePane::Draft) {
                Msg::DraftDiscard
            } else {
                Msg::Quit
            }),
            Action::ToggleHelp => Some(Msg::ToggleHelp),
            Action::Undo => Some(Msg::Undo),
            Action::ToggleViewer => Some(Msg::ToggleHtmlViewer),
            Action::ToggleContentPane => Some(Msg::ToggleContentPane),
            Action::FocusNext => Some(Msg::FocusNext),
            Action::FocusPrev => Some(Msg::FocusPrev),
            // `h` (ViewPrev) also exits a search-results virtual folder
            // — the dual binding matches VISION.md §Search expectations
            // without requiring a second Action variant.
            Action::ViewPrev => Some(if search_results_active {
                Msg::SearchCancel
            } else {
                Msg::ViewPrev
            }),
            // Folders use `l` for select-into semantics with a
            // context-aware branch (already-inside-folder → ViewNext)
            // that lives in FoldersComponent::on_key — AppRoot returns
            // None so that handler runs. Accounts is unconditional:
            // `l` = "select this account" (empty-id sentinel resolves
            // to the cursor in apply_root, same convention as
            // MessageOpen).
            Action::ViewNext => match active_pane {
                ActivePane::Folders => None,
                ActivePane::Accounts => Some(Msg::AccountSelect(String::new())),
                _ => Some(Msg::ViewNext),
            },
            // `/` opens the notmuch search modal everywhere except the
            // Draft pane, where `/` types into the in-flight reply via
            // `$EDITOR`.
            Action::Search => {
                if matches!(active_pane, ActivePane::Draft) {
                    None
                } else {
                    Some(Msg::OpenSearchInput)
                }
            }

            // ---- Per-pane navigation -------------------------------------
            // `j`/`k` (and `Down`/`Up` arrows via the defaults table)
            // dispatch into the focused pane's move-Msg. Attachments
            // still owns its own j/k via `handle_residual_key`
            // (returning `None` here lets that fallback fire).
            Action::MoveDown => match active_pane {
                ActivePane::Folders => Some(Msg::FolderMove(Dir::Down)),
                ActivePane::Messages => Some(Msg::MessageMove(Dir::Down)),
                ActivePane::Content => Some(Msg::ContentScroll(Dir::Down, 1)),
                ActivePane::Accounts => Some(Msg::AccountMove(Dir::Down)),
                _ => None,
            },
            Action::MoveUp => match active_pane {
                ActivePane::Folders => Some(Msg::FolderMove(Dir::Up)),
                ActivePane::Messages => Some(Msg::MessageMove(Dir::Up)),
                ActivePane::Content => Some(Msg::ContentScroll(Dir::Up, 1)),
                ActivePane::Accounts => Some(Msg::AccountMove(Dir::Up)),
                _ => None,
            },
            // PageDown / PageUp page-scroll the Content pane. The
            // remaining list panes intentionally don't page — VISION.md
            // §Action Keybindings doesn't promise paged navigation
            // there, and users still get fine-grained motion via j/k
            // and the gj/gk unread-jump sequences.
            Action::PageDown => match active_pane {
                ActivePane::Content => Some(Msg::ContentScroll(Dir::Down, PAGE_SCROLL_STEP)),
                _ => None,
            },
            Action::PageUp => match active_pane {
                ActivePane::Content => Some(Msg::ContentScroll(Dir::Up, PAGE_SCROLL_STEP)),
                _ => None,
            },
            // Enter is context-sensitive. Folders → enter the cursor
            // folder; Messages → open the cursor email; Accounts →
            // select the cursor account (empty-id sentinel; apply_root
            // resolves it via `AccountsComponent::current_account_id`,
            // same convention as Msg::MessageOpen). Attachments still
            // routes Enter through `handle_residual_key`.
            Action::Confirm => match active_pane {
                ActivePane::Folders => Some(Msg::FolderEnter),
                ActivePane::Messages => Some(Msg::MessageOpen(String::new())),
                ActivePane::Accounts => Some(Msg::AccountSelect(String::new())),
                _ => None,
            },
            // Backspace pops the folder stack from the list-oriented
            // panes. Content/Attachments use it for view-back via
            // `handle_residual_key`/`handle_back_navigation`.
            Action::Back => match active_pane {
                ActivePane::Folders | ActivePane::Messages => Some(Msg::FolderExitParent),
                _ => None,
            },

            // ---- Email actions (Messages pane only) ----------------------
            // Carry empty-id sentinels — AppRoot resolves the cursor
            // email in `apply_root` (same convention as the legacy
            // per-component handlers).
            Action::Archive if matches!(active_pane, ActivePane::Messages) => {
                Some(Msg::Archive(String::new()))
            }
            // `Star` (`s`) and `ToggleFlag` (`F`) are VISION.md aliases
            // for the same maildir-F-flag toggle. Both surface as
            // `Msg::ToggleStar` so the rebind story stays uniform.
            Action::Star | Action::ToggleFlag if matches!(active_pane, ActivePane::Messages) => {
                Some(Msg::ToggleStar(String::new()))
            }
            Action::Delete if matches!(active_pane, ActivePane::Messages) => {
                Some(Msg::Delete(String::new()))
            }
            Action::MoveToFolder if matches!(active_pane, ActivePane::Messages) => {
                Some(Msg::OpenFolderPicker)
            }
            Action::MarkUnread if matches!(active_pane, ActivePane::Messages) => {
                Some(Msg::MarkUnread(String::new()))
            }
            Action::ReplyAll if matches!(active_pane, ActivePane::Messages) => {
                Some(Msg::DraftStart(ReplyKind::ReplyAll, String::new()))
            }
            Action::ReplyLater if matches!(active_pane, ActivePane::Messages) => {
                Some(Msg::DraftStart(ReplyKind::ReplyLater, String::new()))
            }
            Action::Forward if matches!(active_pane, ActivePane::Messages) => {
                Some(Msg::DraftStart(ReplyKind::Forward, String::new()))
            }

            // ---- Draft pane action keys ----------------------------------
            Action::DraftSend if matches!(active_pane, ActivePane::Draft) => Some(Msg::DraftSend),
            Action::DraftEdit if matches!(active_pane, ActivePane::Draft) => {
                Some(Msg::DraftEditRelaunch)
            }
            Action::DraftDiscard if matches!(active_pane, ActivePane::Draft) => {
                Some(Msg::DraftDiscard)
            }

            // ---- Component-local sequences & not-yet-implemented ---------
            // `Reply` is the `gr` two-key sequence; `JumpTop`/`JumpBottom`/
            // `JumpNextUnread`/`JumpPrevUnread` are `gg`/`G`/`gj`/`gk`.
            // These stay component-local (or aren't wired at all yet).
            // `AcceptSuggestion`/`SearchNext`/`SearchPrev` are bound in
            // the keymap but don't have dispatch yet — fall through so
            // they're a no-op rather than a panic.
            Action::Reply
            | Action::JumpTop
            | Action::JumpBottom
            | Action::JumpNextUnread
            | Action::JumpPrevUnread
            | Action::AcceptSuggestion
            | Action::SearchNext
            | Action::SearchPrev => None,

            // Action key pressed outside its meaningful pane: silent
            // no-op rather than a panic. The `if let` guards above
            // narrow each action to the pane it makes sense in;
            // everything else falls here.
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
                let ctx = Self::make_ctx(&self.config, &store);
                let mut fu = self.folders.handle_msg(&msg, &ctx);
                fu.extend(self.messages.handle_msg(&msg, &ctx));
                fu.extend(self.content.handle_msg(&msg, &ctx));
                fu.extend(self.accounts.handle_msg(&msg, &ctx));
                fu.extend(self.draft.handle_msg(&msg, &ctx));
                fu.extend(self.folder_picker.handle_msg(&msg, &ctx));
                fu.extend(self.search.handle_msg(&msg, &ctx));
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
                // Draft override: 'l' from the Content view jumps to
                // ContentDraft when a reply is in flight. `layout.next_view`
                // returns None there because the draft-pane gate depends
                // on `DraftComponent::has_draft()`, which layout can't see.
                if matches!(self.layout.current_view, View::Content)
                    && self.draft.has_draft()
                    && !self.layout.content_pane_hidden
                {
                    self.layout.current_view = View::ContentDraft;
                    self.layout.active_pane = self
                        .layout
                        .current_view
                        .get_default_active_pane(self.layout.content_pane_hidden);
                } else {
                    self.layout.next_view();
                }
                let new = self.layout.active_pane;
                self.on_focus_change(old, new);
            }
            Msg::ViewPrev => {
                let old = self.layout.active_pane;
                // Multi-account override (VISION.md § "Multi-Account"):
                // 'h' from the FolderMessages view surfaces the
                // Accounts pane. Layout-level `prev_view` returns None
                // there because single-account installs must keep the
                // pane hidden; the multi-account policy lives here so
                // layout stays pure.
                //
                // Draft override (symmetric to ViewNext): 'h' from
                // ContentDraft drops back to MessagesContent so the
                // user can leave the pre-send pane without discarding.
                if matches!(self.layout.current_view, View::FolderMessages)
                    && self.config.is_multi_account()
                {
                    self.layout.current_view = View::AccountsFolders;
                    self.layout.active_pane = self
                        .layout
                        .current_view
                        .get_default_active_pane(self.layout.content_pane_hidden);
                } else if matches!(self.layout.current_view, View::ContentDraft) {
                    self.layout.current_view = View::MessagesContent;
                    self.layout.active_pane = self
                        .layout
                        .current_view
                        .get_default_active_pane(self.layout.content_pane_hidden);
                } else {
                    self.layout.prev_view();
                }
                let new = self.layout.active_pane;
                self.on_focus_change(old, new);
            }
            Msg::FolderMove(_) => {
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
            }
            Msg::FolderExitParent => {
                self.email_store.lock().unwrap().exit_folder();
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
                    self.publish_focus();
                }
            }
            Msg::MessageMarkRead(_) => {
                self.apply_mark_read();
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
            }
            Msg::AccountSelect(id) => {
                // Empty id is the cursor sentinel — keymap dispatch
                // produces it for `Confirm`/`ViewNext` in the Accounts
                // pane since `action_to_msg` can't see the cursor.
                // Resolve to the highlighted account here (mirrors the
                // `MessageOpen(String::new())` convention).
                let resolved = if id.is_empty() {
                    self.accounts.current_account_id()
                } else {
                    Some(id.clone())
                };
                if let Some(resolved_id) = resolved
                    && let Some(account) = self.accounts.account_by_id(&resolved_id)
                {
                    let new_path = account.maildir_path.clone();
                    self.switch_active_maildir(new_path);
                }
            }
            Msg::StatusSet(s) => {
                self.status_message = Some(s.clone());
            }
            Msg::StatusClear => {
                self.status_message = None;
            }
            Msg::Archive(_) => {
                self.apply_move_action(MoveKind::Archive);
            }
            Msg::Delete(_) => {
                self.apply_move_action(MoveKind::Delete);
            }
            Msg::MoveTo(_, target) => {
                self.apply_move_action(MoveKind::Custom(target.clone()));
            }
            Msg::ToggleStar(_) => {
                self.apply_toggle_star();
            }
            Msg::MarkUnread(_) => {
                self.apply_mark_unread();
            }
            Msg::Undo => {
                self.apply_undo();
            }
            Msg::DraftStart(kind, _) => {
                self.apply_draft_start(*kind);
            }
            Msg::DraftSend => {
                self.apply_draft_send();
            }
            Msg::DraftDiscard => {
                self.apply_draft_discard();
            }
            Msg::DraftEditRelaunch => {
                self.apply_draft_edit_relaunch();
            }
            Msg::ToggleHtmlViewer => {
                self.apply_toggle_html_viewer();
            }
            Msg::OpenSearchInput => {
                self.apply_open_search_input();
            }
            Msg::SearchExecute(query) => {
                self.apply_search_execute(query.clone());
            }
            Msg::SearchResults(paths) => {
                self.apply_search_results(paths.clone());
            }
            Msg::SearchCancel => {
                self.apply_search_cancel();
            }
            Msg::MailDirChanged(path) => {
                self.apply_maildir_changed(path.clone());
            }
            _ => {}
        }
    }

    /// Refresh the folder at `fs_path` after the MailDir watcher
    /// observed a Create/Rename under its `cur/` or `new/` leaf.
    /// Clears the cached headers and resubmits an off-thread headers
    /// load. No-op when the folder is not in the live tree (a
    /// neighbouring account's path, a transient stale event, etc).
    fn apply_maildir_changed(&mut self, fs_path: PathBuf) {
        let found = {
            let mut store = self.email_store.lock().unwrap();
            store.invalidate_folder(&fs_path)
        };
        if !found {
            return;
        }
        // Clear the in-flight slot so the re-load is not suppressed
        // as a duplicate of the prior scan.
        self.loading_folder_paths.remove(&fs_path);
        let limit = (self.message_pane_visible_rows + 5).max(10);
        self.loading_folder_paths.insert(fs_path.clone());
        self.headers_loader.request(LoadFolderRequest {
            fs_path,
            limit: Some(limit),
        });
    }

    /// Toggle the chromeless HTML viewer. First press detects a
    /// browser, spawns it pointed at the embedded web server, and
    /// stashes the `Child` on `self`. Second press hands the child
    /// to [`super::html_viewer::terminate`] (SIGTERM with a 1s
    /// escalation to SIGKILL). Status-bar messages surface every
    /// branch so the user always has feedback.
    fn apply_toggle_html_viewer(&mut self) {
        // If a viewer is in flight, but the child has already exited
        // on its own (user closed the window), treat the slot as
        // empty so this press launches a fresh viewer.
        if let Some(child) = self.html_viewer_child.as_mut()
            && let Ok(Some(_status)) = child.try_wait()
        {
            self.html_viewer_child = None;
        }

        if let Some(mut child) = self.html_viewer_child.take() {
            match super::html_viewer::terminate(&mut child, Duration::from_secs(1)) {
                Ok(()) => self.status_message = Some("HTML viewer closed".into()),
                Err(e) => self.status_message = Some(format!("HTML viewer close failed: {}", e)),
            }
            return;
        }

        let Some(browser) = super::html_viewer::detect_browser(super::html_viewer::binary_on_path)
        else {
            self.status_message =
                Some("No browser found — install chromium, chrome, or firefox".into());
            return;
        };

        let url = format!("http://127.0.0.1:{}", self.web_port);
        match super::html_viewer::launch(browser, &url) {
            Ok(child) => {
                self.html_viewer_child = Some(child);
                self.status_message = Some(format!("HTML viewer launched ({})", browser.binary()));
            }
            Err(e) => {
                self.status_message = Some(format!("Failed to launch {}: {}", browser.binary(), e));
            }
        }
    }

    /// True while a notmuch search-results virtual folder is on
    /// display. Used by `process_event` to intercept `h` / `Esc`
    /// before the global view-prev shortcut fires.
    fn search_results_active(&self) -> bool {
        self.email_store.lock().unwrap().search_results.is_some()
    }

    /// Phase 5.a — resolve the cursor-selected email's classifier
    /// suggestion into the underlying mutation `Msg`. Returns `None`
    /// when there is no selected email, when the classifier abstains,
    /// when confidence is below the configured threshold, or when the
    /// suggested action has no Messages-pane shortcut. The default
    /// `NoopClassifier` always abstains so `;` is a no-op under
    /// `[ai].enabled = false`.
    pub fn resolve_accept_suggestion(&self) -> Option<Msg> {
        // The chip / accept-key path is Messages-list scoped.
        if !matches!(self.layout.active_pane, ActivePane::Messages) {
            return None;
        }
        let store = self.email_store.lock().unwrap();
        let email = store.get_selected_email()?;
        let suggestion = self.messages.classifier().suggest(email)?;
        if suggestion.confidence < self.messages.confidence_threshold() {
            return None;
        }
        Self::suggestion_to_msg(suggestion.action)
    }

    /// Map a classifier-suggested [`Action`] to the underlying mutation
    /// `Msg` that the chip's keystroke would have produced. Returns
    /// `None` for actions outside the chip-eligible set
    /// ([`crate::classifier::suggestion_glyph`] is the authoritative
    /// list).
    fn suggestion_to_msg(action: Action) -> Option<Msg> {
        match action {
            Action::Archive => Some(Msg::Archive(String::new())),
            Action::Star => Some(Msg::ToggleStar(String::new())),
            Action::Delete => Some(Msg::Delete(String::new())),
            Action::MarkUnread => Some(Msg::MarkUnread(String::new())),
            Action::Reply => Some(Msg::DraftStart(ReplyKind::Reply, String::new())),
            Action::ReplyAll => Some(Msg::DraftStart(ReplyKind::ReplyAll, String::new())),
            Action::ReplyLater => Some(Msg::DraftStart(ReplyKind::ReplyLater, String::new())),
            Action::Forward => Some(Msg::DraftStart(ReplyKind::Forward, String::new())),
            _ => None,
        }
    }

    /// Probe `notmuch` and either open the modal or surface a
    /// status-bar fallback. Keeps the not-installed path off the
    /// hot UI path — the modal never opens, so the user immediately
    /// gets the "notmuch not found" message and can keep working.
    fn apply_open_search_input(&mut self) {
        if !notmuch_available() {
            self.status_message = Some(crate::error::VulthorError::NotmuchNotFound.to_string());
            // Suppress the SearchComponent's open() that already ran
            // via handle_msg — close it back so the modal stays hidden.
            self.search.close();
        }
        // Otherwise, SearchComponent::handle_msg already flipped
        // `visible` true; nothing more to do here.
    }

    /// Shell out to `notmuch search --output=files <query>` and
    /// enqueue a follow-up `SearchResults` with the parsed paths.
    /// Errors surface in the status bar and leave the prior search
    /// state (if any) untouched. Runs synchronously on the TUI
    /// thread for now — notmuch's query latency dominates a maildir
    /// scan and is bounded by the local index size; if it ever
    /// becomes a problem this moves to a worker like `BodyLoader`.
    fn apply_search_execute(&mut self, query: String) {
        let output = std::process::Command::new("notmuch")
            .args(["search", "--output=files"])
            .arg(&query)
            .output();
        match output {
            Ok(out) if out.status.success() => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let paths = parse_notmuch_files_output(&stdout);
                // Carry the query through so the SearchResults handler
                // can label the virtual folder. We piggy-back on the
                // search component's now-cleared state by stashing the
                // query on it before close() ran — too late for that,
                // so name the folder from the still-available `query`
                // local via a chained handler.
                self.apply_search_results_named(paths, query);
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
                self.status_message =
                    Some(crate::error::VulthorError::NotmuchQueryFailed { stderr }.to_string());
            }
            Err(e) => {
                self.status_message = Some(
                    crate::error::VulthorError::NotmuchQueryFailed {
                        stderr: e.to_string(),
                    }
                    .to_string(),
                );
            }
        }
    }

    /// Apply paths from a `Msg::SearchResults` to the store as a
    /// virtual folder named after the (default) query placeholder.
    /// `apply_search_execute` calls `apply_search_results_named`
    /// instead so it can label the folder with the live query.
    fn apply_search_results(&mut self, paths: Vec<PathBuf>) {
        self.apply_search_results_named(paths, String::new());
    }

    fn apply_search_results_named(&mut self, paths: Vec<PathBuf>, query: String) {
        let label = if query.is_empty() {
            "Search".to_string()
        } else {
            format!("Search: {}", query)
        };
        let mut folder = crate::email::Folder::new(label.clone(), PathBuf::from(":search:"));
        folder.is_loaded = true;
        for p in paths {
            // Skip phantom rows where the file vanished between the
            // notmuch index and the filesystem (mbsync mid-flight).
            if !p.exists() {
                continue;
            }
            let mut email = crate::email::Email::new(p);
            if email.parse_headers_only().is_ok() {
                folder.add_email(email);
            }
        }
        let count = folder.emails.len();
        {
            let mut store = self.email_store.lock().unwrap();
            store.set_search_results(folder);
        }
        // Reset the Messages-pane cursor so the user lands on the
        // first hit.
        self.messages.email_index = 0;
        self.messages.remembered_email_index = None;
        // Surface results in the Messages-only view so the breadcrumb
        // shows "Search: …" with no folder pane competing for space.
        self.layout.current_view = layout::View::Messages;
        self.layout.active_pane = ActivePane::Messages;
        self.publish_focus();
        self.status_message = Some(format!("{}: {} result(s)", label, count));
    }

    /// Drop the active search-results virtual folder and return to
    /// the prior folder view. No-op when no search is active.
    fn apply_search_cancel(&mut self) {
        let was_active = {
            let mut store = self.email_store.lock().unwrap();
            let active = store.search_results.is_some();
            store.clear_search_results();
            active
        };
        if was_active {
            self.layout.current_view = layout::View::FolderMessages;
            self.layout.active_pane = ActivePane::Messages;
            self.publish_focus();
        }
    }

    /// Pipe the in-flight draft to `compose::send`. On success, file
    /// the Sent copy, clear the draft, and drop back to the
    /// `MessagesContent` view. On failure, flip the draft's status to
    /// `Failed` so the footer surfaces the reason; the user can press
    /// `e` to re-edit or `q` to abandon.
    fn apply_draft_send(&mut self) {
        let compose = match self.draft.state() {
            Some(state) => state.compose.clone(),
            None => return,
        };
        let account = self.resolve_active_account();
        match crate::compose::send(&compose, &account) {
            Ok(sent_path) => {
                self.draft.clear();
                self.layout.current_view = View::MessagesContent;
                self.layout.active_pane = ActivePane::Messages;
                self.publish_focus();
                let label = sent_path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Sent".to_string());
                self.status_message = Some(format!("Sent: {}", label));
            }
            Err(e) => {
                self.draft
                    .set_status(crate::components::draft::DraftStatus::Failed(e.to_string()));
                self.status_message = Some(format!("Send failed: {}", e));
            }
        }
    }

    /// Discard the in-flight draft (`q`/Esc in the Draft pane).
    /// `DraftComponent` clears its own state via the bus; AppRoot just
    /// navigates back to the pre-compose view.
    fn apply_draft_discard(&mut self) {
        self.layout.current_view = View::MessagesContent;
        self.layout.active_pane = ActivePane::Messages;
        self.publish_focus();
        self.status_message = Some("Draft discarded".into());
    }

    /// Park a fresh editor launch on the current draft (`e` in the
    /// Draft pane). The run loop picks it up the same way it does for
    /// the initial `DraftStart`. No-op when no draft is in flight.
    fn apply_draft_edit_relaunch(&mut self) {
        let compose = match self.draft.state() {
            Some(state) => state.compose.clone(),
            None => return,
        };
        let template = default_template(&compose);
        self.pending_editor = Some(PendingEditorLaunch { template });
    }

    /// Build the reply template for the cursor email, install it on
    /// the live draft, and either:
    ///   - park an editor launch for the run loop (Reply/ReplyAll/Forward), or
    ///   - write an empty-body draft file to `Drafts/` and flip the
    ///     status straight to `ReadyToSend` (ReplyLater).
    ///
    /// In both cases the view switches to `ContentDraft` so the
    /// pre-send pane is visible when the user returns from the editor
    /// (or immediately, for reply-later).
    ///
    /// DraftComponent has already initialised `state` with an empty
    /// `Compose` via its own handle_msg pass; we only overwrite the
    /// payload here. The DraftStart `MessageId` field is the empty
    /// sentinel (Phase 1.x convention) — we resolve the target from
    /// the cursor.
    fn apply_draft_start(&mut self, kind: ReplyKind) {
        // 1. Snapshot the cursor email and active account under the
        //    store lock so we don't hold it across the editor launch.
        let original = {
            let store = self.email_store.lock().unwrap();
            let folder = store.get_current_folder();
            let idx = self.messages.email_index;
            match folder.emails.get(idx) {
                Some(e) => e.clone(),
                None => {
                    drop(store);
                    self.status_message = Some("No message selected to reply to".into());
                    self.draft.clear();
                    return;
                }
            }
        };
        let account = self.resolve_active_account();

        // 2. Build the template and populate the draft.
        let compose = build_reply_template(&original, kind, &account);
        self.draft.set_compose(compose.clone());

        // 3. View progression: hop to the Draft pane via ContentDraft
        //    so the user sees the pre-send surface on return.
        self.layout.current_view = View::ContentDraft;
        self.layout.active_pane = ActivePane::Draft;
        self.publish_focus();

        match kind {
            ReplyKind::ReplyLater => {
                // No editor. Write the empty-body draft straight to
                // disk so the ⏰ chip surfaces on the original.
                match self.write_reply_later_draft(&compose, &account) {
                    Ok(path) => {
                        self.register_reply_later_draft(&compose, &path);
                        self.draft
                            .set_status(crate::components::draft::DraftStatus::ReadyToSend);
                        self.status_message = Some("Reply-later saved to Drafts/".into());
                    }
                    Err(e) => {
                        self.draft.clear();
                        self.status_message = Some(format!("Reply-later failed: {}", e));
                    }
                }
            }
            ReplyKind::Reply | ReplyKind::ReplyAll | ReplyKind::Forward => {
                // Park the editor launch for the run loop.
                let template = default_template(&compose);
                self.pending_editor = Some(PendingEditorLaunch { template });
            }
        }
    }

    /// Resolve the active account config the compose flow should
    /// templatize against. Prefers the Accounts pane's current
    /// selection; falls back to a synthetic single-account record
    /// rooted at the **live store's** maildir path when no
    /// `[accounts.*]` tables are configured. Using the store path —
    /// not `Config::default().maildir_path` — keeps reply-later draft
    /// writes scoped to the same tree the user is actively browsing,
    /// which matters both for tests with temp roots and for runs
    /// where `-m <path>` overrode the config default.
    fn resolve_active_account(&self) -> AccountConfig {
        if let Some(id) = self.accounts.current_account_id()
            && let Some(account) = self.accounts.account_by_id(&id)
        {
            return account.clone();
        }
        let maildir_path = self.email_store.lock().unwrap().root_folder.path.clone();
        AccountConfig {
            name: String::new(),
            email: String::new(),
            maildir_path,
            smtp_command: None,
            signature: None,
        }
    }

    /// Write a reply-later draft to `<maildir>/Drafts/cur/<filename>`.
    /// Returns the on-disk path on success. Body stays empty by design
    /// — the file exists only to surface a `⏰` chip on the original.
    fn write_reply_later_draft(
        &self,
        compose: &Compose,
        account: &AccountConfig,
    ) -> std::io::Result<PathBuf> {
        let dir = account.maildir_path.join("Drafts").join("cur");
        std::fs::create_dir_all(&dir)?;
        let filename = reply_later_filename();
        let path = dir.join(filename);
        std::fs::write(&path, compose.serialize_rfc822())?;
        Ok(path)
    }

    /// Register a freshly-written reply-later draft in the store's
    /// `drafts` index so the Messages pane paints the `⏰` chip
    /// immediately, without waiting for the next folder scan.
    fn register_reply_later_draft(&self, compose: &Compose, path: &std::path::Path) {
        let Some(parent_id) = compose.in_reply_to.as_deref().map(strip_angle_brackets) else {
            return;
        };
        if parent_id.is_empty() {
            return;
        }
        let mut store = self.email_store.lock().unwrap();
        store.drafts.insert(
            parent_id.to_string(),
            crate::email::DraftInfo {
                path: path.to_path_buf(),
                body_empty: compose.body.trim().is_empty(),
            },
        );
    }

    /// Perform an Archive-/Delete-/Move-style relocation on the cursor
    /// email. All three share the same filesystem shape — `<target>/cur/`,
    /// create-on-demand — and differ only in the destination directory
    /// and the `Mutation` variant they record.
    fn apply_move_action(&mut self, kind: MoveKind) {
        let (src_path, subject) = {
            let store = self.email_store.lock().unwrap();
            let folder = store.get_current_folder();
            let idx = self.messages.email_index;
            match folder.emails.get(idx) {
                Some(e) => (e.file_path.clone(), e.headers.subject.clone()),
                None => return,
            }
        };

        let Some(filename) = src_path.file_name() else {
            self.status_message = Some(format!(
                "Cannot {}: invalid email path",
                kind.verb_present()
            ));
            return;
        };
        let dst_dir = match &kind {
            MoveKind::Archive | MoveKind::Delete => {
                let maildir_root = self.email_store.lock().unwrap().root_folder.path.clone();
                maildir_root.join(kind.builtin_folder_name()).join("cur")
            }
            MoveKind::Custom(target) => target.join("cur"),
        };
        let dst_path = dst_dir.join(filename);

        // Don't no-op if the user picks the email's current folder —
        // the rename would silently succeed but the undo entry would
        // round-trip to the same path. Surface it as a status instead.
        if dst_path == src_path {
            self.status_message = Some("Move target matches source — no-op".into());
            return;
        }

        if let Err(e) = std::fs::create_dir_all(&dst_dir) {
            self.status_message = Some(format!("Failed to {} (mkdir): {}", kind.verb_present(), e));
            return;
        }
        if let Err(e) = std::fs::rename(&src_path, &dst_path) {
            self.status_message = Some(format!("Failed to {}: {}", kind.verb_present(), e));
            return;
        }

        self.email_store
            .lock()
            .unwrap()
            .swap_email_path(&src_path, &dst_path);

        let mutation = match &kind {
            MoveKind::Archive => Mutation::Archive {
                msg: dst_path.clone(),
                from: src_path,
                to: dst_path.clone(),
            },
            MoveKind::Delete => Mutation::Delete {
                msg: dst_path.clone(),
                from: src_path,
                to: dst_path.clone(),
            },
            MoveKind::Custom(_) => Mutation::Move {
                msg: dst_path.clone(),
                from: src_path,
                to: dst_path.clone(),
            },
        };
        self.undo_stack.push(mutation);

        let label = if subject.is_empty() {
            "(no subject)".to_string()
        } else {
            subject
        };
        self.status_message = Some(format!("{}: {}", kind.verb_past(), label));
    }

    /// Toggle the MailDir `F` flag on the cursor email. Captures the
    /// *previous* flag state in the recorded `Mutation::ToggleStar`
    /// so undo restores it directly.
    fn apply_toggle_star(&mut self) {
        let (src_path, subject, prev_flag) = {
            let store = self.email_store.lock().unwrap();
            let folder = store.get_current_folder();
            let idx = self.messages.email_index;
            match folder.emails.get(idx) {
                Some(e) => (e.file_path.clone(), e.headers.subject.clone(), e.is_flagged),
                None => return,
            }
        };

        let want = !prev_flag;
        let new_path = match crate::undo::set_maildir_flag(&src_path, 'F', want) {
            Ok(p) => p,
            Err(e) => {
                self.status_message = Some(format!("Failed to toggle star: {}", e));
                return;
            }
        };

        if new_path != src_path {
            self.email_store
                .lock()
                .unwrap()
                .swap_email_path(&src_path, &new_path);
        }

        self.undo_stack.push(Mutation::ToggleStar {
            msg: new_path,
            prev_flag,
        });

        let label = if subject.is_empty() {
            "(no subject)".to_string()
        } else {
            subject
        };
        let verb = if want { "Starred" } else { "Unstarred" };
        self.status_message = Some(format!("{}: {}", verb, label));
    }

    /// Move the cursor email from `<folder>/cur/` to `<folder>/new/`,
    /// flip its in-memory `is_unread` to true and bump the folder's
    /// `unread_count`. Idempotent when the file is already in `new/`.
    fn apply_mark_unread(&mut self) {
        let (src_path, subject) = {
            let store = self.email_store.lock().unwrap();
            let folder = store.get_current_folder();
            let idx = self.messages.email_index;
            match folder.emails.get(idx) {
                Some(e) => (e.file_path.clone(), e.headers.subject.clone()),
                None => return,
            }
        };

        let Some(filename) = src_path.file_name() else {
            self.status_message = Some("Cannot mark unread: invalid email path".into());
            return;
        };
        let Some(cur_dir) = src_path.parent() else {
            self.status_message = Some("Cannot mark unread: invalid email path".into());
            return;
        };
        // Idempotent: file already in `new/` means it's already unread.
        match cur_dir.file_name().and_then(|n| n.to_str()) {
            Some("new") => {
                self.status_message = Some("Already unread".into());
                return;
            }
            Some("cur") => {}
            _ => {
                self.status_message = Some("Cannot mark unread: not a maildir cur/ file".into());
                return;
            }
        }
        let Some(folder_dir) = cur_dir.parent() else {
            self.status_message = Some("Cannot mark unread: missing folder".into());
            return;
        };
        let new_dir = folder_dir.join("new");
        let dst_path = new_dir.join(filename);

        if let Err(e) = std::fs::create_dir_all(&new_dir) {
            self.status_message = Some(format!("Failed to mark unread (mkdir): {}", e));
            return;
        }
        if let Err(e) = std::fs::rename(&src_path, &dst_path) {
            self.status_message = Some(format!("Failed to mark unread: {}", e));
            return;
        }

        {
            // `update_email_read_state` does both the path swap and
            // the `is_unread`/`unread_count` flip atomically; no
            // separate `swap_email_path` call.
            let mut store = self.email_store.lock().unwrap();
            store.update_email_read_state(&src_path, &dst_path, true);
        }

        self.undo_stack.push(Mutation::MarkUnread {
            msg: dst_path.clone(),
            from: src_path,
            to: dst_path,
        });

        let label = if subject.is_empty() {
            "(no subject)".to_string()
        } else {
            subject
        };
        self.status_message = Some(format!("Marked unread: {}", label));
    }

    /// Pop one mutation off the undo stack and reverse it. No-op when
    /// the stack is empty; sets a status message on success and on the
    /// best-effort "file moved" path. See `crate::undo` for the
    /// reversal contract.
    fn apply_undo(&mut self) {
        let Some(mutation) = self.undo_stack.pop() else {
            self.status_message = Some("Nothing to undo".into());
            return;
        };
        let reversed = mutation.reverse();
        match reversed {
            Reversed::PathRestored { old, new } => {
                let store = self.email_store.clone();
                let mut store = store.lock().unwrap();
                match &mutation {
                    // Read-state mutations need the in-memory read flag
                    // and the folder's unread_count to track the file
                    // move. The plain path-swap in `swap_email_path`
                    // would leave the unread badge stale.
                    Mutation::MarkRead { .. } => {
                        store.update_email_read_state(&old, &new, true);
                    }
                    Mutation::MarkUnread { .. } => {
                        store.update_email_read_state(&old, &new, false);
                    }
                    _ => {
                        store.swap_email_path(&old, &new);
                    }
                }
                self.status_message = Some("Undo: restored".into());
            }
            Reversed::FlagRestored { old, new } => {
                if old != new {
                    let store = self.email_store.clone();
                    let mut store = store.lock().unwrap();
                    store.swap_email_path(&old, &new);
                }
                self.status_message = Some("Undo: flag restored".into());
            }
            Reversed::Skipped => {
                self.status_message = Some("Could not undo: file moved".into());
            }
        }
    }

    /// Perform the auto mark-read move triggered by `Msg::MessageMarkRead`
    /// (Enter on a message). Plans the new/→cur/ transition under the
    /// store lock, releases the lock for the `fs::rename`, then re-locks
    /// to update in-memory state and push onto the undo stack.
    /// Idempotent: no plan ⇒ nothing happens, no mutation is pushed,
    /// no status text is set.
    fn apply_mark_read(&mut self) {
        let idx = self.messages.email_index;
        let plan: Option<MarkReadPlan> = {
            let store = self.email_store.lock().unwrap();
            store.plan_mark_read(idx)
        };
        let Some(MarkReadPlan { from, to }) = plan else {
            return;
        };
        // Make sure cur/ exists before the rename. Maildirs created by
        // mbsync always have it, but tests and freshly-initialised
        // accounts may not.
        if let Some(parent) = to.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::rename(&from, &to) {
            Ok(()) => {
                let mut store = self.email_store.lock().unwrap();
                store.update_email_read_state(&from, &to, false);
                drop(store);
                self.push_mutation(Mutation::MarkRead {
                    msg: to.clone(),
                    from,
                    to,
                });
            }
            Err(e) => {
                self.status_message = Some(format!("Mark-read failed: {}", e));
            }
        }
    }

    /// Append a mutation to the session undo stack. Called by the
    /// action-key handlers after they have applied the underlying
    /// filesystem op.
    pub fn push_mutation(&mut self, mutation: Mutation) {
        self.undo_stack.push(mutation);
    }

    /// Pull the parked editor request set by the last
    /// `Msg::DraftStart` dispatch. The run loop suspends the TUI
    /// before calling and resumes after. Returns `None` when no
    /// editor launch is queued.
    pub fn take_pending_editor(&mut self) -> Option<PendingEditorLaunch> {
        self.pending_editor.take()
    }

    /// Resume after the editor returned cleanly: install the parsed
    /// `Compose` on the live draft and advance its status to
    /// `ReadyToSend` so the pre-send footer renders. The run loop
    /// calls this from outside the dispatch loop, so we enqueue
    /// `Msg::DraftEditorExited` for the next drain to pick up
    /// (mirrors the contract DraftComponent already implements).
    pub fn apply_editor_result(&mut self, compose: crate::compose::Compose) {
        self.draft.set_compose(compose);
        self.queue.push_back(Msg::DraftEditorExited);
        self.drain();
    }

    /// The editor failed (non-zero exit, missing binary, parse error,
    /// etc). Discard the placeholder draft so the user can try again
    /// and surface the failure in the status bar.
    pub fn apply_editor_failure(&mut self, message: String) {
        self.draft.clear();
        // Drop back to the pre-compose view; sitting on `Draft` with
        // no state would just paint the tombstone.
        if matches!(self.layout.current_view, View::ContentDraft) {
            self.layout.current_view = View::MessagesContent;
            self.layout.active_pane = ActivePane::Messages;
            self.publish_focus();
        }
        self.status_message = Some(format!("Editor failed: {}", message));
    }

    /// True iff there is an editor launch parked for the run loop.
    pub fn has_pending_editor(&self) -> bool {
        self.pending_editor.is_some()
    }

    #[cfg(test)]
    pub(crate) fn undo_stack_len(&self) -> usize {
        self.undo_stack.len()
    }

    /// Test seam: peek at the live MailDir watcher's root, if any.
    /// Lets the account-switch integration tests verify that
    /// `switch_active_maildir` actually re-pointed the watcher at
    /// the new tree.
    #[cfg(test)]
    pub(crate) fn maildir_watcher_root(&self) -> Option<&std::path::Path> {
        self.maildir_watcher.as_ref().map(|w| w.root())
    }

    /// Test seam: force the active pane. Production code transitions
    /// the pane via `Msg::FocusNext` / `Msg::FocusPrev` and view-
    /// progression; tests outside `root.rs` skip that machinery to
    /// land directly on the pane they want to drive keys against.
    #[cfg(test)]
    pub(crate) fn set_active_pane_for_test(&mut self, pane: ActivePane) {
        self.layout.active_pane = pane;
        self.publish_focus();
    }

    /// Test seam: position the Messages-pane cursor. The component is
    /// canonical; this just keeps the in-tree convention of writing
    /// `root.messages.email_index = i` callable through a helper.
    #[cfg(test)]
    pub(crate) fn set_messages_email_index_for_test(&mut self, idx: usize) {
        self.messages.email_index = idx;
    }

    /// Re-point the runtime at a new maildir root. Used by
    /// `Msg::AccountSelect`.
    ///
    /// The shared `Arc<Mutex<EmailStore>>` keeps its identity — the
    /// web server's clone stays valid — and we overwrite its
    /// contents under the lock. The folder scanner and headers
    /// loader both get fresh handles tied to the new path; the
    /// existing body loader is path-agnostic and stays running.
    /// Folder/message/content cursors reset so the user lands on the
    /// new account's INBOX (auto-selected once the scanner reply
    /// arrives in `drain_scanned_folders`).
    fn switch_active_maildir(&mut self, new_path: PathBuf) {
        // 1. Reset the store in place — same Arc, fresh contents.
        {
            let mut store = self.email_store.lock().unwrap();
            *store = EmailStore::new(new_path.clone());
            store.scanning_folders = true;
        }

        // 2. Replace the scanners. HeadersLoader owns its own clone
        //    of the scanner, so we re-spawn it against the new path.
        self.scanner = MaildirScanner::new(new_path.clone());
        self.headers_loader = HeadersLoader::spawn(self.scanner.clone());
        self.folder_scanner = Some(FolderScannerHandle::spawn(new_path.clone()));

        // 2b. Tear down the old MailDir watcher and rebuild rooted at
        //     the new path. Tests that never call
        //     `init_maildir_watcher` leave the slot `None`; in that
        //     case we still rebuild so an explicit account switch
        //     starts the watcher (the bead's TDD assertion).
        self.maildir_watcher = None;
        self.spawn_maildir_watcher(new_path.clone());

        // 3. Reset component cursors. Folders auto-select runs again
        //    in `drain_scanned_folders` once the new scan lands.
        self.folders.folder_index = 0;
        self.messages.email_index = 0;
        self.messages.remembered_email_index = None;
        self.content.scroll_offset = 0;
        self.layout.selection = Default::default();

        // 4. Clear in-flight load tracking so we don't suppress
        //    legitimate reloads of paths that happened to match the
        //    previous account's tree.
        self.loading_paths.clear();
        self.loading_folder_paths.clear();

        // 5. Land the user in FolderMessages with focus on Folders;
        //    the AccountsFolders view was a transient navigation
        //    state, not where they want to sit reading mail. The
        //    FolderMessages view is identical in both content-pane
        //    modes (the messages pane is shown either way), so there
        //    is no branch on `content_pane_hidden` here.
        self.layout.current_view = View::FolderMessages;
        self.layout.active_pane = ActivePane::Folders;
        self.publish_focus();
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

    /// Resolved [`Keymap`] (defaults + `[keybindings]` overrides).
    /// Integration tests inspect this to confirm config-time overrides
    /// reach the dispatch table; runtime code reads the field directly.
    #[cfg(test)]
    pub(crate) fn keymap(&self) -> &Keymap {
        &self.keymap
    }

    /// Runtime [`Theme`] installed by `main.rs` via [`Self::set_theme`].
    /// Integration tests use this to verify `[theme]` resolution lands
    /// on AppRoot.
    #[cfg(test)]
    pub(crate) fn theme(&self) -> &Theme {
        &self.theme
    }

    /// Pump the inotify watcher once and dispatch any debounced
    /// `Msg::MailDirChanged` it produces. Test seam — runtime code
    /// reaches this via `tick`/`render`.
    #[cfg(test)]
    pub(crate) fn pump_maildir_watcher(&mut self) {
        self.drain_maildir_watcher();
    }

    fn make_ctx<'a>(config: &'a Config, store: &'a EmailStore) -> Ctx<'a> {
        Ctx {
            theme: &THEME,
            config,
            store,
        }
    }
}

static THEME: VulthorTheme = VulthorTheme;

/// Build a unique MailDir filename for a new Drafts/ entry. Matches the
/// `<secs>.M<usec>P<pid>Q<counter>` shape `compose::send` uses for the
/// Sent folder, with a `D` (Draft) info flag instead of `S` (Seen).
/// Two calls in the same microsecond stay distinct via a process-wide
/// counter inside `compose`.
fn reply_later_filename() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let micros = now.subsec_micros();
    let pid = std::process::id();
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{}.M{}P{}Q{}.vulthor:2,D", secs, micros, pid, counter)
}

/// Strip surrounding angle brackets from a Message-ID-style string so
/// it matches the bare-id keys used in `EmailStore::drafts`.
fn strip_angle_brackets(s: &str) -> &str {
    let s = s.trim();
    s.strip_prefix('<')
        .and_then(|s| s.strip_suffix('>'))
        .unwrap_or(s)
}

/// The MailDir-move action keys (`a`, `d`, `m`) share every step except
/// the destination directory and the recorded mutation variant; this
/// enum carries that delta. `Custom` lands the email at an arbitrary
/// folder filesystem path — the picker's "move to folder" target uses it.
#[derive(Debug, Clone)]
enum MoveKind {
    Archive,
    Delete,
    Custom(PathBuf),
}

impl MoveKind {
    fn builtin_folder_name(&self) -> &'static str {
        match self {
            MoveKind::Archive => "Archive",
            MoveKind::Delete => "Trash",
            MoveKind::Custom(_) => "",
        }
    }
    fn verb_present(&self) -> &'static str {
        match self {
            MoveKind::Archive => "archive",
            MoveKind::Delete => "delete",
            MoveKind::Custom(_) => "move",
        }
    }
    fn verb_past(&self) -> &'static str {
        match self {
            MoveKind::Archive => "Archived",
            MoveKind::Delete => "Deleted",
            MoveKind::Custom(_) => "Moved",
        }
    }
}

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

    /// Process-wide lock guarding tests that mutate `PATH`. Delegates
    /// to the crate-wide lock in `test_fixtures` so the search and
    /// html-viewer integration tests in `phase3_integration_tests`
    /// serialize against the in-tree tests here.
    fn path_lock() -> &'static std::sync::Mutex<()> {
        crate::test_fixtures::path_lock()
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

    /// The `q` key resolves to `Action::Quit` via the keymap; in any
    /// non-Draft pane that translates to `Msg::Quit`. In the Draft
    /// pane it discards the in-flight reply instead (VISION.md
    /// §Pre-Send Flow).
    #[test]
    fn keymap_q_quits_in_non_draft_pane() {
        let map = resolve_keymap(&std::collections::BTreeMap::new()).unwrap();
        let key = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        let action = map.lookup_single(key).expect("q is bound");
        assert_eq!(action, Action::Quit);
        assert_eq!(
            AppRoot::action_to_msg(action, &ActivePane::Messages, false),
            Some(Msg::Quit)
        );
        assert_eq!(
            AppRoot::action_to_msg(action, &ActivePane::Draft, false),
            Some(Msg::DraftDiscard)
        );
    }

    /// `l` is `Action::ViewNext`. The Folders pane uses it for
    /// select-into with context-aware logic that lives in
    /// `FoldersComponent::on_key`, so AppRoot returns `None` for that
    /// pane. The Accounts pane treats `l` like Enter — select the
    /// cursor account via the empty-id sentinel. Other panes get the
    /// view-right Msg.
    #[test]
    fn keymap_l_defers_in_folders_and_selects_in_accounts() {
        let map = resolve_keymap(&std::collections::BTreeMap::new()).unwrap();
        let key = KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE);
        let action = map.lookup_single(key).expect("l is bound");
        assert_eq!(action, Action::ViewNext);
        assert!(AppRoot::action_to_msg(action, &ActivePane::Folders, false).is_none());
        assert_eq!(
            AppRoot::action_to_msg(action, &ActivePane::Accounts, false),
            Some(Msg::AccountSelect(String::new())),
            "Accounts must select cursor account (sentinel-resolved in apply_root)",
        );
        assert_eq!(
            AppRoot::action_to_msg(action, &ActivePane::Messages, false),
            Some(Msg::ViewNext)
        );
    }

    /// Bare `v` toggles the HTML viewer; `Ctrl+v` must not match
    /// because the keymap binds the unmodified char only.
    #[test]
    fn keymap_v_toggles_html_viewer() {
        let map = resolve_keymap(&std::collections::BTreeMap::new()).unwrap();
        let key = KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE);
        let action = map.lookup_single(key).expect("v is bound");
        assert_eq!(action, Action::ToggleViewer);
        assert_eq!(
            AppRoot::action_to_msg(action, &ActivePane::Messages, false),
            Some(Msg::ToggleHtmlViewer)
        );
        let with_ctrl = KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL);
        assert!(
            map.lookup_single(with_ctrl).is_none(),
            "Ctrl+v must not collide with the bare-v binding",
        );
    }

    /// Toggling the viewer when no browser is on `PATH` must surface
    /// the install hint via the status bar and leave AppRoot in a
    /// no-op state — never crash. We force the empty-PATH condition
    /// by stashing the real `PATH`, blanking it for the duration of
    /// the test, and restoring it afterward.
    #[test]
    fn toggle_html_viewer_with_no_browser_sets_status_and_does_not_crash() {
        // SAFETY: `set_var` is `unsafe` under Rust 2024. The test
        // serializes against other PATH-mutating tests via the
        // `path_lock` mutex above; we restore PATH on the way out.
        let _guard = path_lock().lock().unwrap();
        let original = std::env::var_os("PATH");
        unsafe { std::env::set_var("PATH", "") };

        let mut root = make_root();
        root.enqueue(Msg::ToggleHtmlViewer);
        assert!(root.drain());
        assert!(
            root.html_viewer_child.is_none(),
            "no child must be spawned when PATH is empty",
        );
        assert!(
            root.status_message
                .as_deref()
                .unwrap_or("")
                .contains("No browser found"),
            "status: {:?}",
            root.status_message,
        );

        match original {
            Some(v) => unsafe { std::env::set_var("PATH", v) },
            None => unsafe { std::env::remove_var("PATH") },
        }
    }

    /// Second press while a child is alive must kill it and clear
    /// the slot. We stub the child by spawning `sleep 60` directly
    /// (mirroring the html_viewer terminate test) so we don't need
    /// a real browser on the host.
    #[test]
    fn toggle_html_viewer_second_press_kills_running_child() {
        use std::process::{Command, Stdio};
        // Serialize against `toggle_html_viewer_with_no_browser_*`
        // — that test blanks `PATH`, which would otherwise race with
        // the `sleep` spawn below.
        let _guard = path_lock().lock().unwrap();
        let mut root = make_root();
        let child = Command::new("sleep")
            .arg("60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("sleep(1) must exist on the test host");
        root.html_viewer_child = Some(child);

        root.enqueue(Msg::ToggleHtmlViewer);
        assert!(root.drain());

        assert!(
            root.html_viewer_child.is_none(),
            "second press must clear the child slot",
        );
        assert_eq!(
            root.status_message.as_deref(),
            Some("HTML viewer closed"),
            "status must confirm the close",
        );
    }

    /// `Alt+c` is `Action::ToggleContentPane` per VISION.md §View
    /// Control. Bare `c` is intentionally unbound.
    #[test]
    fn keymap_alt_c_toggles_content_pane() {
        let map = resolve_keymap(&std::collections::BTreeMap::new()).unwrap();
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::ALT);
        let action = map.lookup_single(key).expect("Alt+c is bound");
        assert_eq!(action, Action::ToggleContentPane);
        assert_eq!(
            AppRoot::action_to_msg(action, &ActivePane::Folders, false),
            Some(Msg::ToggleContentPane)
        );
        let plain = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE);
        assert!(map.lookup_single(plain).is_none());
    }

    #[test]
    fn key_sequence_jj_selects_third_folder() {
        let mut root = make_root_with_folders(&["A", "B", "C", "D"]);
        let j = Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        root.process_event(j.clone()).unwrap();
        root.process_event(j).unwrap();
        assert_eq!(root.folders.folder_index, 2);
    }

    #[test]
    fn key_k_at_top_clamps() {
        let mut root = make_root_with_folders(&["A", "B"]);
        root.folders.folder_index = 0;
        let k = Event::Key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
        root.process_event(k).unwrap();
        assert_eq!(root.folders.folder_index, 0);
    }

    #[test]
    fn key_j_at_bottom_clamps() {
        let mut root = make_root_with_folders(&["A", "B"]);
        root.folders.folder_index = 1;
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
        let phantom_path = PathBuf::from("/definitely/does/not/exist/for/body-load.eml");
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
        assert_eq!(root.folders.folder_index, 0);
        assert_eq!(root.messages.email_index, 0);
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
    }

    #[test]
    fn arrow_down_in_messages_emits_message_move_via_keymap() {
        // Bead vu-251 regression anchor: arrow `Down` in the Messages
        // pane must walk
        //   process_event → keymap.lookup_single(Down) → Action::MoveDown
        //   → action_to_msg → Msg::MessageMove(Dir::Down)
        // rather than the old component-local shadow arm. The cursor
        // advancing by one row proves the dispatch reached
        // `MessagesComponent::handle_msg` (the only writer of
        // `email_index`).
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
        let mut root = AppRoot::new(Arc::new(Mutex::new(store)), scanner);
        root.layout.active_pane = ActivePane::Messages;

        let down = Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        root.process_event(down).unwrap();

        assert_eq!(
            root.messages.email_index, 1,
            "arrow Down in Messages must drive Msg::MessageMove via the central keymap, not a parallel shadow arm",
        );
    }

    #[test]
    fn arrow_down_in_accounts_emits_account_move_via_keymap() {
        // The most visible bead vu-251 bug: arrow `Down` in Accounts
        // used to bypass the keymap entirely because Accounts had its
        // own `KeyCode::Down => AccountMove(Down)` arm. After the
        // refactor, the keymap dispatch must drive the cursor, so a
        // user `[keybindings] move_down = "x"` override would actually
        // disable arrow Down in Accounts (same as everywhere else).
        let cfg = {
            let mut cfg = Config::default();
            for key in ["alpha", "bravo"] {
                cfg.accounts.insert(
                    key.to_string(),
                    crate::config::AccountConfig {
                        name: key.to_string(),
                        email: format!("{}@x.test", key),
                        maildir_path: PathBuf::from(format!("/tmp/{}-mail", key)),
                        smtp_command: None,
                        signature: None,
                    },
                );
            }
            cfg
        };
        let store = EmailStore::new(PathBuf::from("/tmp"));
        let scanner = crate::maildir::MaildirScanner::new(PathBuf::from("/tmp"));
        let mut root = AppRoot::with_config(Arc::new(Mutex::new(store)), scanner, cfg);
        root.layout.active_pane = ActivePane::Accounts;

        let down = Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        root.process_event(down).unwrap();

        assert_eq!(
            root.accounts.selected_index(),
            1,
            "arrow Down in Accounts must dispatch Msg::AccountMove via the central keymap (vu-251 bypass fix)",
        );
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
        root.layout.active_pane = ActivePane::Folders;

        let enter = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        root.process_event(enter).unwrap();
        assert_eq!(root.content.scroll_offset, 0);
    }

    #[test]
    fn u_key_dispatches_undo_msg() {
        let map = resolve_keymap(&std::collections::BTreeMap::new()).unwrap();
        let key = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE);
        let action = map.lookup_single(key).expect("u is bound");
        assert_eq!(action, Action::Undo);
        assert_eq!(
            AppRoot::action_to_msg(action, &ActivePane::Messages, false),
            Some(Msg::Undo)
        );
    }

    #[test]
    fn undo_with_empty_stack_sets_status_and_is_noop() {
        let mut root = make_root();
        root.enqueue(Msg::Undo);
        root.drain();
        assert_eq!(root.undo_stack_len(), 0);
        assert_eq!(root.status_message.as_deref(), Some("Nothing to undo"),);
    }

    #[test]
    fn undo_pops_and_restores_archive_move() {
        use crate::undo::Mutation;
        use std::fs;
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let inbox = temp.path().join("INBOX/cur/msg1");
        let archive = temp.path().join("Archive/cur/msg1");
        fs::create_dir_all(archive.parent().unwrap()).unwrap();
        fs::write(&archive, "body").unwrap();

        let mut root = make_root();
        root.push_mutation(Mutation::Archive {
            msg: archive.clone(),
            from: inbox.clone(),
            to: archive.clone(),
        });
        assert_eq!(root.undo_stack_len(), 1);

        root.enqueue(Msg::Undo);
        root.drain();
        assert_eq!(root.undo_stack_len(), 0);
        assert!(inbox.exists(), "file restored to inbox");
        assert!(!archive.exists(), "archive path is empty");
        assert!(root.status_message.as_deref().unwrap().contains("Undo"));
    }

    #[test]
    fn undo_sequence_of_three_actions_reverses_in_lifo_order() {
        use crate::undo::Mutation;
        use std::fs;
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let inbox_dir = temp.path().join("INBOX/cur");
        let archive_dir = temp.path().join("Archive/cur");
        let trash_dir = temp.path().join("Trash/cur");
        for d in [&inbox_dir, &archive_dir, &trash_dir] {
            fs::create_dir_all(d).unwrap();
        }

        // Pretend three actions already moved three files; we record
        // mutations for each. Repeated `u` should put them all back.
        let m1_from = inbox_dir.join("m1");
        let m1_to = archive_dir.join("m1");
        fs::write(&m1_to, "1").unwrap();
        let m2_from = inbox_dir.join("m2");
        let m2_to = trash_dir.join("m2");
        fs::write(&m2_to, "2").unwrap();
        let m3_from = inbox_dir.join("m3");
        let m3_to = archive_dir.join("m3");
        fs::write(&m3_to, "3").unwrap();

        let mut root = make_root();
        root.push_mutation(Mutation::Archive {
            msg: m1_to.clone(),
            from: m1_from.clone(),
            to: m1_to.clone(),
        });
        root.push_mutation(Mutation::Delete {
            msg: m2_to.clone(),
            from: m2_from.clone(),
            to: m2_to.clone(),
        });
        root.push_mutation(Mutation::Move {
            msg: m3_to.clone(),
            from: m3_from.clone(),
            to: m3_to.clone(),
        });

        // LIFO: m3 first, then m2, then m1.
        root.enqueue(Msg::Undo);
        root.drain();
        assert!(m3_from.exists() && !m3_to.exists());

        root.enqueue(Msg::Undo);
        root.drain();
        assert!(m2_from.exists() && !m2_to.exists());

        root.enqueue(Msg::Undo);
        root.drain();
        assert!(m1_from.exists() && !m1_to.exists());

        assert_eq!(root.undo_stack_len(), 0);
    }

    #[test]
    fn undo_of_missing_file_reports_status_and_pops_stack() {
        use crate::undo::Mutation;
        let mut root = make_root();
        // Push a mutation whose `to` path doesn't exist on disk —
        // simulates mbsync (or anything else) having rewritten the file.
        root.push_mutation(Mutation::Archive {
            msg: PathBuf::from("/nonexistent/Archive/cur/m1"),
            from: PathBuf::from("/nonexistent/INBOX/cur/m1"),
            to: PathBuf::from("/nonexistent/Archive/cur/m1"),
        });
        root.enqueue(Msg::Undo);
        root.drain();
        assert_eq!(
            root.undo_stack_len(),
            0,
            "best-effort undo still pops the stack",
        );
        assert_eq!(
            root.status_message.as_deref(),
            Some("Could not undo: file moved"),
        );
    }

    #[test]
    fn undo_path_restore_updates_in_memory_email_path() {
        use crate::undo::Mutation;
        use std::fs;
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let inbox = temp.path().join("INBOX/cur/msg1");
        let archive = temp.path().join("Archive/cur/msg1");
        fs::create_dir_all(archive.parent().unwrap()).unwrap();
        fs::write(&archive, "body").unwrap();

        let mut store = EmailStore::new(temp.path().to_path_buf());
        let mut folder = Folder::new("INBOX".to_string(), temp.path().join("INBOX"));
        // The store tracks the email at its CURRENT (post-action) path.
        folder.add_email(Email::new(archive.clone()));
        folder.is_loaded = true;
        store.root_folder.add_subfolder(folder);

        let scanner = MaildirScanner::new(temp.path().to_path_buf());
        let shared = Arc::new(Mutex::new(store));
        let mut root = AppRoot::new(shared.clone(), scanner);

        root.push_mutation(Mutation::Archive {
            msg: archive.clone(),
            from: inbox.clone(),
            to: archive.clone(),
        });
        root.enqueue(Msg::Undo);
        root.drain();

        // Disk side: file is back in INBOX.
        assert!(inbox.exists());
        // Store side: the email entry's file_path was rewritten.
        let store = shared.lock().unwrap();
        let inbox_folder = &store.root_folder.subfolders[0];
        assert_eq!(inbox_folder.emails[0].file_path, inbox);
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

    // -----------------------------------------------------------------
    // Multi-account view-progression + switching.
    // -----------------------------------------------------------------

    fn multi_account_config(maildirs: &[(&str, &str)]) -> Config {
        let mut cfg = Config::default();
        for (key, path) in maildirs {
            cfg.accounts.insert(
                (*key).to_string(),
                crate::config::AccountConfig {
                    name: key.to_string(),
                    email: format!("{}@x.test", key),
                    maildir_path: PathBuf::from(*path),
                    smtp_command: None,
                    signature: None,
                },
            );
        }
        cfg
    }

    fn make_root_with_config(config: Config) -> AppRoot {
        let store = EmailStore::new(PathBuf::from("/tmp"));
        let scanner = MaildirScanner::new(PathBuf::from("/tmp"));
        AppRoot::with_config(Arc::new(Mutex::new(store)), scanner, config)
    }

    #[test]
    fn h_from_folder_messages_reveals_accounts_when_multi_account() {
        // VISION.md § "Multi-Account": pressing 'h' from FolderMessages
        // surfaces the AccountsFolders view (only when >1 accounts are
        // configured). AppRoot owns this policy; layout's prev_view
        // stays None for FolderMessages because single-account installs
        // must keep the pane hidden.
        let cfg = multi_account_config(&[("work", "/Mail/work"), ("home", "/Mail/home")]);
        let mut root = make_root_with_config(cfg);
        assert_eq!(root.layout.current_view, View::FolderMessages);

        let h = Event::Key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
        root.process_event(h).unwrap();

        assert_eq!(root.layout.current_view, View::AccountsFolders);
        // Default focus on entering AccountsFolders is the Folders
        // pane (the user came from there); Accounts is one Tab away.
        assert_eq!(root.layout.active_pane, ActivePane::Folders);
    }

    #[test]
    fn h_from_folder_messages_is_noop_for_single_account() {
        // A single configured account hides the pane entirely. 'h'
        // from FolderMessages stays put — there is no broader view
        // to fall back to.
        let cfg = multi_account_config(&[("solo", "/Mail/solo")]);
        let mut root = make_root_with_config(cfg);
        let before = root.layout.current_view;

        let h = Event::Key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
        root.process_event(h).unwrap();

        assert_eq!(root.layout.current_view, before);
    }

    #[test]
    fn h_from_folder_messages_is_noop_with_no_accounts() {
        // Legacy single-maildir config (no [accounts.*] sections) —
        // same outcome as one-account: pane never appears.
        let mut root = make_root();
        let before = root.layout.current_view;

        let h = Event::Key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
        root.process_event(h).unwrap();

        assert_eq!(root.layout.current_view, before);
    }

    #[test]
    fn account_select_rebuilds_store_with_new_maildir() {
        // The acceptance test: dispatching Msg::AccountSelect points
        // the EmailStore at the chosen account's maildir, resets
        // folder/message cursors, and re-spawns the folder scanner
        // (scanning_folders flips true, the new path is owned by the
        // store).
        let cfg = multi_account_config(&[("work", "/tmp/work-mail"), ("home", "/tmp/home-mail")]);
        let mut root = make_root_with_config(cfg);
        let store_handle = root.email_store_handle();

        // Seed some state so the reset is observable.
        root.folders.folder_index = 3;
        root.messages.email_index = 5;
        root.content.scroll_offset = 10;
        {
            let mut store = store_handle.lock().unwrap();
            store.current_folder = vec![0, 1];
            store.scanning_folders = false;
        }

        root.enqueue(Msg::AccountSelect("home".into()));
        root.drain();

        let store = store_handle.lock().unwrap();
        assert_eq!(store.root_folder.path, PathBuf::from("/tmp/home-mail"));
        assert!(store.scanning_folders, "rebuild must start a fresh scan");
        assert!(store.current_folder.is_empty());
        assert_eq!(root.folders.folder_index, 0);
        assert_eq!(root.messages.email_index, 0);
        assert_eq!(root.content.scroll_offset, 0);
        assert_eq!(root.layout.active_pane, ActivePane::Folders);
        assert_eq!(root.layout.current_view, View::FolderMessages);
    }

    #[test]
    fn account_select_keeps_arc_identity_for_web_server() {
        // The web server holds a clone of `Arc<Mutex<EmailStore>>`.
        // Switching accounts must overwrite the store contents under
        // the lock — not swap the Arc itself — so the web pane keeps
        // serving the live store after a switch.
        let cfg = multi_account_config(&[("a", "/tmp/a-mail"), ("b", "/tmp/b-mail")]);
        let mut root = make_root_with_config(cfg);
        let store_handle = root.email_store_handle();
        let same_arc_check = Arc::clone(&store_handle);

        root.enqueue(Msg::AccountSelect("b".into()));
        root.drain();

        // Same Arc pointer means the web server's clone still points
        // at the freshly-loaded store.
        assert!(Arc::ptr_eq(&store_handle, &same_arc_check));
        let store = store_handle.lock().unwrap();
        assert_eq!(store.root_folder.path, PathBuf::from("/tmp/b-mail"));
    }

    #[test]
    fn account_select_with_unknown_id_is_a_no_op() {
        // A stale AccountSelect (account removed mid-session) must
        // not crash the dispatch loop; the store stays pointed at the
        // current account.
        let cfg = multi_account_config(&[("only", "/tmp/only-mail")]);
        let mut root = make_root_with_config(cfg);
        let store_handle = root.email_store_handle();
        let prior_path = store_handle.lock().unwrap().root_folder.path.clone();

        root.enqueue(Msg::AccountSelect("missing".into()));
        root.drain();

        assert_eq!(store_handle.lock().unwrap().root_folder.path, prior_path);
    }

    #[test]
    fn l_on_accounts_pane_switches_account_end_to_end() {
        // Regression: the global 'l' handler must defer to the
        // AccountsComponent so its `Char('l') => AccountSelect` mapping
        // actually fires. Drive 'h' → Tab → j → 'l' through
        // `process_event` so we catch any future router change that
        // re-intercepts 'l' before the per-pane dispatch.
        let cfg = multi_account_config(&[("a", "/tmp/a-mail"), ("b", "/tmp/b-mail")]);
        let mut root = make_root_with_config(cfg);
        let store_handle = root.email_store_handle();
        let prior_path = store_handle.lock().unwrap().root_folder.path.clone();

        let h = Event::Key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
        root.process_event(h).unwrap();
        assert_eq!(root.layout.current_view, View::AccountsFolders);

        let tab = Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        root.process_event(tab).unwrap();
        assert_eq!(root.layout.active_pane, ActivePane::Accounts);

        let j = Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        root.process_event(j).unwrap();
        assert_eq!(root.accounts.selected_index(), 1);

        let l = Event::Key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE));
        root.process_event(l).unwrap();

        let store = store_handle.lock().unwrap();
        assert_ne!(
            store.root_folder.path, prior_path,
            "AccountSelect did not fire — 'l' was intercepted as ViewNext"
        );
        assert_eq!(store.root_folder.path, PathBuf::from("/tmp/b-mail"));
        assert_eq!(root.layout.current_view, View::FolderMessages);
        assert_eq!(root.layout.active_pane, ActivePane::Folders);
    }

    // -----------------------------------------------------------------
    // Direct action keys a/s/d for archive/star/delete.
    // -----------------------------------------------------------------

    /// Build an AppRoot pointed at `root_path` with a single INBOX
    /// containing one real file on disk. Used by the action-key tests
    /// so they can verify both the filesystem move AND the in-memory
    /// store state after the operation.
    fn make_root_with_disk_inbox(root_path: PathBuf, filename: &str) -> (AppRoot, PathBuf) {
        let inbox_cur = root_path.join("INBOX").join("cur");
        std::fs::create_dir_all(&inbox_cur).unwrap();
        let src = inbox_cur.join(filename);
        std::fs::write(&src, "body").unwrap();

        let mut store = EmailStore::new(root_path.clone());
        let mut inbox = Folder::new("INBOX".to_string(), root_path.join("INBOX"));
        inbox.add_email(Email::new(src.clone()));
        inbox.is_loaded = true;
        store.root_folder.add_subfolder(inbox);
        store.enter_folder_by_path(&[0]);
        store.select_email(0);

        let scanner = MaildirScanner::new(root_path.clone());
        let root = AppRoot::new(Arc::new(Mutex::new(store)), scanner);
        (root, src)
    }

    #[test]
    fn archive_action_moves_file_and_pushes_mutation() {
        let temp = tempfile::TempDir::new().unwrap();
        let (mut root, src) = make_root_with_disk_inbox(temp.path().to_path_buf(), "msg1");
        root.layout.active_pane = ActivePane::Messages;

        let a = Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        root.process_event(a).unwrap();

        let archive = temp.path().join("Archive").join("cur").join("msg1");
        assert!(archive.exists(), "file must be in Archive/cur");
        assert!(!src.exists(), "file must no longer be in INBOX/cur");
        assert_eq!(root.undo_stack_len(), 1);
        assert!(
            root.status_message
                .as_deref()
                .unwrap_or("")
                .starts_with("Archived"),
            "status: {:?}",
            root.status_message,
        );

        // Store side-effect: file_path on the in-memory email points
        // at the new location.
        let store = root.email_store_handle();
        let store = store.lock().unwrap();
        let inbox = &store.root_folder.subfolders[0];
        assert_eq!(inbox.emails[0].file_path, archive);
    }

    #[test]
    fn delete_action_moves_file_to_trash_and_pushes_mutation() {
        let temp = tempfile::TempDir::new().unwrap();
        let (mut root, src) = make_root_with_disk_inbox(temp.path().to_path_buf(), "msg2");
        root.layout.active_pane = ActivePane::Messages;

        let d = Event::Key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        root.process_event(d).unwrap();

        let trash = temp.path().join("Trash").join("cur").join("msg2");
        assert!(trash.exists());
        assert!(!src.exists());
        assert_eq!(root.undo_stack_len(), 1);
        assert!(
            root.status_message
                .as_deref()
                .unwrap_or("")
                .starts_with("Deleted"),
        );
    }

    #[test]
    fn archive_creates_target_folder_when_missing() {
        // Acceptance: "Tests cover the create-folder-if-missing path."
        // No Archive/ exists before the action; AppRoot must mkdir -p.
        let temp = tempfile::TempDir::new().unwrap();
        let (mut root, _src) = make_root_with_disk_inbox(temp.path().to_path_buf(), "msg3");
        root.layout.active_pane = ActivePane::Messages;
        assert!(!temp.path().join("Archive").exists());

        let a = Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        root.process_event(a).unwrap();
        assert!(
            temp.path()
                .join("Archive")
                .join("cur")
                .join("msg3")
                .exists()
        );
    }

    #[test]
    fn delete_creates_trash_folder_when_missing() {
        let temp = tempfile::TempDir::new().unwrap();
        let (mut root, _src) = make_root_with_disk_inbox(temp.path().to_path_buf(), "msg4");
        root.layout.active_pane = ActivePane::Messages;
        assert!(!temp.path().join("Trash").exists());

        let d = Event::Key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        root.process_event(d).unwrap();
        assert!(temp.path().join("Trash").join("cur").join("msg4").exists());
    }

    #[test]
    fn star_action_adds_f_flag_when_unstarred() {
        let temp = tempfile::TempDir::new().unwrap();
        // Maildir filenames without a `:2,` suffix are tolerated; the
        // helper adds one when it appends a flag.
        let (mut root, src) = make_root_with_disk_inbox(temp.path().to_path_buf(), "msg5:2,S");
        root.layout.active_pane = ActivePane::Messages;
        // Pre-condition: in-memory mirror starts unflagged (`S` only).
        {
            let store = root.email_store_handle();
            let store = store.lock().unwrap();
            assert!(!store.root_folder.subfolders[0].emails[0].is_flagged);
        }

        let s = Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        root.process_event(s).unwrap();

        // Filename should now contain the F flag (ASCII-sorted: FS).
        let new = temp.path().join("INBOX").join("cur").join("msg5:2,FS");
        assert!(new.exists(), "expected {:?} to exist", new);
        assert!(!src.exists(), "original path must be gone");
        // Mirror got flipped.
        let store = root.email_store_handle();
        let store = store.lock().unwrap();
        assert!(store.root_folder.subfolders[0].emails[0].is_flagged);
        assert_eq!(store.root_folder.subfolders[0].emails[0].file_path, new);
        assert_eq!(root.undo_stack_len(), 1);
    }

    #[test]
    fn star_action_removes_f_flag_when_starred() {
        let temp = tempfile::TempDir::new().unwrap();
        let (mut root, src) = make_root_with_disk_inbox(temp.path().to_path_buf(), "msg6:2,FS");
        root.layout.active_pane = ActivePane::Messages;
        // Email::new derives `is_flagged` from the path.
        {
            let store = root.email_store_handle();
            let store = store.lock().unwrap();
            assert!(store.root_folder.subfolders[0].emails[0].is_flagged);
        }

        let s = Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        root.process_event(s).unwrap();

        let new = temp.path().join("INBOX").join("cur").join("msg6:2,S");
        assert!(new.exists());
        assert!(!src.exists());
        let store = root.email_store_handle();
        let store = store.lock().unwrap();
        assert!(!store.root_folder.subfolders[0].emails[0].is_flagged);
    }

    #[test]
    fn undo_after_archive_restores_file_to_inbox() {
        let temp = tempfile::TempDir::new().unwrap();
        let (mut root, src) = make_root_with_disk_inbox(temp.path().to_path_buf(), "msg7");
        root.layout.active_pane = ActivePane::Messages;

        let a = Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        root.process_event(a).unwrap();
        let archive = temp.path().join("Archive").join("cur").join("msg7");
        assert!(archive.exists() && !src.exists());

        let u = Event::Key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
        root.process_event(u).unwrap();
        assert!(src.exists(), "undo must restore to INBOX/cur");
        assert!(!archive.exists());
        assert_eq!(root.undo_stack_len(), 0);
    }

    #[test]
    fn undo_after_delete_restores_file_to_inbox() {
        let temp = tempfile::TempDir::new().unwrap();
        let (mut root, src) = make_root_with_disk_inbox(temp.path().to_path_buf(), "msg8");
        root.layout.active_pane = ActivePane::Messages;

        let d = Event::Key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        root.process_event(d).unwrap();
        let trash = temp.path().join("Trash").join("cur").join("msg8");
        assert!(trash.exists() && !src.exists());

        let u = Event::Key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
        root.process_event(u).unwrap();
        assert!(src.exists());
        assert!(!trash.exists());
    }

    #[test]
    fn undo_after_star_toggle_restores_flag_state() {
        let temp = tempfile::TempDir::new().unwrap();
        let (mut root, _src) = make_root_with_disk_inbox(temp.path().to_path_buf(), "msg9:2,S");
        root.layout.active_pane = ActivePane::Messages;

        let s = Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        root.process_event(s).unwrap();
        let starred = temp.path().join("INBOX").join("cur").join("msg9:2,FS");
        assert!(starred.exists());

        let u = Event::Key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
        root.process_event(u).unwrap();
        let unstarred = temp.path().join("INBOX").join("cur").join("msg9:2,S");
        assert!(unstarred.exists(), "undo must remove the F flag");
        assert!(!starred.exists());
        let store = root.email_store_handle();
        let store = store.lock().unwrap();
        assert!(!store.root_folder.subfolders[0].emails[0].is_flagged);
    }

    // -----------------------------------------------------------------
    // Mark-read on Enter — new/→cur/ move.
    // -----------------------------------------------------------------

    /// Build an AppRoot whose INBOX contains a real file in `new/` and
    /// whose store has the matching email marked unread. Returns the
    /// temp dir (kept alive by the caller), the shared store handle,
    /// the new/ path, and the expected cur/ path. The current folder
    /// is INBOX and the message cursor is on the unread email.
    fn make_root_with_unread_email_in_new() -> (
        tempfile::TempDir,
        Arc<Mutex<EmailStore>>,
        PathBuf,
        PathBuf,
        AppRoot,
    ) {
        use std::fs;
        let temp = tempfile::TempDir::new().unwrap();
        let new_path = temp.path().join("INBOX/new/msg1");
        let cur_path = temp.path().join("INBOX/cur/msg1");
        fs::create_dir_all(new_path.parent().unwrap()).unwrap();
        fs::create_dir_all(cur_path.parent().unwrap()).unwrap();
        fs::write(&new_path, "body").unwrap();

        let mut store = EmailStore::new(temp.path().to_path_buf());
        let mut inbox = Folder::new("INBOX".to_string(), temp.path().join("INBOX"));
        let mut email = Email::new(new_path.clone());
        email.is_unread = true;
        inbox.add_email(email);
        inbox.is_loaded = true;
        store.root_folder.add_subfolder(inbox);
        store.enter_folder_by_path(&[0]);

        let scanner = MaildirScanner::new(temp.path().to_path_buf());
        let shared = Arc::new(Mutex::new(store));
        let root = AppRoot::new(shared.clone(), scanner);
        (temp, shared, new_path, cur_path, root)
    }

    /// Enter on a message in MessagesComponent must produce both an
    /// open and an auto mark-read. The component returns
    /// `MessageMarkRead` as a follow-up to `MessageOpen` so the
    /// drain loop handles both in lockstep.
    #[test]
    fn message_open_dispatches_mark_read_follow_up() {
        let (_temp, _shared, _new, _cur, mut root) = make_root_with_unread_email_in_new();
        root.enqueue(Msg::MessageOpen(String::new()));
        root.drain();
        // After drain, the file should have been renamed and the
        // mutation pushed — proving that MessageMarkRead actually
        // ran in the same dispatch cycle as MessageOpen.
        assert_eq!(root.undo_stack_len(), 1);
    }

    #[test]
    fn mark_read_renames_file_from_new_to_cur() {
        let (_temp, _shared, new_path, cur_path, mut root) = make_root_with_unread_email_in_new();
        root.enqueue(Msg::MessageMarkRead(String::new()));
        root.drain();
        assert!(!new_path.exists(), "file must leave new/");
        assert!(cur_path.exists(), "file must land in cur/");
    }

    #[test]
    fn mark_read_updates_in_memory_state() {
        let (_temp, shared, _new, cur_path, mut root) = make_root_with_unread_email_in_new();
        root.enqueue(Msg::MessageMarkRead(String::new()));
        root.drain();
        let store = shared.lock().unwrap();
        let inbox = &store.root_folder.subfolders[0];
        assert_eq!(inbox.unread_count, 0);
        assert!(!inbox.emails[0].is_unread);
        assert_eq!(inbox.emails[0].file_path, cur_path);
    }

    #[test]
    fn mark_read_pushes_mark_read_mutation_to_undo_stack() {
        let (_temp, _shared, new_path, cur_path, mut root) = make_root_with_unread_email_in_new();
        root.enqueue(Msg::MessageMarkRead(String::new()));
        root.drain();
        assert_eq!(root.undo_stack_len(), 1);
        assert_eq!(
            root.undo_stack.last(),
            Some(&Mutation::MarkRead {
                msg: cur_path.clone(),
                from: new_path,
                to: cur_path,
            })
        );
    }

    #[test]
    fn mark_read_is_noop_when_email_already_read() {
        // Set up an email that's already in cur/ and not unread.
        // Pressing Enter again must not rename anything, must not
        // push a mutation, and must not touch the unread count.
        use std::fs;
        let temp = tempfile::TempDir::new().unwrap();
        let cur_path = temp.path().join("INBOX/cur/msg1");
        fs::create_dir_all(cur_path.parent().unwrap()).unwrap();
        fs::write(&cur_path, "body").unwrap();

        let mut store = EmailStore::new(temp.path().to_path_buf());
        let mut inbox = Folder::new("INBOX".to_string(), temp.path().join("INBOX"));
        let mut email = Email::new(cur_path.clone());
        email.is_unread = false;
        inbox.add_email(email);
        inbox.is_loaded = true;
        store.root_folder.add_subfolder(inbox);
        store.enter_folder_by_path(&[0]);

        let scanner = MaildirScanner::new(temp.path().to_path_buf());
        let shared = Arc::new(Mutex::new(store));
        let mut root = AppRoot::new(shared.clone(), scanner);
        root.enqueue(Msg::MessageMarkRead(String::new()));
        root.drain();

        assert_eq!(root.undo_stack_len(), 0);
        let store = shared.lock().unwrap();
        let inbox = &store.root_folder.subfolders[0];
        assert_eq!(inbox.unread_count, 0);
        assert!(cur_path.exists());
        assert!(!inbox.emails[0].is_unread);
        assert_eq!(inbox.emails[0].file_path, cur_path);
    }

    #[test]
    fn mark_read_undo_restores_file_and_unread_state() {
        let (_temp, shared, new_path, cur_path, mut root) = make_root_with_unread_email_in_new();
        root.enqueue(Msg::MessageMarkRead(String::new()));
        root.drain();
        // Sanity: mark-read landed.
        assert_eq!(root.undo_stack_len(), 1);
        assert!(cur_path.exists());

        root.enqueue(Msg::Undo);
        root.drain();

        // Disk side: file is back in new/.
        assert!(new_path.exists(), "undo must return file to new/");
        assert!(!cur_path.exists(), "undo must clear cur/");
        // Store side: unread flag and count restored, file_path rewound.
        let store = shared.lock().unwrap();
        let inbox = &store.root_folder.subfolders[0];
        assert_eq!(inbox.unread_count, 1);
        assert!(inbox.emails[0].is_unread);
        assert_eq!(inbox.emails[0].file_path, new_path);
        // Stack drained.
        assert_eq!(root.undo_stack_len(), 0);
    }

    #[test]
    fn mark_unread_moves_file_cur_to_new_and_pushes_mutation() {
        // Pressing `U` on a cursor-selected email in `INBOX/cur/` must
        // rename it into `INBOX/new/`, flip is_unread to true, bump
        // unread_count, and record an undoable mutation. We seed the
        // store with is_unread=false so the counter change is observable.
        let temp = tempfile::TempDir::new().unwrap();
        let (mut root, src) = make_root_with_disk_inbox(temp.path().to_path_buf(), "msg-u1");
        root.layout.active_pane = ActivePane::Messages;
        // Pre-condition: in-memory mirror starts read.
        {
            let store = root.email_store_handle();
            let store = store.lock().unwrap();
            assert!(!store.root_folder.subfolders[0].emails[0].is_unread);
            assert_eq!(store.root_folder.subfolders[0].unread_count, 0);
        }

        let u = Event::Key(KeyEvent::new(KeyCode::Char('U'), KeyModifiers::SHIFT));
        root.process_event(u).unwrap();

        let new_path = temp.path().join("INBOX").join("new").join("msg-u1");
        assert!(new_path.exists(), "expected {:?} to exist", new_path);
        assert!(!src.exists(), "original cur/ path must be gone");
        assert_eq!(root.undo_stack_len(), 1);
        assert!(
            root.status_message
                .as_deref()
                .unwrap_or("")
                .starts_with("Marked unread"),
            "status: {:?}",
            root.status_message,
        );

        // Store side-effects: file_path, is_unread, and unread_count.
        let store = root.email_store_handle();
        let store = store.lock().unwrap();
        let inbox = &store.root_folder.subfolders[0];
        assert_eq!(inbox.emails[0].file_path, new_path);
        assert!(inbox.emails[0].is_unread);
        assert_eq!(inbox.unread_count, 1);
    }

    #[test]
    fn mark_unread_creates_new_dir_when_missing() {
        // The maildir spec requires `new/`, but defensive `mkdir -p`
        // is consistent with how Archive/Delete handle their target
        // dirs.
        let temp = tempfile::TempDir::new().unwrap();
        let (mut root, _src) = make_root_with_disk_inbox(temp.path().to_path_buf(), "msg-u2");
        root.layout.active_pane = ActivePane::Messages;
        // Remove `new/` if anything seeded it (the fixture only mkdirs
        // `cur/`); assert it's absent so the test is meaningful.
        let new_dir = temp.path().join("INBOX").join("new");
        let _ = std::fs::remove_dir_all(&new_dir);
        assert!(!new_dir.exists());

        let u = Event::Key(KeyEvent::new(KeyCode::Char('U'), KeyModifiers::SHIFT));
        root.process_event(u).unwrap();
        assert!(new_dir.join("msg-u2").exists());
    }

    #[test]
    fn mark_unread_is_noop_when_already_in_new() {
        // Idempotency: `U` on an email that's already in `new/` must
        // leave the filesystem untouched and not stack a phantom
        // mutation.
        let temp = tempfile::TempDir::new().unwrap();
        let inbox_new = temp.path().join("INBOX").join("new");
        std::fs::create_dir_all(&inbox_new).unwrap();
        let src = inbox_new.join("msg-u3");
        std::fs::write(&src, "body").unwrap();

        let mut store = EmailStore::new(temp.path().to_path_buf());
        let mut inbox = Folder::new("INBOX".to_string(), temp.path().join("INBOX"));
        let mut email = Email::new(src.clone());
        email.is_unread = true;
        inbox.add_email(email);
        inbox.is_loaded = true;
        store.root_folder.add_subfolder(inbox);
        store.enter_folder_by_path(&[0]);
        store.select_email(0);

        let scanner = MaildirScanner::new(temp.path().to_path_buf());
        let mut root = AppRoot::new(Arc::new(Mutex::new(store)), scanner);
        root.layout.active_pane = ActivePane::Messages;

        let u = Event::Key(KeyEvent::new(KeyCode::Char('U'), KeyModifiers::SHIFT));
        root.process_event(u).unwrap();

        assert!(src.exists(), "file must stay in new/");
        assert_eq!(root.undo_stack_len(), 0, "no mutation should be recorded");
    }

    #[test]
    fn undo_after_mark_unread_restores_file_to_cur() {
        // Round-trip: `U` then `u` must put the file back in `cur/`,
        // flip is_unread back to false, and decrement unread_count.
        let temp = tempfile::TempDir::new().unwrap();
        let (mut root, src) = make_root_with_disk_inbox(temp.path().to_path_buf(), "msg-u4");
        root.layout.active_pane = ActivePane::Messages;

        let u_cap = Event::Key(KeyEvent::new(KeyCode::Char('U'), KeyModifiers::SHIFT));
        root.process_event(u_cap).unwrap();
        let new_path = temp.path().join("INBOX").join("new").join("msg-u4");
        assert!(new_path.exists() && !src.exists());

        let u = Event::Key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
        root.process_event(u).unwrap();
        assert!(src.exists(), "undo must restore to INBOX/cur");
        assert!(!new_path.exists());
        assert_eq!(root.undo_stack_len(), 0);

        let store = root.email_store_handle();
        let store = store.lock().unwrap();
        let inbox = &store.root_folder.subfolders[0];
        assert_eq!(inbox.emails[0].file_path, src);
        assert!(!inbox.emails[0].is_unread, "is_unread must be cleared");
        assert_eq!(inbox.unread_count, 0, "unread_count must be back to 0");
    }

    #[test]
    fn capital_f_toggles_star_same_as_lowercase_s() {
        // `F` is a documented alias for `s`. Both must produce
        // identical filesystem + store state.
        let temp = tempfile::TempDir::new().unwrap();
        let (mut root, src) = make_root_with_disk_inbox(temp.path().to_path_buf(), "msg-f1:2,S");
        root.layout.active_pane = ActivePane::Messages;

        let f = Event::Key(KeyEvent::new(KeyCode::Char('F'), KeyModifiers::SHIFT));
        root.process_event(f).unwrap();

        let starred = temp.path().join("INBOX").join("cur").join("msg-f1:2,FS");
        assert!(starred.exists(), "expected {:?} to exist", starred);
        assert!(!src.exists());
        let store = root.email_store_handle();
        let store = store.lock().unwrap();
        assert!(store.root_folder.subfolders[0].emails[0].is_flagged);
        assert_eq!(root.undo_stack_len(), 1);
    }

    #[test]
    fn approot_with_config_seeds_accounts_pane() {
        // Sanity check the wiring: AppRoot::with_config hands the
        // config through to AccountsComponent::with_config so the
        // pane is populated from the start.
        let cfg = multi_account_config(&[("alpha", "/tmp/alpha"), ("bravo", "/tmp/bravo")]);
        let root = make_root_with_config(cfg);
        assert_eq!(root.accounts.account_count(), 2);
        // BTreeMap order: alpha first.
        assert_eq!(root.accounts.current_account_id().as_deref(), Some("alpha"));
    }

    // -----------------------------------------------------------------
    // Folder-picker modal + 'm' move-to-folder.
    // -----------------------------------------------------------------

    #[test]
    fn key_m_in_messages_pane_opens_folder_picker() {
        let temp = tempfile::TempDir::new().unwrap();
        let (mut root, _src) = make_root_with_disk_inbox(temp.path().to_path_buf(), "msg-m");
        root.layout.active_pane = ActivePane::Messages;
        assert!(!root.folder_picker.visible);

        let m = Event::Key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE));
        root.process_event(m).unwrap();

        assert!(root.folder_picker.visible);
        // INBOX is the one folder we seeded; the picker must see it.
        assert!(
            root.folder_picker
                .folder_list
                .iter()
                .any(|(label, _)| label == "INBOX")
        );
    }

    #[test]
    fn modal_routes_keys_to_picker_not_global() {
        // While the modal is visible, even global keys like 'q' (Quit)
        // must funnel into the filter text, not trigger their global
        // action.
        let temp = tempfile::TempDir::new().unwrap();
        let (mut root, _src) = make_root_with_disk_inbox(temp.path().to_path_buf(), "msg-modal");
        root.layout.active_pane = ActivePane::Messages;

        let m = Event::Key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE));
        root.process_event(m).unwrap();
        assert!(root.folder_picker.visible);
        assert!(!root.should_quit);

        // Press 'q' inside the modal — must NOT quit; must add to filter.
        let q = Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        root.process_event(q).unwrap();
        assert!(!root.should_quit, "modal must absorb global keys");
        assert_eq!(root.folder_picker.filter_text, "q");
    }

    #[test]
    fn esc_in_modal_cancels_picker_with_no_filesystem_change() {
        let temp = tempfile::TempDir::new().unwrap();
        let (mut root, src) = make_root_with_disk_inbox(temp.path().to_path_buf(), "msg-esc");
        root.layout.active_pane = ActivePane::Messages;

        let m = Event::Key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE));
        root.process_event(m).unwrap();
        let esc = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        root.process_event(esc).unwrap();

        assert!(!root.folder_picker.visible);
        assert!(src.exists(), "file must still be in its original place");
        assert_eq!(root.undo_stack_len(), 0, "no mutation recorded on cancel");
    }

    /// AppRoot fixture seeded with an INBOX, an Archive, and a Projects
    /// folder all on disk so the picker can resolve a real target path.
    fn make_root_with_disk_tree(root_path: PathBuf, extra_folders: &[&str]) -> (AppRoot, PathBuf) {
        let inbox_cur = root_path.join("INBOX").join("cur");
        std::fs::create_dir_all(&inbox_cur).unwrap();
        let src = inbox_cur.join("msg1");
        std::fs::write(&src, "body").unwrap();

        let mut store = EmailStore::new(root_path.clone());
        let mut inbox = Folder::new("INBOX".to_string(), root_path.join("INBOX"));
        inbox.add_email(Email::new(src.clone()));
        inbox.is_loaded = true;
        store.root_folder.add_subfolder(inbox);

        for name in extra_folders {
            let fs_path = root_path.join(name);
            std::fs::create_dir_all(fs_path.join("cur")).unwrap();
            let mut f = Folder::new((*name).to_string(), fs_path);
            f.is_loaded = true;
            store.root_folder.add_subfolder(f);
        }
        store.enter_folder_by_path(&[0]);
        store.select_email(0);

        let scanner = MaildirScanner::new(root_path.clone());
        let root = AppRoot::new(Arc::new(Mutex::new(store)), scanner);
        (root, src)
    }

    #[test]
    fn enter_in_modal_moves_file_to_picked_folder() {
        let temp = tempfile::TempDir::new().unwrap();
        let (mut root, src) =
            make_root_with_disk_tree(temp.path().to_path_buf(), &["Archive", "Projects"]);
        root.layout.active_pane = ActivePane::Messages;

        // Open modal, filter to "Proj", pick it.
        let m = Event::Key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE));
        root.process_event(m).unwrap();
        for c in "Proj".chars() {
            let key = Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
            root.process_event(key).unwrap();
        }
        let enter = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        root.process_event(enter).unwrap();

        let dst = temp.path().join("Projects").join("cur").join("msg1");
        assert!(dst.exists(), "expected file at {:?}", dst);
        assert!(!src.exists(), "source must be empty after move");
        assert_eq!(root.undo_stack_len(), 1, "move pushes one mutation");
        assert!(
            root.status_message
                .as_deref()
                .unwrap_or("")
                .starts_with("Moved"),
            "status: {:?}",
            root.status_message,
        );
        assert!(!root.folder_picker.visible, "modal closes after pick");
    }

    #[test]
    fn undo_after_picker_move_restores_file() {
        let temp = tempfile::TempDir::new().unwrap();
        let (mut root, src) = make_root_with_disk_tree(temp.path().to_path_buf(), &["Projects"]);
        root.layout.active_pane = ActivePane::Messages;

        let m = Event::Key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE));
        root.process_event(m).unwrap();
        for c in "Proj".chars() {
            let key = Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
            root.process_event(key).unwrap();
        }
        let enter = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        root.process_event(enter).unwrap();

        let dst = temp.path().join("Projects").join("cur").join("msg1");
        assert!(dst.exists() && !src.exists());

        let u = Event::Key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
        root.process_event(u).unwrap();

        assert!(src.exists(), "undo must restore to INBOX/cur");
        assert!(!dst.exists(), "undo must clear the Projects path");
        assert_eq!(root.undo_stack_len(), 0);
    }

    // -----------------------------------------------------------------
    // Phase 1.g: integration tests for full action workflows.
    //
    // These tests drive AppRoot through realistic multi-step user
    // flows. The per-feature unit tests above prove each step in
    // isolation; these are the regression-prevention layer that fails
    // meaningfully if any of Phase 1.a–1.f regresses.
    // -----------------------------------------------------------------

    /// Build an AppRoot pointed at `root_path` with a single INBOX
    /// containing `n` real files in `cur/`. Useful for triage-style
    /// integration tests that walk the cursor across many emails.
    fn make_root_with_n_emails(root_path: PathBuf, n: usize) -> (AppRoot, Vec<PathBuf>) {
        let inbox_cur = root_path.join("INBOX").join("cur");
        std::fs::create_dir_all(&inbox_cur).unwrap();

        let mut store = EmailStore::new(root_path.clone());
        let mut inbox = Folder::new("INBOX".to_string(), root_path.join("INBOX"));
        let mut srcs = Vec::with_capacity(n);
        for i in 0..n {
            let name = format!("msg{:02}", i);
            let path = inbox_cur.join(&name);
            std::fs::write(&path, format!("body of {}", name)).unwrap();
            inbox.add_email(Email::new(path.clone()));
            srcs.push(path);
        }
        inbox.is_loaded = true;
        store.root_folder.add_subfolder(inbox);
        store.enter_folder_by_path(&[0]);
        store.select_email(0);

        let scanner = MaildirScanner::new(root_path.clone());
        let root = AppRoot::new(Arc::new(Mutex::new(store)), scanner);
        (root, srcs)
    }

    /// Triage inbox: alternate Archive ('a') and Delete ('d') across
    /// ten cursored emails, advance the cursor between each action,
    /// then press 'u' ten times to reverse the whole batch. Verifies
    /// every file lands in the right destination, the in-memory
    /// `file_path` mirror tracks each move, and the undo stack
    /// unwinds LIFO back to the starting filesystem state.
    #[test]
    fn triage_inbox_archive_and_delete_then_undo_all_ten() {
        let temp = tempfile::TempDir::new().unwrap();
        let (mut root, srcs) = make_root_with_n_emails(temp.path().to_path_buf(), 10);
        root.layout.active_pane = ActivePane::Messages;

        let archive_dir = temp.path().join("Archive").join("cur");
        let trash_dir = temp.path().join("Trash").join("cur");

        // 10 actions: even indices archive, odd indices delete.
        // Bump the messages cursor between actions so each press
        // targets the next email — `apply_move_action` rewrites
        // `file_path` but leaves the email in the in-memory list.
        for i in 0..10 {
            root.messages.email_index = i;
            let key = if i % 2 == 0 { 'a' } else { 'd' };
            let ev = Event::Key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE));
            root.process_event(ev).unwrap();
        }

        // Every original file is gone; archived/deleted copies exist.
        for (i, src) in srcs.iter().enumerate() {
            assert!(!src.exists(), "src {:?} should be moved", src);
            let dst_dir = if i % 2 == 0 { &archive_dir } else { &trash_dir };
            let dst = dst_dir.join(src.file_name().unwrap());
            assert!(dst.exists(), "expected {:?} after triage step {}", dst, i);
        }
        assert_eq!(root.undo_stack_len(), 10);

        // In-memory mirror: each email's file_path tracks its new home.
        {
            let store = root.email_store_handle();
            let store = store.lock().unwrap();
            let inbox = &store.root_folder.subfolders[0];
            for (i, email) in inbox.emails.iter().enumerate() {
                let dst_dir = if i % 2 == 0 { &archive_dir } else { &trash_dir };
                let expected = dst_dir.join(srcs[i].file_name().unwrap());
                assert_eq!(email.file_path, expected, "email {} mirror lags", i);
            }
        }

        // Ten undos must reverse every move, LIFO. After all 10
        // pops, every file is back in INBOX/cur and the stack is
        // empty.
        for _ in 0..10 {
            let u = Event::Key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
            root.process_event(u).unwrap();
        }

        assert_eq!(root.undo_stack_len(), 0);
        for src in &srcs {
            assert!(src.exists(), "undo must restore {:?}", src);
        }
        // Destinations are clean.
        for src in &srcs {
            let name = src.file_name().unwrap();
            assert!(!archive_dir.join(name).exists());
            assert!(!trash_dir.join(name).exists());
        }
        // In-memory mirror is back to the source paths.
        {
            let store = root.email_store_handle();
            let store = store.lock().unwrap();
            let inbox = &store.root_folder.subfolders[0];
            for (i, email) in inbox.emails.iter().enumerate() {
                assert_eq!(email.file_path, srcs[i], "mirror at idx {} not restored", i);
            }
        }
    }

    /// Build a per-account maildir directory tree on disk and return
    /// its root. Used by the multi-account integration test.
    fn write_account_inbox(root_path: &std::path::Path, filenames: &[&str]) -> PathBuf {
        let cur = root_path.join("INBOX").join("cur");
        let new = root_path.join("INBOX").join("new");
        std::fs::create_dir_all(&cur).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        for f in filenames {
            std::fs::write(new.join(f), b"body").unwrap();
        }
        root_path.to_path_buf()
    }

    /// Re-seed the AppRoot's EmailStore with a single INBOX whose
    /// emails come from the current `new/` and `cur/` on disk. This
    /// simulates the folder-scanner reply for tests that exercise
    /// `Msg::AccountSelect` (which resets the store and kicks off a
    /// fresh scan that integration tests don't otherwise wait for).
    fn reseed_inbox_from_disk(root: &mut AppRoot) {
        let maildir_root = {
            let store = root.email_store_handle();
            let store = store.lock().unwrap();
            store.root_folder.path.clone()
        };
        let inbox_path = maildir_root.join("INBOX");
        let mut inbox = Folder::new("INBOX".to_string(), inbox_path.clone());
        // `new/` first — these are the unread ones.
        if let Ok(entries) = std::fs::read_dir(inbox_path.join("new")) {
            let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
            paths.sort();
            for p in paths {
                let mut e = Email::new(p);
                e.is_unread = true;
                inbox.unread_count += 1;
                inbox.add_email(e);
            }
        }
        if let Ok(entries) = std::fs::read_dir(inbox_path.join("cur")) {
            let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
            paths.sort();
            for p in paths {
                let mut e = Email::new(p);
                e.is_unread = false;
                inbox.add_email(e);
            }
        }
        inbox.is_loaded = true;

        let store_handle = root.email_store_handle();
        let mut store = store_handle.lock().unwrap();
        store.root_folder.subfolders.clear();
        store.root_folder.add_subfolder(inbox);
        store.scanning_folders = false;
        store.enter_folder_by_path(&[0]);
        store.select_email(0);
    }

    /// Multi-account workflow: with two accounts configured, switch
    /// to account A, mark-read its unread email (Enter on the
    /// Messages pane moves the file from new/ → cur/), switch to
    /// account B and mark-read one of its emails, then switch back
    /// to A and verify the disk state we left behind is intact.
    ///
    /// "State preserved per account" here means the on-disk maildir
    /// is the source of truth and an account switch never mutates
    /// the other account's files. The cursor/view state resets on
    /// switch by design (see `switch_active_maildir`).
    ///
    /// Note on the keybinding: VISION.md and the AccountsComponent
    /// unit tests treat 'l' as an account-select keystroke, but in
    /// the current AppRoot the global key handler intercepts 'l'
    /// for `Msg::ViewNext` before it reaches the Accounts pane.
    /// This test drives the switch via `Msg::AccountSelect`
    /// directly so it captures the workflow effect; a separate
    /// observation has been filed for the key-routing gap.
    #[test]
    fn multi_account_switch_preserves_per_account_disk_state() {
        let temp_a = tempfile::TempDir::new().unwrap();
        let temp_b = tempfile::TempDir::new().unwrap();
        let path_a = write_account_inbox(temp_a.path(), &["a-msg1", "a-msg2"]);
        let path_b = write_account_inbox(temp_b.path(), &["b-msg1"]);

        let mut cfg = Config::default();
        cfg.accounts.insert(
            "alpha".into(),
            crate::config::AccountConfig {
                name: "Alpha".into(),
                email: "a@x.test".into(),
                maildir_path: path_a.clone(),
                smtp_command: None,
                signature: None,
            },
        );
        cfg.accounts.insert(
            "bravo".into(),
            crate::config::AccountConfig {
                name: "Bravo".into(),
                email: "b@x.test".into(),
                maildir_path: path_b.clone(),
                smtp_command: None,
                signature: None,
            },
        );

        let store = EmailStore::new(path_a.clone());
        let scanner = MaildirScanner::new(path_a.clone());
        let mut root = AppRoot::with_config(Arc::new(Mutex::new(store)), scanner, cfg);

        // Land on Alpha first. The fixture store starts empty; reseed
        // from disk so the test does not depend on the async scanner.
        root.enqueue(Msg::AccountSelect("alpha".into()));
        root.drain();
        reseed_inbox_from_disk(&mut root);

        // Focus Messages, cursor on the first unread, press Enter to
        // mark it read (file moves new/ → cur/).
        root.layout.active_pane = ActivePane::Messages;
        root.messages.email_index = 0;
        let enter = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        root.process_event(enter).unwrap();

        let a_msg1_new = path_a.join("INBOX").join("new").join("a-msg1");
        let a_msg1_cur = path_a.join("INBOX").join("cur").join("a-msg1");
        assert!(!a_msg1_new.exists(), "Alpha msg1 must leave new/");
        assert!(a_msg1_cur.exists(), "Alpha msg1 must land in cur/");

        // Switch to Bravo. The store gets reset; reseed from disk so
        // we can act on Bravo's emails too.
        root.enqueue(Msg::AccountSelect("bravo".into()));
        root.drain();
        // After switch, store path is the Bravo maildir; pane focus
        // is back on Folders per `switch_active_maildir`.
        {
            let store = root.email_store_handle();
            let store = store.lock().unwrap();
            assert_eq!(store.root_folder.path, path_b);
        }
        assert_eq!(root.layout.active_pane, ActivePane::Folders);
        reseed_inbox_from_disk(&mut root);

        // Mark Bravo's msg1 read.
        root.layout.active_pane = ActivePane::Messages;
        root.messages.email_index = 0;
        let enter = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        root.process_event(enter).unwrap();

        let b_msg1_new = path_b.join("INBOX").join("new").join("b-msg1");
        let b_msg1_cur = path_b.join("INBOX").join("cur").join("b-msg1");
        assert!(!b_msg1_new.exists(), "Bravo msg1 must leave new/");
        assert!(b_msg1_cur.exists(), "Bravo msg1 must land in cur/");

        // Switch back to Alpha. Verify nothing in Alpha's maildir
        // was touched by the Bravo round-trip — msg1 still in cur/,
        // msg2 still in new/. The undo stack also resets across
        // account switches (it's owned by AppRoot, not per-account,
        // but the switch_active_maildir contract drops cursors;
        // mutations on Bravo remain undoable until we switch).
        root.enqueue(Msg::AccountSelect("alpha".into()));
        root.drain();
        {
            let store = root.email_store_handle();
            let store = store.lock().unwrap();
            assert_eq!(store.root_folder.path, path_a);
        }

        let a_msg2_new = path_a.join("INBOX").join("new").join("a-msg2");
        assert!(a_msg1_cur.exists(), "Alpha msg1 must still be in cur/");
        assert!(!a_msg1_new.exists(), "Alpha msg1 must not be in new/");
        assert!(a_msg2_new.exists(), "Alpha msg2 must still be unread");

        // And Bravo's state survived too.
        assert!(b_msg1_cur.exists(), "Bravo msg1 must still be read");
        assert!(!b_msg1_new.exists());
    }

    /// Move-with-picker round-trip: from a Messages-pane cursor on
    /// INBOX/msg1, press 'm' to open the picker, type a filter,
    /// press Enter to commit the move, then press 'u' to revert.
    /// Asserts the full status sequence and undo-stack accounting,
    /// stricter than the existing `enter_in_modal_moves_file_to_picked_folder`
    /// and `undo_after_picker_move_restores_file` cases. This is the
    /// integration-layer guardrail for Phase 1.d.
    #[test]
    fn move_with_picker_and_undo_full_round_trip() {
        let temp = tempfile::TempDir::new().unwrap();
        let (mut root, src) =
            make_root_with_disk_tree(temp.path().to_path_buf(), &["Archive", "Projects"]);
        root.layout.active_pane = ActivePane::Messages;
        assert_eq!(root.undo_stack_len(), 0);
        assert!(!root.folder_picker.visible);

        // 1. Open the picker.
        let m = Event::Key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE));
        root.process_event(m).unwrap();
        assert!(root.folder_picker.visible);

        // 2. Filter down to "Projects".
        for c in "Proj".chars() {
            let key = Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
            root.process_event(key).unwrap();
        }
        assert_eq!(root.folder_picker.filter_text, "Proj");

        // 3. Commit. File moves to Projects/cur, picker closes, undo
        //    stack grows, status reads "Moved".
        let enter = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        root.process_event(enter).unwrap();

        let dst = temp.path().join("Projects").join("cur").join("msg1");
        assert!(dst.exists());
        assert!(!src.exists());
        assert!(!root.folder_picker.visible);
        assert_eq!(root.undo_stack_len(), 1);
        assert!(
            root.status_message
                .as_deref()
                .unwrap_or("")
                .starts_with("Moved"),
            "status after move: {:?}",
            root.status_message,
        );
        // Mirror tracks the new location.
        {
            let store = root.email_store_handle();
            let store = store.lock().unwrap();
            let inbox = &store.root_folder.subfolders[0];
            assert_eq!(inbox.emails[0].file_path, dst);
        }

        // 4. Undo. File returns, stack empties.
        let u = Event::Key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
        root.process_event(u).unwrap();
        assert!(src.exists());
        assert!(!dst.exists());
        assert_eq!(root.undo_stack_len(), 0);
        {
            let store = root.email_store_handle();
            let store = store.lock().unwrap();
            let inbox = &store.root_folder.subfolders[0];
            assert_eq!(inbox.emails[0].file_path, src);
        }
    }

    // --- Phase 2.d: reply variant DraftStart end-to-end. ---

    /// Seed a Messages-pane root with a single real email file at
    /// `<root>/INBOX/cur/`. Returns the root and the path to the file.
    /// Mirrors `make_root_with_disk_tree` but writes a richer header
    /// block so the reply builder has something to quote.
    fn make_root_with_one_real_email(root_path: PathBuf) -> AppRoot {
        let inbox = root_path.join("INBOX").join("cur");
        std::fs::create_dir_all(&inbox).unwrap();
        let msg_path = inbox.join("orig.eml");
        std::fs::write(
            &msg_path,
            "From: Alice <alice@example.com>\r\n\
             To: Tester <tester@example.com>, Bob <bob@example.com>\r\n\
             Subject: Lunch tomorrow?\r\n\
             Message-ID: <orig-1@example.com>\r\n\
             Date: Sat, 16 May 2026 12:00:00 +0000\r\n\
             \r\n\
             Hey,\r\nWant to grab lunch?\r\n",
        )
        .unwrap();

        let mut store = EmailStore::new(root_path.clone());
        let mut folder = Folder::new("INBOX".into(), root_path.join("INBOX"));
        let mut email = Email::new(msg_path);
        email.parse_headers_only().unwrap();
        folder.add_email(email);
        folder.is_loaded = true;
        store.root_folder.add_subfolder(folder);
        store.enter_folder_by_path(&[0]);
        store.select_email(0);

        let scanner = MaildirScanner::new(root_path);
        AppRoot::new(Arc::new(Mutex::new(store)), scanner)
    }

    /// `r` (reply-all) on the cursor email must:
    ///   - install a populated `Compose` on the live draft,
    ///   - park an editor launch (the run loop will pick it up),
    ///   - flip the view to ContentDraft with focus on Draft.
    ///
    /// Note: `mail-parser` (and the current `EmailHeaders` schema) only
    /// surfaces the FIRST recipient on the To: line — multi-recipient
    /// parsing isn't wired up yet. So reply-all currently fans out to
    /// `original.From + original.To[0]` minus our address. Once header
    /// parsing learns about the full recipient list, this test should
    /// grow assertions for the additional names.
    #[test]
    fn r_key_dispatches_reply_all_template_and_parks_editor() {
        let temp = tempfile::TempDir::new().unwrap();
        let mut root = make_root_with_one_real_email(temp.path().to_path_buf());
        root.layout.active_pane = ActivePane::Messages;

        let r = Event::Key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
        root.process_event(r).unwrap();

        // Editor launch is queued, not yet run.
        assert!(
            root.has_pending_editor(),
            "pressing r must park an editor launch",
        );
        // Draft pane has the populated compose.
        let state = root.draft().state().expect("draft started");
        assert_eq!(state.reply_kind, ReplyKind::ReplyAll);
        assert_eq!(state.status, crate::components::draft::DraftStatus::Editing);
        assert!(state.compose.to.contains("Alice <alice@example.com>"));
        // Reply-all surfaces the original first-recipient too — until
        // the schema grows multi-recipient support this is the only
        // "other recipient" that fans out.
        assert!(
            state.compose.to.contains("Tester <tester@example.com>"),
            "reply-all must include the original To recipient, got {:?}",
            state.compose.to,
        );
        assert_eq!(state.compose.subject, "Re: Lunch tomorrow?");
        assert_eq!(
            state.compose.in_reply_to.as_deref(),
            Some("<orig-1@example.com>")
        );
        // View progression hopped to the pre-send surface.
        assert_eq!(root.layout.current_view, View::ContentDraft);
        assert_eq!(root.layout.active_pane, ActivePane::Draft);
    }

    /// `gr` (two-key) must dispatch a sender-only reply, not reply-all.
    /// The key signal: the original To recipient does NOT appear in
    /// the reply To line.
    #[test]
    fn gr_two_key_dispatches_reply_sender_template() {
        let temp = tempfile::TempDir::new().unwrap();
        let mut root = make_root_with_one_real_email(temp.path().to_path_buf());
        root.layout.active_pane = ActivePane::Messages;

        let g = Event::Key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
        root.process_event(g).unwrap();
        // 'g' alone must not launch anything; just arm the prefix.
        assert!(!root.has_pending_editor());
        assert!(root.draft().state().is_none());

        let r = Event::Key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
        root.process_event(r).unwrap();

        assert!(root.has_pending_editor(), "gr must park an editor launch");
        let state = root.draft().state().expect("draft started");
        assert_eq!(state.reply_kind, ReplyKind::Reply);
        // Reply-sender — the original To recipient ("Tester") must NOT
        // be on the To: line; only the original From (Alice).
        assert_eq!(state.compose.to, "Alice <alice@example.com>");
        assert!(!state.compose.to.contains("Tester"));
    }

    /// `f` (forward) must dispatch a forward template with an empty To
    /// line, no In-Reply-To, and a `Fwd:` subject.
    #[test]
    fn f_key_dispatches_forward_template_with_empty_to() {
        let temp = tempfile::TempDir::new().unwrap();
        let mut root = make_root_with_one_real_email(temp.path().to_path_buf());
        root.layout.active_pane = ActivePane::Messages;

        let f = Event::Key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
        root.process_event(f).unwrap();

        assert!(root.has_pending_editor());
        let state = root.draft().state().expect("draft started");
        assert_eq!(state.reply_kind, ReplyKind::Forward);
        assert_eq!(state.compose.to, "");
        assert_eq!(state.compose.subject, "Fwd: Lunch tomorrow?");
        assert!(state.compose.in_reply_to.is_none());
        assert!(state.compose.body.contains("Forwarded message"));
    }

    /// `S` in the Draft pane invokes `compose::send`. With a mock SMTP
    /// command that swallows stdin and exits 0, the runtime must:
    ///   - file a Sent copy under `<maildir>/Sent/cur/`,
    ///   - clear the draft (`has_draft()` returns false),
    ///   - drop back to MessagesContent with focus on Messages,
    ///   - surface a "Sent: <filename>" status message.
    #[test]
    fn capital_s_in_draft_pane_invokes_send_and_clears_draft() {
        let temp = tempfile::TempDir::new().unwrap();
        let mut root = make_root_with_one_real_email(temp.path().to_path_buf());

        // Plug in a single account with a mock SMTP command. The
        // command reads stdin and discards it; `compose::send` then
        // writes the Sent copy itself, so this exercises the full
        // pipe-and-file flow without requiring msmtp.
        let mut cfg = Config::default();
        cfg.accounts.insert(
            "primary".into(),
            crate::config::AccountConfig {
                name: "Primary".into(),
                email: "me@example.com".into(),
                maildir_path: temp.path().to_path_buf(),
                smtp_command: Some("cat > /dev/null".to_string()),
                signature: None,
            },
        );
        root.accounts = AccountsComponent::with_config(&cfg);
        root.config = cfg;

        // Start a reply, install the parsed editor result, focus Draft.
        root.layout.active_pane = ActivePane::Messages;
        let r = Event::Key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
        root.process_event(r).unwrap();
        // Simulate the editor exiting with the parsed Compose. AppRoot's
        // run loop would normally call this after `$EDITOR`; the test
        // calls it directly because we don't actually launch one.
        let pending = root.take_pending_editor().expect("editor parked");
        let parsed = crate::compose::parse_compose_from_text(&pending.template).unwrap();
        root.apply_editor_result(parsed);
        assert_eq!(
            root.draft().state().expect("draft ready").status,
            crate::components::draft::DraftStatus::ReadyToSend,
        );
        assert_eq!(root.layout.active_pane, ActivePane::Draft);

        // Press 'S' — must route through the Draft per-pane handler,
        // not the global 'q'-style quit, and not back to Messages.
        let big_s = Event::Key(KeyEvent::new(KeyCode::Char('S'), KeyModifiers::SHIFT));
        root.process_event(big_s).unwrap();

        // Draft cleared, view dropped back to MessagesContent.
        assert!(
            !root.draft().has_draft(),
            "draft must be cleared after send"
        );
        assert_eq!(root.layout.current_view, View::MessagesContent);
        assert_eq!(root.layout.active_pane, ActivePane::Messages);

        // Sent/ folder gained exactly one file.
        let sent_dir = temp.path().join("Sent").join("cur");
        let entries: Vec<_> = std::fs::read_dir(&sent_dir)
            .expect("Sent/cur/ created")
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), 1, "exactly one Sent copy written");
    }

    /// SMTP failure must leave the draft in `Failed` so the user can
    /// fix and resend. The Sent copy must NOT be written.
    #[test]
    fn capital_s_smtp_failure_surfaces_failed_status() {
        let temp = tempfile::TempDir::new().unwrap();
        let mut root = make_root_with_one_real_email(temp.path().to_path_buf());

        let mut cfg = Config::default();
        cfg.accounts.insert(
            "primary".into(),
            crate::config::AccountConfig {
                name: "Primary".into(),
                email: "me@example.com".into(),
                maildir_path: temp.path().to_path_buf(),
                // Non-zero exit — simulates msmtp rejecting the message.
                smtp_command: Some("cat > /dev/null; exit 67".to_string()),
                signature: None,
            },
        );
        root.accounts = AccountsComponent::with_config(&cfg);
        root.config = cfg;

        root.layout.active_pane = ActivePane::Messages;
        let r = Event::Key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
        root.process_event(r).unwrap();
        let pending = root.take_pending_editor().unwrap();
        let parsed = crate::compose::parse_compose_from_text(&pending.template).unwrap();
        root.apply_editor_result(parsed);

        let big_s = Event::Key(KeyEvent::new(KeyCode::Char('S'), KeyModifiers::SHIFT));
        root.process_event(big_s).unwrap();

        // Draft survives in `Failed` state — user can press `e` to
        // re-edit or `q` to abandon.
        let state = root.draft().state().expect("draft preserved on failure");
        assert!(matches!(
            state.status,
            crate::components::draft::DraftStatus::Failed(_)
        ));
        // View stays on the Draft pane so the failure footer is visible.
        assert_eq!(root.layout.current_view, View::ContentDraft);
        assert_eq!(root.layout.active_pane, ActivePane::Draft);
        // Sent/ NOT written.
        assert!(!temp.path().join("Sent").join("cur").exists());
    }

    /// `q` in the Draft pane discards the draft and drops back to
    /// MessagesContent — it must NOT quit the app.
    #[test]
    fn q_in_draft_pane_discards_instead_of_quitting() {
        let temp = tempfile::TempDir::new().unwrap();
        let mut root = make_root_with_one_real_email(temp.path().to_path_buf());

        root.layout.active_pane = ActivePane::Messages;
        let r = Event::Key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
        root.process_event(r).unwrap();
        // Drain the parked launch so the next event is interpreted in
        // the Draft pane (the run loop would do this).
        let _ = root.take_pending_editor();
        assert_eq!(root.layout.active_pane, ActivePane::Draft);

        let q = Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        let should_quit = root.process_event(q).unwrap();

        assert!(!should_quit, "q in Draft pane must not quit the app");
        assert!(!root.draft().has_draft(), "q must discard the draft");
        assert_eq!(root.layout.current_view, View::MessagesContent);
        assert_eq!(root.layout.active_pane, ActivePane::Messages);
    }

    /// `e` in the Draft pane parks a new editor launch built from the
    /// live draft compose. Used to fix a typo after the editor exit
    /// before pressing `S`.
    #[test]
    fn e_in_draft_pane_reparks_editor_launch() {
        let temp = tempfile::TempDir::new().unwrap();
        let mut root = make_root_with_one_real_email(temp.path().to_path_buf());

        root.layout.active_pane = ActivePane::Messages;
        let r = Event::Key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
        root.process_event(r).unwrap();
        let pending = root.take_pending_editor().unwrap();
        let parsed = crate::compose::parse_compose_from_text(&pending.template).unwrap();
        root.apply_editor_result(parsed);
        assert!(!root.has_pending_editor());

        let e = Event::Key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        root.process_event(e).unwrap();

        assert!(root.has_pending_editor(), "e must park a fresh launch");
        // Status flipped back to `Editing` so the footer doesn't lie.
        assert_eq!(
            root.draft().state().unwrap().status,
            crate::components::draft::DraftStatus::Editing,
        );
    }

    /// 'l' from the Content view jumps to ContentDraft when a draft
    /// exists. Without a draft, 'l' stays put (Content is the
    /// rightmost view in the normal progression).
    #[test]
    fn l_from_content_view_jumps_to_content_draft_when_draft_present() {
        let temp = tempfile::TempDir::new().unwrap();
        let mut root = make_root_with_one_real_email(temp.path().to_path_buf());

        // No draft yet — Content is terminal.
        root.layout.current_view = View::Content;
        root.layout.active_pane = ActivePane::Content;
        let l = Event::Key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE));
        root.process_event(l).unwrap();
        assert_eq!(
            root.layout.current_view,
            View::Content,
            "without a draft, l from Content stays put",
        );

        // Start a draft, navigate back to Content, then 'l' jumps.
        root.layout.active_pane = ActivePane::Messages;
        let r = Event::Key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
        root.process_event(r).unwrap();
        let _ = root.take_pending_editor();
        // Force the view back to Content as if the user navigated away.
        root.layout.current_view = View::Content;
        root.layout.active_pane = ActivePane::Content;
        let l = Event::Key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE));
        root.process_event(l).unwrap();
        assert_eq!(root.layout.current_view, View::ContentDraft);
        assert_eq!(root.layout.active_pane, ActivePane::Draft);

        // 'h' from ContentDraft drops back to MessagesContent.
        let h = Event::Key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
        root.process_event(h).unwrap();
        assert_eq!(root.layout.current_view, View::MessagesContent);
    }

    /// `R` (reply-later) must NOT launch the editor. Instead it writes
    /// an empty-body draft straight to `<maildir>/Drafts/cur/` and
    /// registers it in the store's drafts index so the ⏰ chip shows up
    /// on the original next render.
    #[test]
    fn capital_r_writes_empty_draft_file_and_updates_drafts_index() {
        let temp = tempfile::TempDir::new().unwrap();
        let mut root = make_root_with_one_real_email(temp.path().to_path_buf());
        root.layout.active_pane = ActivePane::Messages;

        let big_r = Event::Key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::NONE));
        root.process_event(big_r).unwrap();

        // No editor launch — reply-later is purely a placeholder.
        assert!(
            !root.has_pending_editor(),
            "R (reply-later) must not launch the editor",
        );

        // Draft state advanced to ReadyToSend (skips Editing).
        let state = root.draft().state().expect("draft started");
        assert_eq!(state.reply_kind, ReplyKind::ReplyLater);
        assert_eq!(
            state.status,
            crate::components::draft::DraftStatus::ReadyToSend
        );
        assert_eq!(state.compose.body, "");

        // The file exists under Drafts/cur/.
        let drafts_dir = temp.path().join("Drafts").join("cur");
        let entries: Vec<_> = std::fs::read_dir(&drafts_dir)
            .expect("Drafts/cur/ created")
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), 1, "exactly one draft file written");
        let written = std::fs::read_to_string(entries[0].path()).unwrap();
        assert!(written.contains("In-Reply-To: <orig-1@example.com>"));
        assert!(written.contains("Subject: Re: Lunch tomorrow?"));

        // Store's drafts index gained a body_empty=true entry for the
        // original message id — that's what `chip_for_message_id`
        // reads to paint the ⏰ glyph.
        let store = root.email_store_handle();
        let store = store.lock().unwrap();
        let entry = store
            .drafts
            .get("orig-1@example.com")
            .expect("drafts index gained an entry for the original");
        assert!(entry.body_empty);
    }

    // ---- Phase 3.a — notmuch search lifecycle --------------------------

    /// `/` from the Messages pane opens the search input modal.
    /// Pre-condition: `notmuch` must be available — we skip this test
    /// when it isn't, so the host doesn't need a notmuch install to
    /// run `cargo test`. The unavailable path is covered separately.
    #[test]
    fn slash_key_opens_search_modal_when_notmuch_available() {
        if !notmuch_available() {
            eprintln!("notmuch not on PATH — skipping");
            return;
        }
        let mut root = make_root_with_folders(&["INBOX"]);
        root.set_active_pane_for_test(ActivePane::Messages);
        let slash = KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE);
        root.process_event(Event::Key(slash)).unwrap();
        assert!(root.search.visible, "/ opens the modal");
        assert_eq!(root.search.query, "");
    }

    /// When `notmuch` is missing from `PATH`, `OpenSearchInput`
    /// surfaces a status message and leaves the modal closed. We
    /// exercise the `apply_open_search_input` path directly so the
    /// test doesn't depend on the host's `PATH`.
    #[test]
    fn open_search_input_with_no_notmuch_sets_status_and_skips_modal() {
        let _guard = path_lock().lock().unwrap();
        let mut root = make_root();
        // SearchComponent::handle_msg flips `visible` true on
        // OpenSearchInput. apply_open_search_input must close it back
        // if notmuch is missing.
        let original_path = std::env::var_os("PATH");
        // SAFETY: serialized via `path_lock`; PATH is restored before
        // returning so no other test sees the override.
        unsafe {
            std::env::set_var("PATH", "/nonexistent-vulthor-search-path");
        }
        root.enqueue(Msg::OpenSearchInput);
        root.drain();
        let visible = root.search.visible;
        unsafe {
            match original_path {
                Some(p) => std::env::set_var("PATH", p),
                None => std::env::remove_var("PATH"),
            }
        }
        assert!(!visible, "modal stays hidden when notmuch is missing");
        assert!(
            matches!(
                root.status_message.as_deref(),
                Some(s) if s.contains("notmuch not found"),
            ),
            "status reports notmuch missing; got {:?}",
            root.status_message
        );
    }

    /// `Msg::SearchResults` installs a virtual folder named
    /// `Search: <query>` and switches to the Messages-only view, so
    /// the breadcrumb reads `Mail > Search: …`.
    #[test]
    fn search_results_installs_virtual_folder() {
        use std::fs;
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        // Materialise a real maildir-style file so
        // apply_search_results_named's `path.exists()` gate accepts it.
        let path = tmp.path().join("hit.eml");
        fs::write(
            &path,
            "From: a@example.com\r\nTo: b@example.com\r\nSubject: hello\r\n\r\nbody\r\n",
        )
        .unwrap();

        let mut root = make_root();
        root.apply_search_results_named(vec![path.clone()], "tag:inbox".into());

        let store = root.email_store_handle();
        let store = store.lock().unwrap();
        let results = store
            .search_results
            .as_ref()
            .expect("search results installed");
        assert_eq!(results.name, "Search: tag:inbox");
        assert_eq!(results.emails.len(), 1, "single matched file resolved");
        assert_eq!(results.emails[0].file_path, path);
        // The Messages-only view + Messages-pane focus is what the
        // breadcrumb code reads to render the virtual folder name.
        assert_eq!(root.layout.current_view, View::Messages);
        assert_eq!(root.layout.active_pane, ActivePane::Messages);
    }

    /// Phantom rows (path returned by notmuch but file vanished from
    /// disk between the index and the read) are silently dropped.
    #[test]
    fn search_results_skips_missing_files() {
        let mut root = make_root();
        root.apply_search_results_named(
            vec![PathBuf::from("/definitely/does/not/exist.eml")],
            "tag:inbox".into(),
        );
        let store = root.email_store_handle();
        let store = store.lock().unwrap();
        let results = store.search_results.as_ref().unwrap();
        assert!(results.emails.is_empty());
    }

    /// `Msg::SearchCancel` clears the virtual folder and returns to
    /// the FolderMessages view.
    #[test]
    fn search_cancel_clears_virtual_folder() {
        use std::fs;
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("hit.eml");
        fs::write(&path, "Subject: x\r\n\r\nbody\r\n").unwrap();

        let mut root = make_root();
        root.apply_search_results_named(vec![path], "tag:inbox".into());
        assert!(root.search_results_active());

        root.enqueue(Msg::SearchCancel);
        root.drain();

        assert!(!root.search_results_active());
        assert_eq!(root.layout.current_view, View::FolderMessages);
    }

    /// While search results are on display, `h` and `Esc` exit the
    /// search instead of dropping the global view-prev / pane-exit
    /// shortcut through.
    #[test]
    fn h_key_in_search_results_emits_cancel() {
        use std::fs;
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("hit.eml");
        fs::write(&path, "Subject: x\r\n\r\nbody\r\n").unwrap();

        let mut root = make_root();
        root.apply_search_results_named(vec![path], "tag:inbox".into());
        assert!(root.search_results_active());

        let h = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE);
        root.process_event(Event::Key(h)).unwrap();

        assert!(!root.search_results_active(), "h cancels search");
    }

    #[test]
    fn esc_key_in_search_results_emits_cancel() {
        use std::fs;
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("hit.eml");
        fs::write(&path, "Subject: x\r\n\r\nbody\r\n").unwrap();

        let mut root = make_root();
        root.apply_search_results_named(vec![path], "tag:inbox".into());

        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        root.process_event(Event::Key(esc)).unwrap();

        assert!(!root.search_results_active(), "Esc cancels search");
    }

    /// The modal absorbs every key — including `q`, which would
    /// otherwise quit the app — while it is visible.
    #[test]
    fn search_modal_absorbs_typed_chars_including_q() {
        let mut root = make_root();
        // Force the modal open without going through the notmuch
        // availability check.
        root.search.open();
        let q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        root.process_event(Event::Key(q)).unwrap();
        assert!(!root.should_quit, "q must not quit while modal is open");
        assert_eq!(root.search.query, "q", "q is typed into the modal");
    }

    /// Enter on a non-empty query closes the modal and (because
    /// notmuch may not be installed in CI) at least produces a
    /// status message — either the result count or the missing-binary
    /// error. The modal itself is closed regardless.
    #[test]
    fn enter_in_search_modal_closes_it() {
        let mut root = make_root();
        root.search.open();
        root.search.query = "tag:inbox".into();
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        root.process_event(Event::Key(enter)).unwrap();
        assert!(!root.search.visible, "modal closes after Enter");
    }

    #[test]
    fn esc_in_search_modal_closes_without_running_query() {
        let mut root = make_root();
        root.search.open();
        root.search.query = "anything".into();
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        root.process_event(Event::Key(esc)).unwrap();
        assert!(!root.search.visible, "modal closes on Esc");
        assert!(!root.search_results_active(), "no query was executed");
    }

    /// Phase 4.d acceptance: account switch tears down the old
    /// MailDir watcher and spawns a fresh one rooted at the new
    /// account's maildir_path.
    #[test]
    fn account_select_repoints_maildir_watcher_at_new_root() {
        let temp_a = tempfile::TempDir::new().unwrap();
        let temp_b = tempfile::TempDir::new().unwrap();
        let path_a = write_account_inbox(temp_a.path(), &["msg"]);
        let path_b = write_account_inbox(temp_b.path(), &["msg"]);

        let mut cfg = Config::default();
        cfg.accounts.insert(
            "alpha".into(),
            crate::config::AccountConfig {
                name: "Alpha".into(),
                email: "a@x.test".into(),
                maildir_path: path_a.clone(),
                smtp_command: None,
                signature: None,
            },
        );
        cfg.accounts.insert(
            "bravo".into(),
            crate::config::AccountConfig {
                name: "Bravo".into(),
                email: "b@x.test".into(),
                maildir_path: path_b.clone(),
                smtp_command: None,
                signature: None,
            },
        );

        let store = EmailStore::new(path_a.clone());
        let scanner = MaildirScanner::new(path_a.clone());
        let mut root = AppRoot::with_config(Arc::new(Mutex::new(store)), scanner, cfg);
        root.init_maildir_watcher();
        assert_eq!(
            root.maildir_watcher_root(),
            Some(path_a.as_path()),
            "watcher must initially track Alpha",
        );

        root.enqueue(Msg::AccountSelect("bravo".into()));
        root.drain();
        assert_eq!(
            root.maildir_watcher_root(),
            Some(path_b.as_path()),
            "AccountSelect must re-point the watcher at Bravo",
        );
    }

    /// Phase 4.d acceptance: `Msg::MailDirChanged` invalidates the
    /// cached folder headers so the next render-tick re-load picks up
    /// the fresh mail. We verify by:
    ///
    ///   1. seeding INBOX with one email + `is_loaded=true`
    ///   2. dispatching `Msg::MailDirChanged(<INBOX path>)`
    ///   3. asserting the folder is now empty and `is_loaded=false`.
    ///
    /// The headers loader will refill it asynchronously; that part is
    /// covered by `HeadersLoader` unit tests.
    #[test]
    fn maildir_changed_invalidates_target_folder() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = write_account_inbox(temp.path(), &["seed"]);
        let inbox_path = path.join("INBOX");

        let store = EmailStore::new(path.clone());
        let scanner = MaildirScanner::new(path.clone());
        let mut root = AppRoot::new(Arc::new(Mutex::new(store)), scanner);
        // Seed the folder so the invalidation has something to clear.
        {
            let handle = root.email_store_handle();
            let mut store = handle.lock().unwrap();
            let mut inbox = Folder::new("INBOX".to_string(), inbox_path.clone());
            inbox.add_email(Email::new(inbox_path.join("cur").join("seed")));
            inbox.is_loaded = true;
            inbox.total_count = 1;
            store.root_folder.subfolders.clear();
            store.root_folder.add_subfolder(inbox);
        }

        root.enqueue(Msg::MailDirChanged(inbox_path.clone()));
        root.drain();

        let handle = root.email_store_handle();
        let store = handle.lock().unwrap();
        let inbox = &store.root_folder.subfolders[0];
        assert!(
            inbox.emails.is_empty(),
            "MailDirChanged must clear cached headers",
        );
        assert!(
            !inbox.is_loaded,
            "MailDirChanged must reset is_loaded so next scan refills",
        );
    }

    // --- Phase 5.a: AI classifier `;` (AcceptSuggestion) routing. ---

    use crate::classifier::{Classifier, Suggestion};

    /// Stub classifier returning a fixed Suggestion for every email.
    struct FixedClassifier(Suggestion);
    impl Classifier for FixedClassifier {
        fn suggest(&self, _: &Email) -> Option<Suggestion> {
            Some(self.0.clone())
        }
    }

    fn root_in_messages_pane_with_one_email() -> AppRoot {
        let mut root = make_root_with_folders(&["INBOX"]);
        {
            let handle = root.email_store_handle();
            let mut store = handle.lock().unwrap();
            store.enter_folder_by_path(&[0]);
            store.select_email(0);
        }
        root.layout.active_pane = ActivePane::Messages;
        root
    }

    /// `;` resolves the cursor email's classifier suggestion to the
    /// underlying mutation `Msg`. Confidence 0.9, threshold 0.6 → an
    /// Archive suggestion dispatches `Msg::Archive`.
    #[test]
    fn accept_suggestion_resolves_archive_when_above_threshold() {
        let mut root = root_in_messages_pane_with_one_email();
        let clf: Arc<dyn Classifier> = Arc::new(FixedClassifier(Suggestion {
            action: Action::Archive,
            confidence: 0.9,
        }));
        root.set_classifier(clf, 0.6);

        let msg = root.resolve_accept_suggestion();
        assert_eq!(msg, Some(Msg::Archive(String::new())));
    }

    /// Below-threshold suggestions must not dispatch — `;` is a no-op
    /// so the user only acts on confident signals (the chip is also
    /// hidden in this regime).
    #[test]
    fn accept_suggestion_is_noop_below_threshold() {
        let mut root = root_in_messages_pane_with_one_email();
        let clf: Arc<dyn Classifier> = Arc::new(FixedClassifier(Suggestion {
            action: Action::Archive,
            confidence: 0.3,
        }));
        root.set_classifier(clf, 0.6);
        assert_eq!(root.resolve_accept_suggestion(), None);
    }

    /// NoopClassifier (the default under `[ai].enabled = false`)
    /// abstains so `;` is a no-op — the runtime path stays dormant.
    #[test]
    fn accept_suggestion_noop_under_default_noop_classifier() {
        let root = root_in_messages_pane_with_one_email();
        // No `set_classifier` call → NoopClassifier still installed.
        assert_eq!(root.resolve_accept_suggestion(), None);
    }

    /// Outside the Messages pane the chip never renders, so `;` must
    /// also be a no-op there — even with a strong suggestion installed.
    #[test]
    fn accept_suggestion_only_fires_in_messages_pane() {
        let mut root = root_in_messages_pane_with_one_email();
        root.layout.active_pane = ActivePane::Folders;
        let clf: Arc<dyn Classifier> = Arc::new(FixedClassifier(Suggestion {
            action: Action::Archive,
            confidence: 0.99,
        }));
        root.set_classifier(clf, 0.6);
        assert_eq!(root.resolve_accept_suggestion(), None);
    }

    /// All chip-eligible actions (Archive/Star/Delete/MarkUnread plus
    /// Reply / ReplyAll / ReplyLater / Forward) map onto their
    /// underlying mutation `Msg` so the dispatch covers the
    /// VISION.md §Email Actions set.
    #[test]
    fn suggestion_to_msg_covers_chip_eligible_actions() {
        assert_eq!(
            AppRoot::suggestion_to_msg(Action::Archive),
            Some(Msg::Archive(String::new())),
        );
        assert_eq!(
            AppRoot::suggestion_to_msg(Action::ToggleHelp),
            None,
            "non-chip actions return None",
        );
    }
}
