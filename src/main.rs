//! Vulthor — a TUI email client with an HTML render-pane companion.
//!
//! The binary entry point wires together the four runtime concerns: the
//! component tree (`components`), the MailDir scanner that feeds it, the
//! configuration loaded from CLI + TOML, and the embedded web server used to
//! render HTML bodies in a browser. The TUI runs on the main thread; the web
//! server and folder scanner run on tokio tasks and communicate with the TUI
//! over channels.
//!
//! See `VISION.md` for product scope and `CLAUDE.md` for architectural notes.
#![deny(missing_docs)]

mod components;
mod compose;
mod config;
mod email;
mod error;
mod keymap;
mod layout;
mod maildir;
mod theme;
mod ui;
mod undo;
mod web;

#[cfg(test)]
mod test_fixtures;

#[cfg(test)]
mod integration_tests;

#[cfg(test)]
mod phase3_integration_tests;

use clap::Parser;
use components::{AppRoot, FolderScannerHandle};
use config::{CliArgs, Config};
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use email::EmailStore;
use error::Result;
use maildir::MaildirScanner;
use ratatui::{Terminal, backend::CrosstermBackend};
use std::{
    io,
    sync::{Arc, Mutex},
};
use ui::UI;
use web::WebServer;

#[tokio::main]
async fn main() -> Result<()> {
    let args = CliArgs::parse();

    let mut config = match Config::load(args.config_path).await {
        Ok(config) => config,
        Err(e) => {
            eprintln!("Error loading configuration: {}", e);
            eprintln!("Using default configuration...");
            Config::default()
        }
    };

    if let Some(maildir_path) = args.maildir_path {
        config.maildir_path = maildir_path;
    }

    // `-m` overrides the maildir for single-account runs; for
    // multi-account configs, the active account's `maildir_path`
    // wins. `Config::active_maildir()` resolves both cases.
    let initial_maildir = config.active_maildir();

    // Folder-structure scan runs off the main thread. We start the
    // worker but do NOT block; the TUI comes up immediately and
    // renders a splash until the scan reply lands in
    // `drain_scanned_folders`.
    let scanner = MaildirScanner::new(initial_maildir.clone());
    let folder_scanner_handle = FolderScannerHandle::spawn(initial_maildir.clone());

    let mut email_store = EmailStore::new(initial_maildir.clone());
    email_store.scanning_folders = true;
    let email_store: Arc<Mutex<EmailStore>> = Arc::new(Mutex::new(email_store));

    // CLI `--port` wins over `[web].port`; both default to 8080.
    let web_port = args.port.unwrap_or(config.web.port);
    let web_bind = config.web.bind.clone();

    // Resolve the runtime theme before building AppRoot so a malformed
    // user theme / override fails loud at startup instead of silently
    // rendering the built-in palette.
    let resolved_theme = match theme::build_theme(&config) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("Theme configuration error: {}", e);
            eprintln!("Falling back to built-in theme.");
            theme::Theme::default()
        }
    };

    let mut app_root = AppRoot::with_config(email_store.clone(), scanner, config);
    app_root.attach_folder_scanner(folder_scanner_handle);
    app_root.set_web_port(web_port);
    app_root.set_theme(resolved_theme);
    app_root.init_maildir_watcher();

    let web_server = WebServer::new(
        web_bind.clone(),
        web_port,
        email_store.clone(),
        app_root.focused_pane(),
        app_root.body_request_sender(),
    );
    let web_handle = tokio::spawn(async move {
        if let Err(e) = web_server.start().await {
            eprintln!("Web server error: {}", e);
        }
    });

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut ui = UI::new();

    println!(
        "Vulthor started! Web interface available at http://{}:{}",
        web_bind, web_port
    );
    println!("Press 'q' to quit, '?' for help");

    let result = run_app(&mut terminal, &mut ui, &mut app_root).await;

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    web_handle.abort();

    if let Err(e) = result {
        eprintln!("Application error: {}", e);
        return Err(e);
    }

    println!("Thank you for using Vulthor!");
    Ok(())
}

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ui: &mut UI,
    app_root: &mut AppRoot,
) -> Result<()> {
    loop {
        if app_root.render(terminal, ui)? {
            break;
        }
        if app_root.tick()? {
            break;
        }
        // Phase 2.d: drain any reply/forward editor launch parked by
        // `Msg::DraftStart`. The editor inherits stdio, so we must
        // suspend the TUI around the call. Errors get surfaced via
        // `apply_editor_failure` so the user sees them in the status
        // bar; we never propagate them up — the rest of the session
        // is still useful.
        if let Some(launch) = app_root.take_pending_editor() {
            suspend_terminal(terminal)?;
            let result = compose::launch_editor(&launch.template);
            restore_terminal(terminal)?;
            match result {
                Ok(parsed) => app_root.apply_editor_result(parsed),
                Err(e) => app_root.apply_editor_failure(e.to_string()),
            }
        }
    }
    Ok(())
}

/// Drop the alt-screen, raw mode, and mouse capture so `$EDITOR` can
/// take over stdio. The inverse of [`restore_terminal`]; the two are
/// always paired around an external program invocation.
fn suspend_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

/// Re-enter the alt-screen and raw mode after a suspend. Repaints
/// from scratch because the editor has left arbitrary text on the
/// terminal.
fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    enable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        EnterAlternateScreen,
        EnableMouseCapture
    )?;
    terminal.clear()?;
    terminal.hide_cursor()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_app_initialization() {
        let temp_dir = TempDir::new().unwrap();
        let config = Config {
            maildir_path: temp_dir.path().to_path_buf(),
            ..Config::default()
        };

        let scanner = MaildirScanner::new(config.maildir_path.clone());
        let root_folder = scanner.scan().unwrap();

        let mut email_store = EmailStore::new(config.maildir_path.clone());
        email_store.root_folder = root_folder;
        let store = Arc::new(Mutex::new(email_store));
        let app_root = AppRoot::new(store, scanner);

        assert!(!app_root.should_quit());
    }

    #[tokio::test]
    async fn test_config_loading() {
        let config = Config::default();
        assert!(config.maildir_path.to_string_lossy().contains("Mail"));

        let result = Config::load(Some(PathBuf::from("/non/existent/path"))).await;
        assert!(result.is_err());
    }
}
