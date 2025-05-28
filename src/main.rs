mod config;
mod email;
mod app;
mod maildir;
mod ui;
mod input;
mod web;

use app::{App, AppState, SharedAppState};
use clap::Parser;
use config::{CliArgs, Config};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use email::EmailStore;
use input::handle_input;
use maildir::MaildirScanner;
use ratatui::{
    backend::CrosstermBackend,
    Terminal,
};
use std::{
    io,
    sync::{Arc, Mutex},
    time::Duration,
};
use ui::UI;
use web::WebServer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Parse command line arguments
    let args = CliArgs::parse();
    
    // Load configuration
    let config = match Config::load(args.config_path) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("Error loading configuration: {}", e);
            eprintln!("Using default configuration...");
            Config::default()
        }
    };

    // Initialize maildir scanner and scan emails
    println!("Scanning MailDir at: {}", config.maildir_path.display());
    let scanner = MaildirScanner::new(config.maildir_path.clone());
    let root_folder = match scanner.scan() {
        Ok(folder) => folder,
        Err(e) => {
            eprintln!("Error scanning MailDir: {}", e);
            eprintln!("Make sure the path exists and contains a valid MailDir structure.");
            std::process::exit(1);
        }
    };

    // Create email store and app
    let mut email_store = EmailStore::new(config.maildir_path);
    email_store.root_folder = root_folder;
    let app = App::new(email_store);
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

    // Create UI
    let mut ui = UI::new();

    println!("Vulthor started! Web interface available at http://127.0.0.1:{}", args.port);
    println!("Press 'q' to quit, '?' for help");

    // Main application loop
    let result = run_app(&mut terminal, &mut ui, shared_app_state.clone()).await;

    // Cleanup
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    // Abort web server
    web_handle.abort();

    // Handle any errors from the main loop
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
    app_state: SharedAppState,
) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        // Draw UI
        {
            let app = app_state.lock().unwrap();
            terminal.draw(|f| ui.draw(f, &app))?;
            
            // Check if we should quit
            if app.should_quit || matches!(app.state, AppState::Quit) {
                break;
            }
        }

        // Handle input with timeout to allow for periodic updates
        if event::poll(Duration::from_millis(100))? {
            let event = event::read()?;
            let mut app = app_state.lock().unwrap();
            
            // Clear status message on any input (except for status-setting actions)
            if !matches!(event, Event::Resize(_, _)) {
                app.clear_status();
            }
            
            let should_quit = handle_input(&mut app, event);
            if should_quit {
                break;
            }
        }
    }

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
        };

        let scanner = MaildirScanner::new(config.maildir_path.clone());
        let root_folder = scanner.scan().unwrap();
        
        let mut email_store = EmailStore::new(config.maildir_path);
        email_store.root_folder = root_folder;
        let app = App::new(email_store);

        assert!(!app.should_quit);
        assert!(matches!(app.state, AppState::FolderView));
    }

    #[test]
    fn test_config_loading() {
        // Test default config
        let config = Config::default();
        assert!(config.maildir_path.to_string_lossy().contains("Mail"));

        // Test config loading with non-existent path
        let result = Config::load(Some(PathBuf::from("/non/existent/path")));
        assert!(result.is_err());
    }
}
