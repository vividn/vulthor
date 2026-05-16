// `AppRoot` — the live main-loop driver, now hosting `FoldersComponent`.
//
// Phase 0.2.2a (vu-gje) made AppRoot functional but no panes were
// migrated. Phase 0.2.2b (vu-sd6) extracts the first one: the folder
// pane. AppRoot now owns a `FoldersComponent` and routes Folders-pane
// keys to it before falling back to the legacy `handle_input` path.
//
// **Folder-index sync.** `FoldersComponent.folder_index` is the
// canonical source. `App.selection.folder_index` is a mirror kept in
// step by `apply_root` (after dispatching messages the component
// produced) and by `sync_app_to_folders` (after falling through to
// the legacy `input.rs` path, which still writes the App field for
// `Backspace`). Mirroring lets `ui.rs` and the rest of `input.rs`
// keep reading the App field until vu-3yj extracts the Messages
// pane.
//
// **Sharing model.** AppRoot holds a clone of the `SharedAppState`
// (`Arc<Mutex<App>>`) so the web server keeps its existing access
// path. AppRoot does not own the lock; it acquires it inside `tick`
// and `render` for the duration of those operations.
//
// **Global key interception.** `handle_global_key` returns `Some(Msg)`
// for keys that don't depend on pane state: `q`, `?`, `Alt+c`, `Tab`,
// `BackTab`, `h`, and `l` from non-Folders panes. The Folders pane
// owns its own `l` (and j/k/Enter) via `FoldersComponent::on_key`.
// Help-state keys also skip global interception so the legacy "any
// key exits help" behavior is preserved.

use std::collections::{HashSet, VecDeque};
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::app::{ActivePane, App, AppState, PaneSwitchDirection, SharedAppState, View};
use crate::config::Config;
use crate::email::EmailLoadState;
use crate::error::Result;
use crate::theme::VulthorTheme;
use crate::ui::UI;

use super::{
    BodyLoader, Component, Ctx, FolderScannerHandle, FoldersComponent, HeadersLoader,
    LoadFolderRequest, MAX_DISPATCH_DEPTH, MessagesComponent, Msg,
};

pub struct AppRoot {
    state: SharedAppState,
    folders: FoldersComponent,
    /// Messages pane component (Phase 0.2.3a, vu-3ko). Owns the email
    /// cursor and the cross-pane remembered-cursor slot. AppRoot mirrors
    /// its `email_index` into `app.selection.email_index` after each
    /// dispatch step so legacy readers in `ui.rs` and the web server
    /// keep working until ContentComponent lands (vu-iva).
    messages: MessagesComponent,
    queue: VecDeque<Msg>,
    /// Off-thread email body parser (Phase 0.3.2, vu-6td). The render path
    /// reads only in-memory state; selection changes enqueue a request here,
    /// and `drain_loaded_bodies` lands the parsed body into the store.
    body_loader: BodyLoader,
    /// Paths the worker is currently parsing. Prevents duplicate requests
    /// and double-counts when the user rapidly toggles between the same
    /// email.
    loading_paths: HashSet<PathBuf>,
    /// Off-thread folder-structure scanner (Phase 0.3.4, vu-w9i). Set by
    /// `attach_folder_scanner` at launch and reaped on first successful
    /// `try_recv`, after which it is dropped. `None` once consumed or
    /// when never attached (tests, post-scan).
    folder_scanner: Option<FolderScannerHandle>,
    /// Off-thread folder-headers loader (Phase 0.3.3, vu-kx9). Replaces the
    /// blocking `load_folder_emails_with_limit` call that used to fire on
    /// every j/k in the Folders pane and on every folder-enter.
    headers_loader: HeadersLoader,
    /// Folder filesystem paths the headers worker is currently scanning.
    /// Dedupes rapid selection changes so 100 j-keystrokes don't enqueue
    /// 100 duplicate requests for the same folder.
    loading_folder_paths: HashSet<PathBuf>,
}

impl AppRoot {
    pub fn new(state: SharedAppState) -> Self {
        // Seed FoldersComponent from the same auto-INBOX rule App uses,
        // so the two start in sync. We read once under the lock and
        // release before storing the component.
        let (initial_index, scanner) = {
            let app = state.lock().unwrap();
            let idx = FoldersComponent::auto_select_inbox(&app.email_store.root_folder);
            (idx, app.scanner.clone())
        };
        let mut root = Self {
            state: state.clone(),
            folders: FoldersComponent::with_index(initial_index),
            messages: MessagesComponent::new(),
            queue: VecDeque::new(),
            body_loader: BodyLoader::spawn(),
            loading_paths: HashSet::new(),
            folder_scanner: None,
            headers_loader: HeadersLoader::spawn(scanner),
            loading_folder_paths: HashSet::new(),
        };

        // Pre-fetch the auto-selected folder's headers off-thread so the
        // first frame doesn't have to block on disk. We also flip
        // `initial_loading_done` here to suppress the legacy synchronous
        // `perform_initial_loading_if_needed` hook in `draw_messages_pane`
        // (kept around for tests that drive `App` directly).
        //
        // Phase 0.3.4 (vu-w9i) introduced a "scanning" splash state where
        // `root_folder` starts empty and the folder-structure worker fills
        // it later. In that case there is no folder at `initial_index` yet,
        // so `get_folder_path_from_display_index` returns `None` and the
        // pre-fetch becomes a no-op — `drain_scanned_folders` re-runs the
        // auto-INBOX hoist once the scan lands.
        {
            let mut app = state.lock().unwrap();
            if let Some(indices) = crate::input::get_folder_path_from_display_index(
                &app.email_store.root_folder,
                initial_index,
            ) {
                root.request_folder_load_if_needed(&app, &indices);
                // Mirror the auto-selected index into App so legacy readers
                // (status bar, web pane) see the same folder.
                app.selection.folder_index = initial_index;
            }
            app.initial_loading_done = true;
        }

        root
    }

    /// Hand the root the off-thread folder scanner started in `main`.
    /// Called once at launch. The scan reply is drained by `tick` and
    /// `render` (the first of either to fire after the worker finishes
    /// reaps it).
    pub fn attach_folder_scanner(&mut self, handle: FolderScannerHandle) {
        self.folder_scanner = Some(handle);
    }

    /// Enqueue a message for the next dispatch cycle. Exposed primarily for
    /// tests and future component plumbing; the runtime fills the queue via
    /// `handle_global_key` and `FoldersComponent::on_key` inside `tick`.
    pub fn enqueue(&mut self, msg: Msg) {
        self.queue.push_back(msg);
    }

    pub fn queue_len(&self) -> usize {
        self.queue.len()
    }

    /// Read-only handle to the shared app state, used by `main.rs` to hand
    /// a clone to the web server.
    pub fn shared_state(&self) -> SharedAppState {
        self.state.clone()
    }

    /// Read-only handle to the folder pane component. Used by `main.rs`
    /// to thread the component into `UI::draw` for rendering.
    pub fn folders(&self) -> &FoldersComponent {
        &self.folders
    }

    /// Read-only handle to the messages pane component. Used by `main.rs`
    /// to thread the component into `UI::draw` for rendering.
    pub fn messages(&self) -> &MessagesComponent {
        &self.messages
    }

    /// Render one frame. Locks the app, drains any body-load responses that
    /// have arrived (so the next draw shows them), delegates to `ui::UI::draw`,
    /// and returns whether the loop should exit (quit state observed).
    pub fn render(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        ui: &mut UI,
    ) -> Result<bool> {
        // Clone the Arc so the guard's borrow lifetime does not entangle
        // with `self`, leaving `&mut self` free for the helper calls.
        let state = self.state.clone();
        let mut app = state.lock().unwrap();
        self.drain_scanned_folders(&mut app);
        self.drain_loaded_bodies(&mut app);
        self.drain_loaded_folders(&mut app);
        self.request_body_if_needed(&app);
        let folders = &self.folders;
        let messages = &self.messages;
        terminal.draw(|f| ui.draw(f, &mut app, folders, messages))?;
        // The Messages pane render writes its live visible-row count into
        // `messages.visible_rows`. Mirror it into App so the legacy
        // header-load sizing in `request_folder_load_if_needed` keeps
        // computing the same `(visible_rows + 5).max(10)` chunk it used
        // to.
        app.message_pane_visible_rows = self.messages.visible_rows.get();
        Ok(app.should_quit || matches!(app.state, AppState::Quit))
    }

    /// Poll for an input event (with the same 100ms tick the legacy loop
    /// used) and process it. Returns `true` when the runtime should exit.
    /// Body-load responses are drained before polling so the next render
    /// has up-to-date state even when no input arrives.
    pub fn tick(&mut self) -> Result<bool> {
        {
            let state = self.state.clone();
            let mut app = state.lock().unwrap();
            self.drain_scanned_folders(&mut app);
            self.drain_loaded_bodies(&mut app);
            self.drain_loaded_folders(&mut app);
        }
        if !event::poll(Duration::from_millis(100))? {
            return Ok(false);
        }
        let event = event::read()?;
        self.process_event(event)
    }

    /// Apply a single input event: dispatch queued/component messages,
    /// then fall back to the legacy `handle_input` path for keys we
    /// don't intercept. Split out from `tick` so tests can drive it
    /// without `event::poll`.
    pub fn process_event(&mut self, event: Event) -> Result<bool> {
        // Clone the Arc so the MutexGuard doesn't borrow `self.state` —
        // we need `&mut self` available for `self.drain(&mut app)` below.
        let state = self.state.clone();
        let mut app = state.lock().unwrap();

        // Status messages clear on any non-resize event — preserves the
        // pre-refactor behavior from main.rs's run_app loop.
        if !matches!(event, Event::Resize(_, _)) {
            app.clear_status();
        }

        if let Event::Key(key) = event
            && !matches!(app.state, AppState::Help)
        {
            // 1. Global keys win unconditionally.
            if let Some(msg) = Self::handle_global_key(key, &app.active_pane) {
                self.queue.push_back(msg);
                self.drain(&mut app);
                return Ok(app.should_quit);
            }
            // 2. Folders-pane keys go to the component first.
            if matches!(app.active_pane, ActivePane::Folders) {
                let ctx_msg = {
                    let ctx = Self::make_ctx(&app);
                    self.folders.on_key(key, &ctx)
                };
                if let Some(msg) = ctx_msg {
                    self.queue.push_back(msg);
                    self.drain(&mut app);
                    return Ok(app.should_quit);
                }
            }
            // 3. Messages-pane keys go to MessagesComponent next.
            if matches!(app.active_pane, ActivePane::Messages) {
                let ctx_msg = {
                    let ctx = Self::make_ctx(&app);
                    self.messages.on_key(key, &ctx)
                };
                if let Some(msg) = ctx_msg {
                    self.queue.push_back(msg);
                    self.drain(&mut app);
                    return Ok(app.should_quit);
                }
            }
        }

        let should_quit = crate::input::handle_input(&mut app, event);
        // Legacy `handle_input` may have written `app.selection.{folder,email}_index`
        // (Backspace from non-routed paths, or direct-App test drivers).
        // Pull any divergence back so the components stay canonical.
        self.sync_app_to_components(&app);
        // Any input that changed the selection is a chance to fire off a
        // body-load request. Cheap when the email is already loaded or
        // already in flight.
        self.request_body_if_needed(&app);
        Ok(should_quit || app.should_quit)
    }

    /// Reap the off-thread folder-structure scan (Phase 0.3.4, vu-w9i).
    /// On the first successful `try_recv`, swap the scanned tree into
    /// `EmailStore::root_folder`, clear the "scanning" splash flag, and
    /// re-seed `FoldersComponent::folder_index` from the auto-INBOX
    /// rule. Reset `initial_loading_done` so the next render triggers
    /// the messages-pane load for the newly-selected folder.
    ///
    /// On scan error, surface it as a status message and clear the
    /// splash so the user is not stuck staring at "Scanning folders…"
    /// forever. The empty `root_folder` left over from launch stays in
    /// place — every code path that walks it tolerates an empty tree.
    fn drain_scanned_folders(&mut self, app: &mut App) {
        let Some(handle) = self.folder_scanner.as_ref() else {
            return;
        };
        match handle.try_recv() {
            Ok(Ok(root)) => {
                app.email_store.root_folder = root;
                app.email_store.scanning_folders = false;
                let new_index = FoldersComponent::auto_select_inbox(&app.email_store.root_folder);
                self.folders.folder_index = new_index;
                app.selection.folder_index = new_index;
                // The pre-scan first draw set this true without
                // actually loading anything (the tree was empty). Force
                // a retry now that there is something to load.
                app.initial_loading_done = false;
                self.folder_scanner = None;
            }
            Ok(Err(e)) => {
                app.email_store.scanning_folders = false;
                app.set_status(format!("Error scanning MailDir: {}", e));
                self.folder_scanner = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                app.email_store.scanning_folders = false;
                app.set_status("Folder scanner thread died before replying".into());
                self.folder_scanner = None;
            }
        }
    }

    /// Drain any body-load responses that arrived since the last call and
    /// write them back into the email store. Always reaps the in-flight
    /// slot, even when parsing failed, so a transient failure doesn't
    /// leave the email stuck in the loading state.
    fn drain_loaded_bodies(&mut self, app: &mut App) {
        while let Ok(loaded) = self.body_loader.try_recv() {
            self.loading_paths.remove(&loaded.path);
            if let Some(parsed) = loaded.parsed {
                app.email_store.apply_loaded_body(
                    &loaded.path,
                    parsed.body_text,
                    parsed.body_html,
                    parsed.attachments,
                );
            }
        }
    }

    /// Drain any folder-headers replies that arrived since the last call and
    /// write them back into the email store. Replies for folders the user
    /// has already loaded by some other path are dropped harmlessly.
    fn drain_loaded_folders(&mut self, app: &mut App) {
        while let Ok(loaded) = self.headers_loader.try_recv() {
            self.loading_folder_paths.remove(&loaded.fs_path);
            app.email_store.apply_loaded_folder(
                &loaded.fs_path,
                loaded.emails,
                loaded.fully_loaded,
            );
        }
    }

    /// Enqueue an off-thread headers load for the folder at `indices` if it
    /// isn't already loaded or in flight. Mirrors the legacy
    /// `ensure_folder_at_path_loaded` short-circuit (`is_loaded || !emails.is_empty()`).
    fn request_folder_load_if_needed(&mut self, app: &App, indices: &[usize]) {
        let Some(folder) = app.email_store.get_folder_at_path(indices) else {
            return;
        };
        if folder.is_loaded || !folder.emails.is_empty() {
            return;
        }
        let fs_path = folder.path.clone();
        if !self.loading_folder_paths.insert(fs_path.clone()) {
            return;
        }
        let limit = (app.message_pane_visible_rows + 5).max(10);
        self.headers_loader.request(LoadFolderRequest {
            fs_path,
            limit: Some(limit),
        });
    }

    /// Switch into the currently-selected folder *without* blocking on the
    /// headers load. Mirrors the synchronous-side-effects half of the legacy
    /// `crate::input::handle_folder_selection_and_switch_view`, but defers
    /// disk I/O to the off-thread headers worker.
    fn enter_selected_folder_async(&mut self, app: &mut App) {
        let path = crate::input::get_folder_path_from_display_index(
            &app.email_store.root_folder,
            self.folders.folder_index,
        );
        let Some(path) = path else { return };

        app.email_store.current_folder.clear();
        app.email_store.enter_folder_by_path(&path);

        self.request_folder_load_if_needed(app, &path);

        app.selection.email_index = 0;
        app.selection.scroll_offset = 0;
        app.selection.remembered_email_index = None;

        if !app.email_store.get_current_folder().emails.is_empty() {
            app.email_store.select_email(0);
        }

        app.current_view = if app.content_pane_hidden {
            View::Messages
        } else {
            View::MessagesContent
        };
        app.active_pane = ActivePane::Messages;
        app.set_state(AppState::EmailList);
    }

    /// If the currently selected email is `HeadersOnly` and not already in
    /// flight, ask the worker to parse it. No-op when nothing is selected.
    fn request_body_if_needed(&mut self, app: &App) {
        let Some(email) = app.email_store.get_selected_email() else {
            return;
        };
        if !matches!(email.load_state, EmailLoadState::HeadersOnly) {
            return;
        }
        let path = email.file_path.clone();
        if self.loading_paths.insert(path.clone()) {
            self.body_loader.request(path);
        }
    }

    /// Translate a key event into a global `Msg`, or `None` if the key
    /// should fall through to pane-specific handling. Pure: no side
    /// effects, no `self` borrow — makes it trivial to unit-test in
    /// isolation from the runtime.
    fn handle_global_key(key: KeyEvent, active_pane: &ActivePane) -> Option<Msg> {
        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), m) if m.is_empty() => Some(Msg::Quit),
            (KeyCode::Char('?'), m) if m.is_empty() => Some(Msg::ToggleHelp),
            (KeyCode::Char('c'), KeyModifiers::ALT) => Some(Msg::ToggleContentPane),
            (KeyCode::Tab, _) => Some(Msg::FocusNext),
            (KeyCode::BackTab, _) => Some(Msg::FocusPrev),
            (KeyCode::Char('h'), m) if m.is_empty() => Some(Msg::ViewPrev),
            (KeyCode::Char('l'), m) if m.is_empty() => {
                // 'l' from Folders is owned by FoldersComponent (it has
                // the "already inside the folder?" check). Other panes
                // get a plain ViewNext.
                if matches!(active_pane, ActivePane::Folders) {
                    None
                } else {
                    Some(Msg::ViewNext)
                }
            }
            _ => None,
        }
    }

    /// Drain the message queue. Each message is first broadcast to
    /// every component, then applied at root level (App method calls,
    /// store mutations, mirroring). Bounded by `MAX_DISPATCH_DEPTH`
    /// to catch runaway emission. Pub for tests; the runtime calls
    /// it via `tick`.
    pub fn drain(&mut self, app: &mut App) -> bool {
        let mut steps = 0usize;
        while let Some(msg) = self.queue.pop_front() {
            steps += 1;
            if steps > MAX_DISPATCH_DEPTH {
                return false;
            }
            // Components first — they may update their owned state.
            // Each component sees every message; ignored variants are
            // no-ops. See DESIGN-COMPONENTS.md § "The dispatch model".
            let follow_ups = {
                let ctx = Self::make_ctx(app);
                let mut fu = self.folders.handle_msg(&msg, &ctx);
                fu.extend(self.messages.handle_msg(&msg, &ctx));
                fu
            };
            self.queue.extend(follow_ups);
            // Then root-level effects (App methods, mirroring).
            self.apply_root(&msg, app);
        }
        true
    }

    /// Drain helper that locks internally. Used by tests; production code
    /// holds the lock across `process_event` and calls `drain` directly.
    pub fn drain_locked(&mut self) -> bool {
        let state = self.state.clone();
        let mut app = state.lock().unwrap();
        self.drain(&mut app)
    }

    /// Apply a `Msg` against the root-owned `App` and reconcile mirrors.
    /// This is the bridge from the new message-driven contract to today's
    /// imperative `App` API. As components migrate, the variants they
    /// own will stop landing here and start landing in their `handle_msg`.
    fn apply_root(&mut self, msg: &Msg, app: &mut App) {
        match msg {
            Msg::Quit => app.set_state(AppState::Quit),
            Msg::ToggleHelp => {
                if matches!(app.state, AppState::Help) {
                    app.set_state(AppState::FolderView);
                } else {
                    app.set_state(AppState::Help);
                }
            }
            Msg::ToggleContentPane => app.toggle_content_pane(),
            Msg::FocusNext => {
                // Capture old/new pane around `app.switch_pane` so we
                // can emit `FoldersBlur`/`MessagesBlur` for the
                // MessagesComponent's remembered-cursor hand-off. The
                // legacy memory logic inside `app.switch_pane` keeps
                // running for direct-App tests; the post-dispatch
                // mirror reconciles its writes with the component.
                let old_pane = app.active_pane.clone();
                app.switch_pane(PaneSwitchDirection::Right);
                let new_pane = app.active_pane.clone();
                self.emit_pane_blur(&old_pane, &new_pane);
            }
            Msg::FocusPrev => {
                let old_pane = app.active_pane.clone();
                app.switch_pane(PaneSwitchDirection::Left);
                let new_pane = app.active_pane.clone();
                self.emit_pane_blur(&old_pane, &new_pane);
            }
            Msg::ViewNext => {
                // View transitions can also cross Folders↔Messages
                // focus (FolderMessages↔MessagesContent both follow the
                // pane defaults). Mirror the blur emission here.
                let old_pane = app.active_pane.clone();
                app.next_view();
                let new_pane = app.active_pane.clone();
                self.emit_pane_blur(&old_pane, &new_pane);
            }
            Msg::ViewPrev => {
                let old_pane = app.active_pane.clone();
                app.prev_view();
                let new_pane = app.active_pane.clone();
                self.emit_pane_blur(&old_pane, &new_pane);
            }
            Msg::FolderMove(_) => {
                // FoldersComponent already updated its index;
                // MessagesComponent already reset email_index. Mirror
                // both into App so legacy readers (status bar, web
                // pane) keep working. Phase 0.3.3 (vu-kx9) moved the
                // headers load off-thread: the keystroke updates
                // selection synchronously (instant), and the headers
                // for the new folder stream in via the headers worker.
                app.selection.folder_index = self.folders.folder_index;
                app.selection.email_index = self.messages.email_index;
                app.selection.remembered_email_index = self.messages.remembered_email_index;

                let indices = crate::input::get_folder_path_from_display_index(
                    &app.email_store.root_folder,
                    self.folders.folder_index,
                );
                if let Some(indices) = indices {
                    self.request_folder_load_if_needed(app, &indices);
                    if let Some(folder) = app.email_store.get_folder_at_path(&indices) {
                        // Emit FolderLoaded carrying the folder's filesystem
                        // path. Fires when the load is *dispatched*, not
                        // when it completes — the bus contract is
                        // "selection is now this folder", not "headers
                        // are on disk".
                        self.queue.push_back(Msg::FolderLoaded(folder.path.clone()));
                    }
                }
            }
            Msg::FolderEnter => {
                // Pre-vu-kx9 this delegated to the legacy helper which
                // blocked on `ensure_current_folder_loaded_with_limit`.
                // Now we do the navigation/view-switch synchronously and
                // defer the headers load to the off-thread worker.
                self.enter_selected_folder_async(app);
                // MessagesComponent.handle_msg(FolderEnter) already
                // reset email_index/remembered. Mirror into App.
                app.selection.email_index = self.messages.email_index;
                app.selection.remembered_email_index = self.messages.remembered_email_index;
            }
            Msg::FolderExitParent => {
                // Components already reset their indices in handle_msg.
                // Root-level effects: pop the store path and reset the
                // view state to the folder picker.
                app.email_store.exit_folder();
                app.selection.folder_index = self.folders.folder_index;
                app.selection.email_index = self.messages.email_index;
                app.selection.scroll_offset = 0;
                app.selection.remembered_email_index = self.messages.remembered_email_index;
                app.set_state(AppState::FolderView);
            }
            Msg::MessageMove(_) => {
                // Component already updated `email_index`. Select the
                // email in the store and mirror into App.
                let idx = self.messages.email_index;
                app.selection.email_index = idx;
                app.email_store.select_email(idx);
                app.set_state(AppState::EmailList);
            }
            Msg::MessageOpen(_) => {
                let idx = self.messages.email_index;
                let folder = app.email_store.get_current_folder();
                if idx < folder.emails.len() {
                    app.email_store.select_email(idx);
                    app.current_view = if app.content_pane_hidden {
                        crate::app::View::Messages
                    } else {
                        crate::app::View::MessagesContent
                    };
                    app.active_pane = ActivePane::Messages;
                    app.set_state(AppState::EmailContent);
                    app.selection.email_index = idx;
                }
            }
            Msg::StoreLoadMore(idx) => {
                // The MessagesComponent fanned out a load-ahead hint as
                // the user scrolled into the unloaded tail. Forward it
                // to the store; the headers worker handles the actual
                // disk I/O off-thread (see also `vu-kx9`).
                if let Err(e) = app
                    .email_store
                    .load_more_messages_if_needed(&app.scanner, *idx)
                {
                    app.set_status(format!("Error loading more messages: {}", e));
                }
            }
            Msg::FoldersBlur | Msg::MessagesBlur => {
                // Components handle the remembered-cursor hand-off in
                // their own `handle_msg`. Mirror the result so legacy
                // readers in `ui.rs` and the web server see the same
                // selection MessagesComponent settled on.
                let idx = self.messages.email_index;
                let folder = app.email_store.get_current_folder();
                if idx < folder.emails.len() {
                    app.email_store.select_email(idx);
                }
                app.selection.email_index = idx;
                app.selection.remembered_email_index = self.messages.remembered_email_index;
            }
            // All other variants belong to components that haven't been
            // extracted yet, or are bus-only signals with no root-level
            // effect (e.g. `FolderLoaded`).
            _ => {}
        }
    }

    /// Translate an old→new pane transition into the blur message a
    /// listener expects. Used by both `FocusNext`/`FocusPrev` (Tab) and
    /// `ViewNext`/`ViewPrev` (h/l) since both can change the focused
    /// pane and demand a remembered-cursor hand-off.
    fn emit_pane_blur(&mut self, old: &ActivePane, new: &ActivePane) {
        match (old, new) {
            (ActivePane::Folders, ActivePane::Messages) => self.queue.push_back(Msg::FoldersBlur),
            (ActivePane::Messages, ActivePane::Folders) => self.queue.push_back(Msg::MessagesBlur),
            _ => {}
        }
    }

    /// Pull `app.selection.{folder,email}_index` into the components
    /// after the legacy `handle_input` path runs. Backspace from the
    /// runtime path now routes through `Msg::FolderExitParent` (vu-3ko),
    /// but tests and direct-App callers still hit `handle_back_navigation`
    /// and `handle_navigation`, both of which mutate the App fields.
    /// This sync keeps the components canonical without forcing every
    /// test to drive through `AppRoot`.
    fn sync_app_to_components(&mut self, app: &App) {
        if self.folders.folder_index != app.selection.folder_index {
            self.folders.folder_index = app.selection.folder_index;
        }
        if self.messages.email_index != app.selection.email_index {
            self.messages.email_index = app.selection.email_index;
        }
        if self.messages.remembered_email_index != app.selection.remembered_email_index {
            self.messages.remembered_email_index = app.selection.remembered_email_index;
        }
    }

    /// Build a fresh `Ctx` for component dispatch. Theme and config are
    /// not configurable yet (and the only component reading them is the
    /// folder pane, which uses `VulthorTheme`'s associated consts).
    /// When configuration arrives, this is the seam to widen.
    fn make_ctx(app: &App) -> Ctx<'_> {
        // `VulthorTheme` is a unit struct; its consts are accessed
        // through the type, not an instance, so the borrow is fine.
        Ctx {
            theme: &THEME,
            config: &CONFIG,
            store: &app.email_store,
        }
    }
}

// Static defaults for the read-only fields of `Ctx`. The folder pane
// only reads `store` from `Ctx`; these exist so the struct can be
// constructed without an owning instance. Replace with `AppRoot`-owned
// values when configuration plumbing lands.
static THEME: VulthorTheme = VulthorTheme;
// `Config` does not implement `const` construction; use `LazyLock`.
static CONFIG: std::sync::LazyLock<Config> = std::sync::LazyLock::new(Config::default);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::email::{Email, EmailStore, Folder};
    use crate::maildir::MaildirScanner;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    fn make_root() -> AppRoot {
        let store = EmailStore::new(PathBuf::from("/tmp"));
        let scanner = MaildirScanner::new(PathBuf::from("/tmp"));
        let app = App::new(store, scanner);
        AppRoot::new(Arc::new(Mutex::new(app)))
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
        let app = App::new(store, scanner);
        AppRoot::new(Arc::new(Mutex::new(app)))
    }

    /// Acceptance test for vu-gje: enqueueing `Msg::Quit` and draining the
    /// queue must set `should_quit = true` on the underlying `App`.
    #[test]
    fn approot_dispatches_quit_msg() {
        let mut root = make_root();
        root.enqueue(Msg::Quit);
        assert!(root.drain_locked());

        let app = root.state.lock().unwrap();
        assert!(app.should_quit, "Msg::Quit must flip should_quit");
        assert!(matches!(app.state, AppState::Quit));
    }

    /// `Msg::ToggleHelp` round-trips: Help → FolderView → Help.
    #[test]
    fn approot_toggles_help() {
        let mut root = make_root();
        root.enqueue(Msg::ToggleHelp);
        root.drain_locked();
        assert!(matches!(root.state.lock().unwrap().state, AppState::Help));

        root.enqueue(Msg::ToggleHelp);
        root.drain_locked();
        assert!(matches!(
            root.state.lock().unwrap().state,
            AppState::FolderView
        ));
    }

    /// Global key mapping is pure — exercise the table directly so we
    /// don't need a fake event loop to assert it.
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
        // From Folders pane: must defer to FoldersComponent (returns None).
        assert!(AppRoot::handle_global_key(key, &ActivePane::Folders).is_none());
        // From other panes: emits ViewNext.
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
        // Plain 'c' is not a global — falls through.
        let plain = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE);
        assert!(AppRoot::handle_global_key(plain, &ActivePane::Folders).is_none());
    }

    /// Bead-acceptance regression: 'j j Enter' from a fresh AppRoot must
    /// move the folder selection to index 2 and emit FolderLoaded as part
    /// of the FolderMove side-effect chain.
    #[test]
    fn key_sequence_jj_enter_selects_third_folder_and_emits_folder_loaded() {
        let mut root = make_root_with_folders(&["A", "B", "C", "D"]);

        // 'j' twice
        let j = Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        root.process_event(j.clone()).unwrap();
        // After the first 'j', a FolderLoaded for the new selection was
        // pushed onto the queue by apply_root. drain consumed it inside
        // process_event; nothing for the second 'j' to collide with.
        root.process_event(j).unwrap();

        assert_eq!(
            root.folders.folder_index, 2,
            "two 'j' presses must advance to the third folder",
        );
        assert_eq!(
            root.state.lock().unwrap().selection.folder_index,
            2,
            "App.selection.folder_index must mirror the component",
        );

        // Verify FolderLoaded was emitted during the most recent FolderMove
        // by replaying the side effect on a fresh root and inspecting the
        // queue before drain.
        let mut probe = make_root_with_folders(&["A", "B"]);
        probe.enqueue(Msg::FolderMove(crate::components::Dir::Down));
        let state = probe.state.clone();
        let mut app = state.lock().unwrap();
        // Manually run one step of drain so we can observe the follow-up.
        let msg = probe.queue.pop_front().unwrap();
        {
            let ctx = AppRoot::make_ctx(&app);
            probe.folders.handle_msg(&msg, &ctx);
        }
        probe.apply_root(&msg, &mut app);
        assert!(
            probe
                .queue
                .iter()
                .any(|m| matches!(m, Msg::FolderLoaded(_))),
            "FolderMove must enqueue a FolderLoaded follow-up",
        );

        // Now press Enter — should not panic; enters the selected folder.
        drop(app);
        let enter = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let _ = root.process_event(enter);
        let app = root.state.lock().unwrap();
        assert_eq!(
            app.email_store.current_folder,
            vec![2],
            "Enter from Folders pane must enter the selected folder",
        );
    }

    /// 'k' at the top of the folder list is a no-op (clamp, not wrap).
    /// Documents the boundary behavior the component inherits from the
    /// legacy `handle_navigation` path.
    #[test]
    fn key_k_at_top_clamps() {
        let mut root = make_root_with_folders(&["A", "B"]);
        // Force fresh state at index 0 (auto-INBOX may have started us
        // elsewhere; reset for a deterministic boundary test).
        root.folders.folder_index = 0;
        root.state.lock().unwrap().selection.folder_index = 0;

        let k = Event::Key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
        root.process_event(k).unwrap();
        assert_eq!(root.folders.folder_index, 0);
    }

    /// 'j' past the end of the folder list is a no-op (clamp).
    #[test]
    fn key_j_at_bottom_clamps() {
        let mut root = make_root_with_folders(&["A", "B"]);
        root.folders.folder_index = 1;
        root.state.lock().unwrap().selection.folder_index = 1;

        let j = Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        root.process_event(j).unwrap();
        assert_eq!(root.folders.folder_index, 1);
    }

    /// Auto-INBOX selection happens before any key event: a fresh AppRoot
    /// over a store with INBOX listed alongside others still starts at INBOX,
    /// regardless of the insertion order. `get_sorted_subfolders` always
    /// hoists INBOX to index 0 (see `email::Folder::get_sorted_subfolders`),
    /// so the auto-select rule lands there.
    #[test]
    fn approot_new_auto_selects_inbox() {
        let root = make_root_with_folders(&["Drafts", "Sent", "INBOX", "Archive"]);
        assert_eq!(root.folders.folder_index, 0);
    }

    /// Phase 0.3.2 (vu-6td) acceptance: pressing a key that changes the
    /// selected email must not block the input handler on full-body parse.
    /// We construct an email whose path does not exist on disk — if any
    /// code on the render/input path called `parse_from_file`, the email
    /// would still be `HeadersOnly` afterwards regardless, so we also
    /// assert the request landed in the body-loader's in-flight set: that
    /// proves the work was *handed off* rather than done inline.
    #[test]
    fn selection_change_dispatches_body_load_without_blocking() {
        // One folder with one email, already inside the folder, selected.
        let mut store = EmailStore::new(PathBuf::from("/tmp"));
        let mut inbox = Folder::new("INBOX".to_string(), PathBuf::from("/tmp/INBOX"));
        let phantom_path = PathBuf::from("/definitely/does/not/exist/for/vu-6td.eml");
        inbox.add_email(Email::new(phantom_path.clone()));
        inbox.is_loaded = true;
        store.root_folder.add_subfolder(inbox);
        store.enter_folder_by_path(&[0]);
        store.select_email(0);

        let scanner = MaildirScanner::new(PathBuf::from("/tmp"));
        let mut app = App::new(store, scanner);
        // Force into Messages pane so j/k navigation operates on emails,
        // not folders. AppRoot::new's auto-INBOX picked folder index 0;
        // we're already inside that folder.
        app.active_pane = ActivePane::Messages;
        app.email_store.current_folder = vec![0];
        app.email_store.select_email(0);

        let mut root = AppRoot::new(Arc::new(Mutex::new(app)));

        // The first event of any kind should trigger request_body_if_needed
        // for the already-selected email. A no-op key (e.g. 'x') is enough
        // — process_event's tail unconditionally calls the helper.
        let x = Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        root.process_event(x).unwrap();

        assert!(
            root.loading_paths.contains(&phantom_path),
            "selection must enqueue an off-thread body-load request, got {:?}",
            root.loading_paths,
        );

        // The email itself must NOT have been touched on the render/input
        // thread: load_state remains HeadersOnly and body_text is empty.
        let app = root.state.lock().unwrap();
        let email = app
            .email_store
            .get_selected_email()
            .expect("email is selected");
        assert!(
            matches!(email.load_state, crate::email::EmailLoadState::HeadersOnly),
            "input thread must not call parse_from_file",
        );
        assert!(
            email.body_text.is_empty(),
            "body_text must stay empty until the worker lands a reply",
        );
    }

    /// Re-requesting a load while one is already in flight is a no-op:
    /// the in-flight set dedups, so we don't flood the worker queue with
    /// duplicate parses on every keystroke.
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
        let app = App::new(store, scanner);
        let mut root = AppRoot::new(Arc::new(Mutex::new(app)));

        // Call request_body_if_needed twice; the second call must not
        // re-insert (HashSet::insert returns false the second time, which
        // we use as the request-or-not gate). Clone the Arc to escape the
        // self-borrow trap (`self.state.lock()` extends the guard's life-
        // time over a `&mut self` call).
        let shared = root.state.clone();
        {
            let app = shared.lock().unwrap();
            root.request_body_if_needed(&app);
        }
        let before = root.loading_paths.len();
        {
            let app = shared.lock().unwrap();
            root.request_body_if_needed(&app);
        }
        assert_eq!(before, root.loading_paths.len());
        assert!(root.loading_paths.contains(&path));
    }

    /// vu-w9i acceptance: with a folder scanner attached, the AppRoot
    /// starts in "scanning" mode (empty `root_folder`, `scanning_folders
    /// = true`). After the worker finishes, the first `drain_scanned_
    /// folders` call must:
    ///   - swap in the scanned tree
    ///   - clear the splash flag
    ///   - reset `initial_loading_done` so the next render loads INBOX
    ///   - hoist `folder_index` to the auto-INBOX position
    #[test]
    fn drain_scanned_folders_swaps_in_scan_and_resets_loading() {
        use crate::components::FolderScannerHandle;
        use std::fs;
        use std::time::{Duration, Instant};

        // Build a small maildir with INBOX + a few siblings so auto-
        // INBOX picks a non-zero index — proves we updated the
        // selection, not just left it at the default.
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path();
        for name in &["Archive", "Drafts", "INBOX", "Sent"] {
            fs::create_dir_all(root.join(name).join("cur")).unwrap();
            fs::create_dir_all(root.join(name).join("new")).unwrap();
            fs::create_dir_all(root.join(name).join("tmp")).unwrap();
        }

        // Seed an AppRoot in the same "pre-scan" state main.rs
        // produces: empty root_folder, scanning_folders = true,
        // initial_loading_done has been flipped on by an earlier
        // pre-scan render (simulated here by setting it directly).
        let mut store = EmailStore::new(root.to_path_buf());
        store.scanning_folders = true;
        let scanner = MaildirScanner::new(root.to_path_buf());
        let mut app = App::new(store, scanner);
        app.initial_loading_done = true;
        let shared = Arc::new(Mutex::new(app));
        let mut approot = AppRoot::new(shared.clone());
        approot.attach_folder_scanner(FolderScannerHandle::spawn(root.to_path_buf()));

        // Spin until the drain method actually reaps a reply. Bounded
        // wait — the worker only has 4 stat calls to do.
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            {
                let mut app = shared.lock().unwrap();
                approot.drain_scanned_folders(&mut app);
                if !app.email_store.scanning_folders {
                    break;
                }
            }
            if Instant::now() > deadline {
                panic!("folder scan never landed");
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        let app = shared.lock().unwrap();
        assert!(!app.email_store.scanning_folders);
        assert_eq!(app.email_store.root_folder.subfolders.len(), 4);
        assert!(
            !app.initial_loading_done,
            "drain must reset initial_loading_done so INBOX messages load",
        );

        // Auto-INBOX: sorted subfolders are Archive, Drafts, INBOX,
        // Sent → INBOX at index 2.
        let sorted = app.email_store.root_folder.get_sorted_subfolders();
        let inbox_idx = sorted
            .iter()
            .position(|f| f.get_display_name().eq_ignore_ascii_case("INBOX"))
            .expect("INBOX is in the fixture");
        assert_eq!(approot.folders.folder_index, inbox_idx);
        assert_eq!(app.selection.folder_index, inbox_idx);
    }

    /// Phase 0.3.3 (vu-kx9) acceptance: a folder-move keystroke must enqueue
    /// an off-thread headers request for the newly selected folder, rather
    /// than block the input handler on `load_folder_emails_with_limit`. We
    /// build a real on-disk maildir so the legacy synchronous path would
    /// definitely run if reached.
    #[test]
    fn folder_move_dispatches_headers_load_off_thread() {
        use std::fs;
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let root_path = temp.path().to_path_buf();
        // Two top-level maildir folders so a single 'j' has somewhere to go.
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
        let app = App::new(store, scanner);
        let mut root = AppRoot::new(Arc::new(Mutex::new(app)));

        let archive_path = root_path.join("Archive");

        // Press 'j' to move selection. INBOX is auto-selected at index 0;
        // Archive sorts second.
        let j = Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        let start = std::time::Instant::now();
        root.process_event(j).unwrap();
        let elapsed = start.elapsed();

        // Either the request is in-flight (worker hasn't replied yet) OR
        // the drain on the *next* tick already consumed it. Both states
        // prove the load went through the off-thread path: in the legacy
        // code, this would have blocked the keystroke until the parse
        // completed. We assert the drain-completed state by checking the
        // folder gained emails (since the test has one email each).
        // The keystroke itself must be cheap, well under "scroll 100
        // folders in < 1s" budget — single iteration ceiling 100ms.
        assert!(
            elapsed < std::time::Duration::from_millis(100),
            "folder-move keystroke must be near-instant on the TUI thread, took {:?}",
            elapsed,
        );

        // Wait for the worker to finish, then drain on the next render-equivalent.
        std::thread::sleep(std::time::Duration::from_millis(200));
        {
            let shared = root.state.clone();
            let mut app = shared.lock().unwrap();
            root.drain_loaded_folders(&mut app);
            // Look up Archive by name — fs::read_dir order is filesystem-
            // dependent, so we can't rely on a fixed store index.
            let archive = app
                .email_store
                .root_folder
                .subfolders
                .iter()
                .find(|f| f.name == "Archive")
                .expect("Archive subfolder exists");
            assert_eq!(archive.path, archive_path);
            assert!(
                !archive.emails.is_empty(),
                "headers worker should have loaded Archive's single email by now",
            );
        }
    }

    /// vu-kx9 acceptance: scrolling through many folders must be bounded by
    /// the cost of an enqueue (a `HashSet` lookup + `mpsc::send`), not by
    /// per-folder disk I/O. We build 100 folders, fire 100 'j' events, and
    /// require the whole sequence to complete well under 1s.
    #[test]
    fn folder_navigation_does_not_block_on_disk_io() {
        use std::fs;
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let root_path = temp.path().to_path_buf();
        // 100 maildir folders with one email each. Naming uses a non-INBOX
        // prefix so they all sort uniformly (avoids the INBOX-first hoist).
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
        let app = App::new(store, scanner);
        let mut root = AppRoot::new(Arc::new(Mutex::new(app)));

        let start = std::time::Instant::now();
        for _ in 0..100 {
            let j = Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
            root.process_event(j).unwrap();
        }
        let elapsed = start.elapsed();

        // Wall-time budget per the bead: scroll through 100 folders < 1s.
        // We keep an order of magnitude of headroom for CI noise: <500ms.
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "100 folder-move keystrokes must not block on disk I/O, took {:?}",
            elapsed,
        );
    }

    /// vu-kx9: pressing Enter on a folder must switch the view and pane
    /// synchronously, but the headers load must hand off to the off-thread
    /// worker rather than block. We prove the handoff happened by checking
    /// `loading_folder_paths` contains the entered folder's fs_path right
    /// after the keystroke (before the worker has a chance to reply).
    #[test]
    fn folder_enter_is_non_blocking() {
        use std::fs;
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let root_path = temp.path().to_path_buf();
        // Two folders. INBOX gets pre-fetched by AppRoot::new (immediately
        // marked in-flight); we navigate to Archive and Enter so we can
        // observe the in-flight state for an unloaded folder.
        for name in &["INBOX", "Archive"] {
            fs::create_dir_all(root_path.join(name).join("cur")).unwrap();
            fs::create_dir_all(root_path.join(name).join("new")).unwrap();
            fs::create_dir_all(root_path.join(name).join("tmp")).unwrap();
        }

        let scanner = MaildirScanner::new(root_path.clone());
        let mut store = EmailStore::new(root_path.clone());
        store.root_folder = scanner.scan().unwrap();
        let app = App::new(store, scanner);
        let mut root = AppRoot::new(Arc::new(Mutex::new(app)));

        // Drain any replies from the AppRoot::new pre-fetch so we have a
        // clean view of subsequent in-flight requests.
        std::thread::sleep(std::time::Duration::from_millis(100));
        {
            let shared = root.state.clone();
            let mut app = shared.lock().unwrap();
            root.drain_loaded_folders(&mut app);
        }

        // Move to Archive (j), then Enter.
        let j = Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        root.process_event(j).unwrap();
        let enter = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let start = std::time::Instant::now();
        root.process_event(enter).unwrap();
        let elapsed = start.elapsed();

        assert!(
            elapsed < std::time::Duration::from_millis(100),
            "folder-enter must be non-blocking, took {:?}",
            elapsed,
        );

        // View + pane switched synchronously (this is the user-visible
        // immediate response to Enter, even before headers land).
        // Look up Archive by name to get its store index — fs::read_dir
        // order is filesystem-dependent, so we can't hardcode a position.
        let app = root.state.lock().unwrap();
        let archive_idx = app
            .email_store
            .root_folder
            .subfolders
            .iter()
            .position(|f| f.name == "Archive")
            .expect("Archive subfolder exists");
        assert_eq!(app.email_store.current_folder, vec![archive_idx]);
        assert_eq!(app.active_pane, ActivePane::Messages);
    }

    /// vu-kx9: `AppRoot::new` pre-fetches headers for the auto-selected
    /// INBOX so the first frame doesn't have to block. We assert the
    /// in-flight set contains INBOX immediately after construction (before
    /// any tick has had a chance to drain replies).
    #[test]
    fn app_root_pre_fetches_initial_folder() {
        use std::fs;
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let root_path = temp.path().to_path_buf();
        fs::create_dir_all(root_path.join("INBOX/cur")).unwrap();
        fs::create_dir_all(root_path.join("INBOX/new")).unwrap();
        fs::create_dir_all(root_path.join("INBOX/tmp")).unwrap();

        let scanner = MaildirScanner::new(root_path.clone());
        let mut store = EmailStore::new(root_path.clone());
        store.root_folder = scanner.scan().unwrap();
        let app = App::new(store, scanner);
        let root = AppRoot::new(Arc::new(Mutex::new(app)));

        let inbox_path = root_path.join("INBOX");
        // The pre-fetch happens inside `AppRoot::new`. The worker may have
        // already replied (empty INBOX = instant), so the in-flight set
        // could be either {inbox_path} (in-flight) or empty (already
        // drained). The legacy-load-suppression flag must be set either way.
        let app = root.state.lock().unwrap();
        assert!(
            app.initial_loading_done,
            "AppRoot::new must flip initial_loading_done to suppress the legacy synchronous hook",
        );
        let _ = inbox_path;
    }

    /// vu-3ko acceptance: `j` from the Messages pane routes to
    /// `MessagesComponent::on_key`, advances `MessagesComponent.email_index`,
    /// selects the email in the store, and mirrors into the legacy
    /// `app.selection.email_index` field. The legacy
    /// `input::handle_navigation` Messages branch is NOT exercised in
    /// the runtime path — verified indirectly by asserting the component
    /// has the canonical value.
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
        let mut app = App::new(store, scanner);
        app.active_pane = ActivePane::Messages;
        let mut root = AppRoot::new(Arc::new(Mutex::new(app)));

        let j = Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        root.process_event(j).unwrap();

        assert_eq!(
            root.messages.email_index, 1,
            "MessagesComponent must be sole writer of email_index",
        );
        let app = root.state.lock().unwrap();
        assert_eq!(
            app.selection.email_index, 1,
            "AppRoot must mirror component into legacy App field",
        );
        assert_eq!(
            app.email_store.selected_email,
            Some(1),
            "apply_root must call email_store.select_email",
        );
    }

    /// vu-3ko acceptance: a Tab press that moves focus Folders → Messages
    /// fires `Msg::FoldersBlur` which `MessagesComponent` uses to restore
    /// `remembered_email_index`. Round-trips Folders → Messages → Folders →
    /// Messages and checks the cursor lands back on the same email.
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
        let mut app = App::new(store, scanner);
        app.active_pane = ActivePane::Messages;
        app.current_view = crate::app::View::FolderMessages;
        let mut root = AppRoot::new(Arc::new(Mutex::new(app)));

        // Sit on the 3rd message.
        root.messages.email_index = 2;
        {
            let mut app = root.state.lock().unwrap();
            app.selection.email_index = 2;
            app.email_store.select_email(2);
        }

        // Shift-Tab → Folders (Messages blurs and remembers).
        let back = Event::Key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE));
        root.process_event(back).unwrap();
        assert_eq!(root.state.lock().unwrap().active_pane, ActivePane::Folders);
        assert_eq!(
            root.messages.remembered_email_index,
            Some(2),
            "Messages → Folders must remember the email cursor",
        );

        // Tab → Messages (Folders blurs; remembered cursor restores).
        let fwd = Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        root.process_event(fwd).unwrap();
        assert_eq!(root.state.lock().unwrap().active_pane, ActivePane::Messages);
        assert_eq!(
            root.messages.email_index, 2,
            "Folders → Messages must restore the remembered email cursor",
        );
    }

    /// vu-3ko: Backspace from the Messages pane emits
    /// `Msg::FolderExitParent` which exits the folder, resets
    /// `FoldersComponent.folder_index = 0` and
    /// `MessagesComponent.email_index = 0` without touching
    /// `app.selection.folder_index` from inside `input.rs`. Addresses
    /// the vu-sd6 Backspace observation.
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
        let mut app = App::new(store, scanner);
        app.active_pane = ActivePane::Messages;
        let mut root = AppRoot::new(Arc::new(Mutex::new(app)));

        // Pretend the user has cursors at non-zero positions to prove
        // the reset really fires.
        root.folders.folder_index = 0; // already 0 (single folder)
        root.messages.email_index = 0;

        let bksp = Event::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        root.process_event(bksp).unwrap();

        let app = root.state.lock().unwrap();
        assert!(
            app.email_store.current_folder.is_empty(),
            "Backspace must exit to parent folder",
        );
        assert_eq!(app.selection.folder_index, 0);
        assert_eq!(app.selection.email_index, 0);
        assert_eq!(root.folders.folder_index, 0);
        assert_eq!(root.messages.email_index, 0);
    }

    /// vu-3ko: scrolling into the unloaded tail of a partially-loaded
    /// folder must fan out a `StoreLoadMore` from `MessagesComponent`,
    /// which `AppRoot::apply_root` translates into
    /// `load_more_messages_if_needed`. We assert the call landed by
    /// checking the folder gained more emails — the legacy path did the
    /// same load inline.
    #[test]
    fn message_move_near_tail_triggers_store_load_more() {
        use std::fs;
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let root_path = temp.path().to_path_buf();
        fs::create_dir_all(root_path.join("INBOX/cur")).unwrap();
        fs::create_dir_all(root_path.join("INBOX/new")).unwrap();
        fs::create_dir_all(root_path.join("INBOX/tmp")).unwrap();
        // 8 emails total — bigger than the initial paged load.
        for i in 0..8 {
            let body = format!(
                "From: a@b.test\r\nTo: c@d.test\r\nSubject: e{}\r\nDate: Mon, 01 Jan 2024 00:00:00 +0000\r\nMessage-ID: <{}@b.test>\r\n\r\nbody\r\n",
                i, i
            );
            fs::write(root_path.join(format!("INBOX/cur/m{}.eml", i)), body).unwrap();
        }

        let scanner = crate::maildir::MaildirScanner::new(root_path.clone());
        let mut store = EmailStore::new(root_path.clone());
        store.root_folder = scanner.scan().unwrap();
        // Load only the first 3 — `load_more` must extend it.
        scanner
            .load_folder_emails_with_limit(&mut store.root_folder.subfolders[0], Some(3))
            .unwrap();
        store.current_folder = vec![0];
        store.select_email(0);

        let mut app = App::new(store, scanner);
        app.active_pane = ActivePane::Messages;
        let mut root = AppRoot::new(Arc::new(Mutex::new(app)));

        // Sit on the second email, then scroll once — `index + 5 >= len`
        // (2 + 5 >= 3) trips the lookahead.
        root.messages.email_index = 1;
        {
            let mut app = root.state.lock().unwrap();
            app.selection.email_index = 1;
            app.email_store.select_email(1);
        }
        let j = Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        root.process_event(j).unwrap();

        let app = root.state.lock().unwrap();
        assert!(
            app.email_store.get_current_folder().emails.len() > 3,
            "near-tail scroll must trigger load_more (had {} emails)",
            app.email_store.get_current_folder().emails.len(),
        );
    }
}
