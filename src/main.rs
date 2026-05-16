mod components;
mod compose;
mod config;
mod email;
mod error;
mod layout;
mod maildir;
mod theme;
mod ui;
mod undo;
mod web;

#[cfg(test)]
mod test_fixtures;

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

    let mut app_root = AppRoot::with_config(email_store.clone(), scanner, config);
    app_root.attach_folder_scanner(folder_scanner_handle);

    let web_server = WebServer::new(
        args.port,
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
        "Vulthor started! Web interface available at http://127.0.0.1:{}",
        args.port
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
        // Phase 2.b (vu-0gj): if a draft asked for $EDITOR, suspend the
        // TUI, hand the terminal to the editor, then restore and post
        // the parsed Compose back into the dispatch queue. Done between
        // tick and render so the next frame already shows the new view.
        if let Some(pending) = app_root.take_pending_editor() {
            run_editor_modal(terminal, app_root, &pending.template);
        }
    }
    Ok(())
}

/// Hand the terminal off to `$EDITOR`, restore TUI state, deliver the
/// parsed Compose (or an error message) back to AppRoot. Errors here
/// are surfaced as status messages rather than propagated, so a
/// misconfigured editor never crashes the TUI.
fn run_editor_modal(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app_root: &mut AppRoot,
    template: &str,
) {
    // 1. Suspend the TUI so the editor owns the terminal.
    if let Err(e) = disable_raw_mode() {
        app_root.deliver_editor_error(format!("disable raw mode: {}", e));
        return;
    }
    if let Err(e) = execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    ) {
        app_root.deliver_editor_error(format!("leave alt screen: {}", e));
        return;
    }

    // 2. Run the editor.
    let result = compose::launch_editor(template);

    // 3. Restore TUI state — even on editor error — so the user lands
    // back in a usable interface.
    let re_enter = enable_raw_mode().and_then(|()| {
        execute!(
            terminal.backend_mut(),
            EnterAlternateScreen,
            EnableMouseCapture
        )
    });
    let _ = terminal.clear();

    // 4. Deliver the outcome. Restore-failure is reported but does not
    // overwrite an editor-launch failure (both are unlikely; we keep
    // the editor message because it's the user's actionable signal).
    match result {
        Ok(compose) => app_root.deliver_editor_result(compose),
        Err(e) => app_root.deliver_editor_error(e.to_string()),
    }
    if let Err(e) = re_enter {
        app_root.deliver_editor_error(format!("restore TUI: {}", e));
    }
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
