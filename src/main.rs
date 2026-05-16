mod app;
mod components;
mod config;
mod email;
mod error;
mod input;
mod maildir;
mod theme;
mod ui;
mod web;

#[cfg(test)]
mod test_fixtures;

use app::{App, SharedAppState};
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
    // Parse command line arguments
    let args = CliArgs::parse();

    // Load configuration
    let mut config = match Config::load(args.config_path) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("Error loading configuration: {}", e);
            eprintln!("Using default configuration...");
            Config::default()
        }
    };

    // Override maildir path if provided via CLI
    if let Some(maildir_path) = args.maildir_path {
        config.maildir_path = maildir_path;
    }

    // Phase 0.3.4 (vu-w9i): the folder-structure scan moved off the
    // main thread. We start the worker here but do NOT block on it;
    // the TUI comes up immediately and renders a splash until the
    // scan reply lands inside `AppRoot::drain_scanned_folders`.
    let scanner = MaildirScanner::new(config.maildir_path.clone());
    let folder_scanner_handle = FolderScannerHandle::spawn(config.maildir_path.clone());

    let mut email_store = EmailStore::new(config.maildir_path.clone());
    email_store.scanning_folders = true;
    let app = App::new(email_store, scanner);
    let shared_app_state: SharedAppState = Arc::new(Mutex::new(app));

    // Start web server in background
    let web_server = WebServer::new(args.port, shared_app_state.clone());
    let web_handle = tokio::spawn(async move {
        if let Err(e) = web_server.start().await {
            eprintln!("Web server error: {}", e);
        }
    });

    // Setup terminal
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

    let mut app_root = AppRoot::new(shared_app_state.clone());
    app_root.attach_folder_scanner(folder_scanner_handle);
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
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::AppState;
    use std::path::PathBuf;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_app_initialization() {
        let temp_dir = TempDir::new().unwrap();
        let config = Config {
            maildir_path: temp_dir.path().to_path_buf(),
        };

        let scanner = MaildirScanner::new(config.maildir_path.clone());
        let root_folder = scanner.scan().unwrap();

        let mut email_store = EmailStore::new(config.maildir_path.clone());
        email_store.root_folder = root_folder;
        let app = App::new(email_store, scanner);

        assert!(!app.should_quit);
        assert!(matches!(app.state, AppState::FolderView));
    }

    #[test]
    fn test_config_loading() {
        let config = Config::default();
        assert!(config.maildir_path.to_string_lossy().contains("Mail"));

        let result = Config::load(Some(PathBuf::from("/non/existent/path")));
        assert!(result.is_err());
    }
}
