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

use crate::app::{ActivePane, App, AppState, PaneSwitchDirection, SharedAppState};
use crate::config::Config;
use crate::email::EmailLoadState;
use crate::error::Result;
use crate::theme::VulthorTheme;
use crate::ui::UI;

use super::{
    BodyLoader, Component, ContentComponent, Ctx, FolderScannerHandle, FoldersComponent,
    MAX_DISPATCH_DEPTH, MessagesComponent, Msg,
};

pub struct AppRoot {
    state: SharedAppState,
    folders: FoldersComponent,
    messages: MessagesComponent,
    content: ContentComponent,
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
}

impl AppRoot {
    pub fn new(state: SharedAppState) -> Self {
        // Seed FoldersComponent from the same auto-INBOX rule App uses,
        // so the two start in sync. We read once under the lock and
        // release before storing the component.
        let initial_index = {
            let app = state.lock().unwrap();
            FoldersComponent::auto_select_inbox(&app.email_store.root_folder)
        };
        Self {
            state,
            folders: FoldersComponent::with_index(initial_index),
            messages: MessagesComponent::new(),
            content: ContentComponent::new(),
            queue: VecDeque::new(),
            body_loader: BodyLoader::spawn(),
            loading_paths: HashSet::new(),
            folder_scanner: None,
        }
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

    /// Read-only handle to the messages pane component. Used by `ui.rs`
    /// to delegate the messages and attachments panes' rendering.
    pub fn messages(&self) -> &MessagesComponent {
        &self.messages
    }

    /// Read-only handle to the content pane component. Used by `ui.rs`
    /// to delegate the content pane's rendering.
    pub fn content(&self) -> &ContentComponent {
        &self.content
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
        self.request_body_if_needed(&app);
        let folders = &self.folders;
        let messages = &self.messages;
        let content = &self.content;
        terminal.draw(|f| ui.draw(f, &mut app, folders, messages, content))?;
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
            // 2. Pane-specific keys go to the focused component first.
            let active = app.active_pane.clone();
            let ctx_msg = {
                let ctx = Self::make_ctx(&app, &self.folders);
                match active {
                    ActivePane::Folders => self.folders.on_key(key, &ctx),
                    ActivePane::Messages => self.messages.on_key(key, &ctx),
                    ActivePane::Content => self.content.on_key(key, &ctx),
                    ActivePane::Attachments => None,
                }
            };
            if let Some(msg) = ctx_msg {
                self.queue.push_back(msg);
                self.drain(&mut app);
                self.request_body_if_needed(&app);
                return Ok(app.should_quit);
            }
            // Fall through (e.g. Backspace, Enter from Messages,
            // Tab, etc.) — see sync below.
        }

        let should_quit = crate::input::handle_input(&mut app, event);
        // Legacy `handle_input` may have written `app.selection.*`
        // (Backspace resets folder/email/scroll; Enter selects
        // emails; Tab switches panes and uses remembered_email_index).
        // Pull the changes back so the components stay canonical.
        self.sync_app_to_folders(&app);
        self.sync_app_to_messages(&app);
        self.sync_app_to_content(&app);
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
            let follow_ups = {
                let ctx = Self::make_ctx(app, &self.folders);
                let mut out = self.folders.handle_msg(&msg, &ctx);
                out.extend(self.messages.handle_msg(&msg, &ctx));
                out.extend(self.content.handle_msg(&msg, &ctx));
                out
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
            Msg::ToggleContentPane => {
                app.toggle_content_pane();
                self.sync_app_to_messages(app);
                self.sync_app_to_content(app);
            }
            Msg::FocusNext => {
                // Pane switching has cross-pane hand-off logic
                // (remembered_email_index when leaving/entering
                // Messages). Mirror component state into App so
                // `App::switch_pane` reads the canonical values,
                // then sync back so the component reflects the
                // post-transition hand-off.
                self.sync_messages_to_app(app);
                app.switch_pane(PaneSwitchDirection::Right);
                self.sync_app_to_messages(app);
                self.sync_app_to_content(app);
            }
            Msg::FocusPrev => {
                self.sync_messages_to_app(app);
                app.switch_pane(PaneSwitchDirection::Left);
                self.sync_app_to_messages(app);
                self.sync_app_to_content(app);
            }
            Msg::ViewNext => {
                // The h/l view transitions stash/restore the
                // remembered_email_index. That state is canonical
                // in `MessagesComponent`; mirror in before, sync
                // out after.
                self.sync_messages_to_app(app);
                app.next_view();
                self.sync_app_to_messages(app);
                self.sync_app_to_content(app);
            }
            Msg::ViewPrev => {
                self.sync_messages_to_app(app);
                app.prev_view();
                self.sync_app_to_messages(app);
                self.sync_app_to_content(app);
            }
            Msg::FolderMove(_) => {
                // FoldersComponent already updated its index. Mirror
                // into App so the Messages pane (still legacy) sees
                // the new selection, then load the folder's messages.
                app.selection.folder_index = self.folders.folder_index;
                app.load_selected_folder_messages();
                self.sync_app_to_messages(app);
                // Emit FolderLoaded carrying the folder's filesystem
                // path so future subscribers (e.g. the forthcoming
                // MessagesComponent) can react. No component listens
                // yet; this is bookkeeping for the contract documented
                // in DESIGN-COMPONENTS.md.
                let indices = crate::input::get_folder_path_from_display_index(
                    &app.email_store.root_folder,
                    self.folders.folder_index,
                );
                if let Some(indices) = indices
                    && let Some(folder) = app.email_store.get_folder_at_path(&indices)
                {
                    self.queue.push_back(Msg::FolderLoaded(folder.path.clone()));
                }
            }
            Msg::FolderEnter => {
                // Delegate to the legacy helper; it knows how to
                // navigate `current_folder`, load emails, and switch
                // views. `folder_index` is not reset by that helper,
                // so no mirror update is needed here.
                crate::input::handle_folder_selection_and_switch_view(app);
                // `handle_folder_selection_and_switch_view` resets
                // email selection + remembered_email_index for the
                // new folder; pull those back into the component.
                self.sync_app_to_messages(app);
                self.sync_app_to_content(app);
            }
            Msg::MessageMove(_) => {
                // MessagesComponent already advanced its index.
                // Mirror into App, select the email in the store
                // (so the content pane + web see it), and run the
                // legacy load-more-on-scroll trigger.
                app.selection.email_index = self.messages.email_index;
                app.email_store.select_email(self.messages.email_index);
                if let Err(e) = app
                    .email_store
                    .load_more_messages_if_needed(&app.scanner, self.messages.email_index)
                {
                    app.set_status(format!("Error loading more messages: {}", e));
                }
                app.set_state(AppState::EmailList);
            }
            Msg::ContentScroll(_, _) => {
                // ContentComponent already updated its offset. Mirror
                // into App so legacy readers (ui.rs scrollbar) see it.
                app.selection.scroll_offset = self.content.scroll_offset;
            }
            // All other variants belong to components that haven't been
            // extracted yet. Leaving them as no-ops here is correct:
            // the legacy `input.rs` path still drives those behaviors.
            _ => {}
        }
    }

    /// Push canonical Messages-pane state into `App.selection` so the
    /// legacy `App` methods (which read from `selection`) operate on
    /// the values the component owns. Called before any `App` mutation
    /// that branches on those fields (e.g. `switch_pane`, `next_view`).
    fn sync_messages_to_app(&self, app: &mut App) {
        app.selection.email_index = self.messages.email_index;
        app.selection.remembered_email_index = self.messages.remembered_email_index;
        app.selection.attachment_index = self.messages.attachment_index;
        app.message_pane_visible_rows = self.messages.message_pane_visible_rows.get();
    }

    /// Pull `App.selection`'s Messages-pane state back into the
    /// component after any legacy path that may have mutated it
    /// (Tab/Backtab, Backspace, h/l view transitions, FolderEnter).
    fn sync_app_to_messages(&mut self, app: &App) {
        if self.messages.email_index != app.selection.email_index {
            self.messages.email_index = app.selection.email_index;
        }
        if self.messages.remembered_email_index != app.selection.remembered_email_index {
            self.messages.remembered_email_index = app.selection.remembered_email_index;
        }
        if self.messages.attachment_index != app.selection.attachment_index {
            self.messages.attachment_index = app.selection.attachment_index;
        }
        // `message_pane_visible_rows` only flows component→app
        // (render writes the component, sync_messages_to_app pushes
        // the value into `App`). No reverse direction needed.
    }

    /// Pull `App.selection.scroll_offset` back into the content
    /// component after legacy paths that reset it (Backspace).
    fn sync_app_to_content(&mut self, app: &App) {
        if self.content.scroll_offset != app.selection.scroll_offset {
            self.content.scroll_offset = app.selection.scroll_offset;
        }
    }

    /// Pull `app.selection.folder_index` into `FoldersComponent` after the
    /// legacy `handle_input` path runs. The only path that still writes
    /// the App field is `Backspace` → `handle_back_navigation`. This sync
    /// keeps the component canonical without us having to re-implement
    /// back-navigation in the component this phase.
    fn sync_app_to_folders(&mut self, app: &App) {
        if self.folders.folder_index != app.selection.folder_index {
            self.folders.folder_index = app.selection.folder_index;
        }
    }

    /// Build a fresh `Ctx` for component dispatch. Theme and config are
    /// not configurable yet (and the only component reading them is the
    /// folder pane, which uses `VulthorTheme`'s associated consts).
    /// When configuration arrives, this is the seam to widen.
    fn make_ctx<'a>(app: &'a App, folders: &FoldersComponent) -> Ctx<'a> {
        // `VulthorTheme` is a unit struct; its consts are accessed
        // through the type, not an instance, so the borrow is fine.
        Ctx {
            theme: &THEME,
            config: &CONFIG,
            store: &app.email_store,
            view: app.current_view.clone(),
            folder_index: folders.folder_index,
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
            let ctx = AppRoot::make_ctx(&app, &probe.folders);
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

    /// vu-3yj acceptance: the h/l view transitions must hand the
    /// `remembered_email_index` through `MessagesComponent`, not
    /// directly through `App.selection`. Drive a realistic flow
    /// (`l` from Folders → `j` advances email → `h` back → `l`
    /// forward) and verify the component carries the stash/restore
    /// across each step.
    #[test]
    fn remembered_email_index_handoff_round_trips_through_messages_component() {
        let mut store = EmailStore::new(PathBuf::from("/tmp"));
        let mut inbox = Folder::new("INBOX".to_string(), PathBuf::from("/tmp/INBOX"));
        for i in 0..3 {
            inbox.add_email(Email::new(PathBuf::from(format!("/tmp/INBOX/m{}", i))));
        }
        inbox.is_loaded = true;
        store.root_folder.add_subfolder(inbox);
        let scanner = MaildirScanner::new(PathBuf::from("/tmp"));
        let app = App::new(store, scanner);
        let mut root = AppRoot::new(Arc::new(Mutex::new(app)));

        // From Folders pane: 'l' enters INBOX and switches to the
        // MessagesContent view. After this, we are in the Messages
        // pane with email 0 auto-selected.
        let l = Event::Key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE));
        root.process_event(l.clone()).unwrap();
        {
            let app = root.state.lock().unwrap();
            assert_eq!(
                app.active_pane,
                crate::app::ActivePane::Messages,
                "'l' from Folders must land in Messages pane",
            );
            assert_eq!(root.messages.email_index, 0);
        }

        // 'j' from Messages pane: advances email_index in the
        // component. App.selection.email_index mirrors.
        let j = Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        root.process_event(j).unwrap();
        assert_eq!(
            root.messages.email_index, 1,
            "MessagesComponent must advance to email 1 on 'j'",
        );
        assert_eq!(
            root.state.lock().unwrap().selection.email_index,
            1,
            "App.selection.email_index must mirror the component",
        );

        // 'h' back to FolderMessages — must STASH 1 into the
        // component's remembered_email_index.
        let h = Event::Key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
        root.process_event(h).unwrap();
        assert_eq!(
            root.messages.remembered_email_index,
            Some(1),
            "h transition must stash the current email_index into the component",
        );
        assert_eq!(
            root.state.lock().unwrap().email_store.selected_email,
            None,
            "h transition must deselect so welcome screen shows",
        );

        // 'l' forward to MessagesContent — must RESTORE 1 from the
        // component's remembered_email_index. The component is the
        // source of truth; App.selection must mirror.
        root.process_event(l).unwrap();
        assert_eq!(
            root.messages.email_index, 1,
            "l transition must restore email_index from remembered slot",
        );
        let app = root.state.lock().unwrap();
        assert_eq!(
            app.selection.email_index, 1,
            "App.selection.email_index must mirror the restored value",
        );
        assert_eq!(
            app.email_store.selected_email,
            Some(1),
            "restoring must re-select the email in the store",
        );
    }

    /// vu-3yj acceptance: the `App::switch_pane` cross-pane hand-off
    /// (Messages↔Folders) must operate on `MessagesComponent`'s
    /// remembered_email_index, not just `App.selection`. Drive it
    /// after a real h/l round-trip so the test exercises both the
    /// view-transition stash AND the pane-switch restore.
    #[test]
    fn tab_pane_switch_restores_component_remembered_email_index() {
        let mut store = EmailStore::new(PathBuf::from("/tmp"));
        let mut inbox = Folder::new("INBOX".to_string(), PathBuf::from("/tmp/INBOX"));
        for i in 0..3 {
            inbox.add_email(Email::new(PathBuf::from(format!("/tmp/INBOX/m{}", i))));
        }
        inbox.is_loaded = true;
        store.root_folder.add_subfolder(inbox);
        let scanner = MaildirScanner::new(PathBuf::from("/tmp"));
        let app = App::new(store, scanner);
        let mut root = AppRoot::new(Arc::new(Mutex::new(app)));

        // Step 1: 'l' to enter INBOX (Messages pane, email 0).
        let l = Event::Key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE));
        root.process_event(l).unwrap();
        // Step 2: 'j' to advance to email 1.
        let j = Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        root.process_event(j).unwrap();
        // Step 3: 'h' back — view-transition stash through component.
        let h = Event::Key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
        root.process_event(h).unwrap();
        assert_eq!(root.messages.remembered_email_index, Some(1));
        assert_eq!(
            root.state.lock().unwrap().active_pane,
            crate::app::ActivePane::Folders,
            "h to FolderMessages defaults to Folders pane",
        );

        // Step 4: Tab Folders→Messages — pane-switch restore must
        // read the component's remembered slot, mirrored into App
        // by `sync_messages_to_app` before `switch_pane` runs, and
        // synced back out after.
        let tab = Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        root.process_event(tab).unwrap();
        assert_eq!(
            root.state.lock().unwrap().active_pane,
            crate::app::ActivePane::Messages,
        );
        assert_eq!(
            root.messages.email_index, 1,
            "Tab Folders→Messages must restore email_index from MessagesComponent's remembered slot",
        );
    }

    /// vu-3yj: ContentScroll messages are owned by ContentComponent.
    /// j/k from the Content pane must update `content.scroll_offset`
    /// and mirror into `App.selection.scroll_offset`.
    #[test]
    fn content_scroll_is_owned_by_content_component() {
        let mut store = EmailStore::new(PathBuf::from("/tmp"));
        let mut inbox = Folder::new("INBOX".to_string(), PathBuf::from("/tmp/INBOX"));
        inbox.add_email(Email::new(PathBuf::from("/tmp/INBOX/m0")));
        inbox.is_loaded = true;
        store.root_folder.add_subfolder(inbox);
        store.enter_folder_by_path(&[0]);
        store.select_email(0);
        let scanner = MaildirScanner::new(PathBuf::from("/tmp"));
        let mut app = App::new(store, scanner);
        app.active_pane = crate::app::ActivePane::Content;
        app.current_view = crate::app::View::Content;
        let mut root = AppRoot::new(Arc::new(Mutex::new(app)));

        let j = Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        root.process_event(j.clone()).unwrap();
        root.process_event(j).unwrap();
        assert_eq!(
            root.content.scroll_offset, 2,
            "two 'j' presses in Content pane must advance scroll by 2",
        );
        assert_eq!(
            root.state.lock().unwrap().selection.scroll_offset,
            2,
            "App.selection.scroll_offset must mirror the component",
        );

        // PageDown adds 10.
        let pd = Event::Key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        root.process_event(pd).unwrap();
        assert_eq!(root.content.scroll_offset, 12);
    }
}
