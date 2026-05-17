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

mod classifier;
mod components;
mod compose;
mod config;
mod crash;
mod doctor;
mod email;
mod error;
mod keymap;
mod layout;
mod log;
mod maildir;
mod sanitizer;
mod stats;
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

#[cfg(test)]
mod phase4_integration_tests;

use clap::Parser;
use components::{AppRoot, FolderScannerHandle};
use config::{CliArgs, Command, Config};
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
    // vu-61a: route panics through a crash-log writer that also restores
    // the terminal — otherwise an alt-screen panic leaves the user's
    // shell in raw mode with no visible cursor.
    crash::install_panic_hook();

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

    // vu-bdy: prune aged-out routine logs and keep the rotating writer
    // alive for the process lifetime. The handle is dropped at the end
    // of `main` which closes the file; we don't yet have a logging
    // framework to wire it into, but the disk discipline is in place
    // when one lands.
    let _log_writer = match log::init(&config.log) {
        Ok(w) => Some(w),
        Err(e) => {
            eprintln!("Warning: could not initialize log file: {e}");
            None
        }
    };

    // Non-interactive subcommands fork here before any TUI / web /
    // scanner state is set up — `doctor` is a pure diagnostic and
    // must not race with the live runtime.
    if let Some(Command::Doctor) = args.command {
        let checks = doctor::run_doctor(&config);
        doctor::print_report(&checks);
        std::process::exit(doctor::exit_code(&checks));
    }

    if let Some(Command::Stats { json }) = args.command {
        let lines = stats::run_stats(&config);
        if json {
            stats::print_json(&lines);
        } else {
            stats::print_human(&lines);
        }
        std::process::exit(0);
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
    // The Ctrl+T cycle starts from `[theme].preset` (default
    // `default-dark`) only when no user theme file and no `[theme]`
    // overrides have replaced the preset base. Otherwise the resolved
    // palette doesn't match any preset, so we forget the anchor and
    // let `Ctrl+T` fall back to the first preset on press.
    let preset_anchor = match (
        &config.theme.name,
        config.theme.overrides.is_empty(),
        theme::preset_from_config(&config.theme.preset),
    ) {
        (None, true, Ok(preset)) => Some(preset),
        _ => None,
    };

    // Build the AI classifier from `[ai]` config. Phase 5.a always
    // returns NoopClassifier (chip slot blank, `;` no-op); Phase 6 will
    // start honoring `[ai].enabled = true` to load a real backend.
    let classifier = classifier::build_classifier(&config.ai);
    let ai_threshold = config.ai.threshold;

    let mut app_root = AppRoot::with_config(email_store.clone(), scanner, config);
    app_root.attach_folder_scanner(folder_scanner_handle);
    app_root.set_web_port(web_port);
    app_root.set_theme_with_preset(resolved_theme, preset_anchor);
    app_root.set_classifier(classifier, ai_threshold);
    app_root.init_maildir_watcher();

    let web_server = WebServer::new(
        web_bind.clone(),
        web_port,
        email_store.clone(),
        app_root.focused_pane(),
        app_root.body_request_sender(),
    );
    // vu-fi1: the per-launch loopback token is now the gate on every web
    // route. Capture the printable URL (token included) *before* the server
    // moves into the spawn closure — we need to surface it on the TUI
    // splash banner alongside the bind/port. Without this print the user
    // has no way to recover the token.
    let web_url = web_server.url();
    // vu-fi1 companion: if the configured bind is not a loopback address,
    // the token is the *only* protection against random LAN/Internet hits.
    // Emit a single WARN at startup so an operator who reads `[web].bind =
    // "0.0.0.0"` in their config knows what they signed up for.
    if web::is_public_bind(&web_bind) {
        eprintln!(
            "WARN: [web].bind = {} is not a loopback address; per-launch token is the only access \
             control. Treat the token URL as a secret.",
            web_bind,
        );
    }
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

    println!("Vulthor started! Web interface available at {}", web_url);
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
