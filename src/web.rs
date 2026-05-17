use crate::email::{EmailLoadState, EmailStore};
use crate::error::Result;
use crate::layout::ActivePane;
use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse, Json, Response, Sse},
    routing::get,
};
use futures::stream::{self, Stream};
use serde::Serialize;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::time::sleep;

/// State threaded through axum handlers.
///
/// Holds only the email store (locked briefly when reading the current
/// selection) and an atomic encoding of the focused pane (no lock
/// needed for the focus check).
///
/// The web server never performs `fs::read` + MIME parse on its
/// executor threads. When a handler reads a `HeadersOnly` email it
/// dispatches a request to the shared `BodyLoader` worker and returns
/// the current state immediately. The SSE poll loop keys on the
/// email's `load_state` in addition to the selection coordinates, so
/// the client refetches once the body lands.
#[derive(Clone)]
pub struct WebState {
    /// Shared store handle. Locked briefly to clone the focused email
    /// snapshot; never held across an `fs::read` or a body parse.
    pub email_store: Arc<Mutex<EmailStore>>,
    /// Encoded focused pane (see [`ActivePane::to_u8`]). Lets the
    /// server decide between rendering the selected email and showing
    /// the welcome screen without taking the store lock.
    pub focused_pane: Arc<AtomicU8>,
    /// Outbound request channel to the shared `BodyLoader` worker.
    /// Handlers forward `HeadersOnly` emails here so the body parse
    /// happens off the axum executor.
    pub body_request_tx: Sender<PathBuf>,
}

impl WebState {
    fn focused_pane(&self) -> ActivePane {
        ActivePane::from_u8(self.focused_pane.load(Ordering::Relaxed))
    }

    /// Request an off-thread body parse for `path`. The reply lands in
    /// `EmailStore` via `AppRoot::drain_loaded_bodies`. Idempotent: extra
    /// requests just produce extra (cheap) parses; the SSE refire dedups
    /// at the client.
    fn request_body_load(&self, path: PathBuf) {
        let _ = self.body_request_tx.send(path);
    }
}

#[derive(Serialize)]
struct EmailData {
    has_email: bool,
    subject: String,
    from: String,
    to: String,
    date: String,
    body_html: String,
    attachments: Vec<AttachmentData>,
    email_id: String,
}

#[derive(Serialize)]
struct AttachmentData {
    filename: String,
    content_type: String,
    size: String,
}

/// Axum-based HTML viewer for the currently focused email. Bound to
/// `127.0.0.1:<port>`, it serves rendered HTML and an SSE event stream
/// that pushes refresh notifications as the TUI's selection changes.
/// The server is render-only (VISION.md): it never sends mail and
/// never mutates the store.
pub struct WebServer {
    bind: String,
    port: u16,
    state: WebState,
}

impl WebServer {
    /// Construct (but do not start) the server. `email_store` and
    /// `focused_pane` are shared with the TUI; `body_request_tx` is
    /// the request side of the same `BodyLoader` worker the TUI feeds,
    /// so the web handlers never do `fs::read` themselves. `bind` is
    /// the IP literal validated by `Config::validate`.
    pub fn new(
        bind: String,
        port: u16,
        email_store: Arc<Mutex<EmailStore>>,
        focused_pane: Arc<AtomicU8>,
        body_request_tx: Sender<PathBuf>,
    ) -> Self {
        Self {
            bind,
            port,
            state: WebState {
                email_store,
                focused_pane,
                body_request_tx,
            },
        }
    }

    /// Bind to `<bind>:<port>` and serve until the listener errors.
    /// Returns the underlying I/O error wrapped in [`crate::error::VulthorError`] on
    /// bind / serve failure. Designed to be spawned onto a tokio
    /// runtime; the call blocks the current task for the lifetime of
    /// the server.
    pub async fn start(&self) -> Result<()> {
        let app = Router::new()
            .route("/", get(serve_email))
            .route("/health", get(health_check))
            .route("/styles.css", get(serve_styles))
            .route("/vulthor_bird.png", get(serve_bird))
            .route("/vulthor_head.png", get(serve_head))
            .route("/vulthor_letters.png", get(serve_letters))
            .route("/manifest.json", get(serve_manifest))
            .route("/sw.js", get(serve_service_worker))
            .route("/events", get(email_events))
            .route("/api/current-email", get(get_current_email_json))
            .with_state(self.state.clone());

        let addr = format!("{}:{}", self.bind, self.port);
        println!("Web server starting on http://{}", addr);

        let listener = TcpListener::bind(&addr).await?;
        axum::serve(listener, app).await?;

        Ok(())
    }
}

pub(crate) async fn serve_email(State(state): State<WebState>) -> Response {
    let pane = state.focused_pane();
    // Hold the lock just long enough to clone what we need; never call
    // `parse_from_file` under the mutex.
    let snapshot = {
        let store = match state.email_store.lock() {
            Ok(s) => s,
            Err(_) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Html("<h1>Error: Could not access application state</h1>"),
                )
                    .into_response();
            }
        };
        store.current_email_for_web(pane).cloned()
    };

    if let Some(email) = snapshot {
        if matches!(email.load_state, EmailLoadState::HeadersOnly) {
            state.request_body_load(email.file_path.clone());
        }
        Html(generate_email_html(&email)).into_response()
    } else {
        Html(generate_welcome_html()).into_response()
    }
}

async fn health_check() -> &'static str {
    "OK"
}

async fn serve_styles() -> Response {
    let css = include_str!("../static/styles.css");
    ([("content-type", "text/css")], css).into_response()
}

async fn serve_bird() -> Response {
    let logo_bytes = include_bytes!("../assets/vulthor_bird.png");
    ([("content-type", "image/png")], logo_bytes).into_response()
}

async fn serve_head() -> Response {
    let logo_bytes = include_bytes!("../assets/vulthor_head.png");
    ([("content-type", "image/png")], logo_bytes).into_response()
}

async fn serve_letters() -> Response {
    let logo_bytes = include_bytes!("../assets/vulthor_letters.png");
    ([("content-type", "image/png")], logo_bytes).into_response()
}

/// PWA web app manifest (VISION.md §HTML Viewer §PWA bonus). Wired to
/// `<link rel="manifest" href="/manifest.json">` in both rendered HTML
/// shells. The single icon entry points at the bundled `vulthor_bird.png`
/// with `sizes="any"` so Chrome/Edge accept it for the install prompt
/// without a pre-rasterized multi-size set. `theme_color` /
/// `background_color` track [`VulthorTheme::PRIMARY_HEX`] /
/// [`VulthorTheme::DARK_HEX`] so a palette rotation flows through.
pub(crate) async fn serve_manifest() -> Response {
    let body = format!(
        r#"{{
  "name": "Vulthor",
  "short_name": "Vulthor",
  "start_url": "/",
  "display": "standalone",
  "theme_color": "{theme}",
  "background_color": "{bg}",
  "icons": [
    {{
      "src": "/vulthor_bird.png",
      "sizes": "any",
      "type": "image/png"
    }}
  ]
}}"#,
        theme = crate::theme::VulthorTheme::PRIMARY_HEX,
        bg = crate::theme::VulthorTheme::DARK_HEX,
    );
    ([("content-type", "application/manifest+json")], body).into_response()
}

/// Minimal install-only service worker. It pre-caches the shell assets
/// on `install` and falls back to the cache for those same paths if the
/// network is unreachable; everything else is a straight network
/// pass-through. We are not chasing offline mail — the goal is just to
/// satisfy Chrome/Edge's installability heuristic so the OS-level
/// "Install Vulthor" entry appears (see VISION.md).
pub(crate) async fn serve_service_worker() -> Response {
    let body = r#"const CACHE = 'vulthor-shell-v1';
const SHELL = ['/', '/styles.css', '/vulthor_bird.png'];

self.addEventListener('install', (event) => {
  event.waitUntil(
    caches.open(CACHE).then((cache) => cache.addAll(SHELL))
  );
  self.skipWaiting();
});

self.addEventListener('activate', (event) => {
  event.waitUntil(self.clients.claim());
});

self.addEventListener('fetch', (event) => {
  const url = new URL(event.request.url);
  if (SHELL.includes(url.pathname)) {
    event.respondWith(
      fetch(event.request).catch(() => caches.match(event.request))
    );
  }
});
"#;
    (
        [("content-type", "application/javascript; charset=utf-8")],
        body,
    )
        .into_response()
}

async fn email_events(
    State(state): State<WebState>,
) -> Sse<impl Stream<Item = std::result::Result<axum::response::sse::Event, Infallible>>> {
    let stream = stream::unfold(None, move |last_email_id| {
        let state = state.clone();
        async move {
            loop {
                sleep(Duration::from_millis(200)).await;

                let current_email_id = {
                    let pane = state.focused_pane();
                    let store = state.email_store.lock().ok()?;
                    let folder_indices = store.current_folder.clone();
                    let email_index = store.selected_email.unwrap_or(usize::MAX);
                    // Include load_state in the key so SSE refires when the
                    // body-loader fills in the body after the initial
                    // selection event.
                    let load_tag = match store.current_email_for_web(pane) {
                        Some(e) => match e.load_state {
                            EmailLoadState::HeadersOnly => "headers",
                            EmailLoadState::FullyLoaded => "full",
                        },
                        None => "none",
                    };
                    format!("{:?}:{}:{}", folder_indices, email_index, load_tag)
                };

                if last_email_id.as_ref() != Some(&current_email_id) {
                    let event = axum::response::sse::Event::default()
                        .event("email-changed")
                        .data(&current_email_id);
                    return Some((Ok(event), Some(current_email_id)));
                }
            }
        }
    });

    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive-text"),
    )
}

async fn get_current_email_json(State(state): State<WebState>) -> Response {
    let pane = state.focused_pane();
    // Snapshot the visible state under the lock, then drop it before doing
    // any HTML/JSON work. The store lock is shared with the TUI render
    // thread, so we must never block on it.
    let snapshot = {
        let store = match state.email_store.lock() {
            Ok(s) => s,
            Err(_) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(EmailData {
                        has_email: false,
                        subject: String::new(),
                        from: String::new(),
                        to: String::new(),
                        date: String::new(),
                        body_html: "Error: Could not access application state".to_string(),
                        attachments: vec![],
                        email_id: "error".to_string(),
                    }),
                )
                    .into_response();
            }
        };
        let folder_indices = store.current_folder.clone();
        let email_index = store.selected_email.unwrap_or(usize::MAX);
        let email = store.current_email_for_web(pane).cloned();
        (folder_indices, email_index, email)
    };
    let (folder_indices, email_index, current_email) = snapshot;

    let load_tag = match &current_email {
        Some(e) => match e.load_state {
            EmailLoadState::HeadersOnly => "headers",
            EmailLoadState::FullyLoaded => "full",
        },
        None => "none",
    };
    let email_id = format!("{:?}:{}:{}", folder_indices, email_index, load_tag);

    if let Some(email) = current_email {
        if matches!(email.load_state, EmailLoadState::HeadersOnly) {
            state.request_body_load(email.file_path.clone());
        }

        let body_content = if matches!(email.load_state, EmailLoadState::HeadersOnly) {
            "<p><em>Loading body…</em></p>".to_string()
        } else if let Some(html) = &email.body_html {
            html.clone()
        } else {
            markdown_to_html(&email.body_text)
        };

        let attachments: Vec<AttachmentData> = email
            .attachments
            .iter()
            .map(|attachment| AttachmentData {
                filename: attachment.filename.clone(),
                content_type: attachment.content_type.clone(),
                size: format_file_size(attachment.size),
            })
            .collect();

        Json(EmailData {
            has_email: true,
            subject: email.headers.subject.clone(),
            from: email.headers.from.clone(),
            to: email.headers.to.clone(),
            date: email.headers.date.clone(),
            body_html: body_content,
            attachments,
            email_id,
        })
        .into_response()
    } else {
        Json(EmailData {
            has_email: false,
            subject: String::new(),
            from: String::new(),
            to: String::new(),
            date: String::new(),
            body_html: String::new(),
            attachments: vec![],
            email_id,
        })
        .into_response()
    }
}

fn generate_email_html(email: &crate::email::Email) -> String {
    let body_content = if let Some(html) = &email.body_html {
        html.clone()
    } else {
        // Convert plain text to HTML
        markdown_to_html(&email.body_text)
    };

    let attachments_html = if email.has_attachments() {
        let mut attachments_list = String::new();
        for attachment in email.attachments.iter() {
            let size_str = format_file_size(attachment.size);
            attachments_list.push_str(&format!(
                r#"<li class="attachment-item">
                    <span class="attachment-icon">📎</span>
                    <span class="attachment-name">{}</span>
                    <span class="attachment-type">({})</span>
                    <span class="attachment-size">{}</span>
                </li>"#,
                escape_html(&attachment.filename),
                escape_html(&attachment.content_type),
                size_str
            ));
        }

        format!(
            r#"<div class="attachments-section">
                <h3>Attachments</h3>
                <ul class="attachments-list">
                    {}
                </ul>
            </div>"#,
            attachments_list
        )
    } else {
        String::new()
    };

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Vulthor - {}</title>
    <link rel="stylesheet" href="/styles.css">
    <link rel="manifest" href="/manifest.json">
    <meta name='theme-color' content='#2c4f5d'>
    <script>
        if ('serviceWorker' in navigator) {{
            window.addEventListener('load', function() {{
                navigator.serviceWorker.register('/sw.js').catch(function(err) {{
                    console.log('SW registration failed:', err);
                }});
            }});
        }}

        let currentEmailId = null;
        let isLoading = false;

        const eventSource = new EventSource('/events');
        eventSource.addEventListener('email-changed', function(event) {{
            const newEmailId = event.data;
            if (newEmailId !== currentEmailId && !isLoading) {{
                loadEmailContent();
            }}
        }});
        eventSource.onerror = function(event) {{
            console.log('SSE connection error:', event);
        }};
        
        async function loadEmailContent() {{
            if (isLoading) return;
            isLoading = true;
            
            try {{
                const response = await fetch('/api/current-email');
                const emailData = await response.json();
                
                if (emailData.has_email) {{
                    updateEmailDisplay(emailData);
                    currentEmailId = emailData.email_id;
                }} else {{
                    showWelcomeMessage();
                    currentEmailId = emailData.email_id;
                }}
            }} catch (error) {{
                console.error('Error loading email:', error);
            }} finally {{
                isLoading = false;
            }}
        }}
        
        function updateEmailDisplay(emailData) {{
            document.title = 'Vulthor - ' + emailData.subject;
            document.querySelector('.email-subject').textContent = emailData.subject;
            document.querySelector('.email-from').innerHTML = '<strong>From:</strong> ' + emailData.from;
            document.querySelector('.email-to').innerHTML = '<strong>To:</strong> ' + emailData.to;
            document.querySelector('.email-date').innerHTML = '<strong>Date:</strong> ' + emailData.date;
            document.querySelector('.email-content').innerHTML = emailData.body_html;
            
            // Update attachments
            const attachmentsSection = document.querySelector('.attachments-section');
            if (emailData.attachments.length > 0) {{
                let attachmentsHtml = '<div class="attachments-section"><h3>Attachments</h3><ul class="attachments-list">';
                emailData.attachments.forEach(attachment => {{
                    attachmentsHtml += `<li class="attachment-item">
                        <span class="attachment-icon">📎</span>
                        <span class="attachment-name">${{attachment.filename}}</span>
                        <span class="attachment-type">(${{attachment.content_type}})</span>
                        <span class="attachment-size">${{attachment.size}}</span>
                    </li>`;
                }});
                attachmentsHtml += '</ul></div>';
                
                if (attachmentsSection) {{
                    attachmentsSection.outerHTML = attachmentsHtml;
                }} else {{
                    document.querySelector('.email-content').insertAdjacentHTML('afterend', attachmentsHtml);
                }}
            }} else if (attachmentsSection) {{
                attachmentsSection.remove();
            }}
            
            // Show email layout
            document.querySelector('.container').className = 'container email-view';
        }}
        
        function showWelcomeMessage() {{
            document.title = 'Vulthor - Email Client';
            document.querySelector('.container').className = 'container welcome-view';
            document.querySelector('.container').innerHTML = `
                <header class="welcome-header">
                    <img src="/vulthor_bird.png" alt="Vulthor Logo" class="welcome-logo">
                    <h1>Vulthor</h1>
                    <h2>TUI Email Client</h2>
                </header>
                
                <main class="welcome-content">
                    <div class="welcome-message">
                        <h3>Welcome to Vulthor</h3>
                        <p>No email is currently selected in the terminal interface.</p>
                        <p>To view an email here:</p>
                        <ol>
                            <li>Navigate to an email in the terminal</li>
                            <li>Select it with <kbd>Enter</kbd></li>
                            <li>The email will appear on this page</li>
                        </ol>
                    </div>
                    
                    <div class="keybindings">
                        <h3>Key Bindings</h3>
                        <div class="keybinding-grid">
                            <div class="keybinding"><kbd>j</kbd> / <kbd>k</kbd><span>Navigate up/down</span></div>
                            <div class="keybinding"><kbd>h</kbd> / <kbd>l</kbd><span>Switch views</span></div>
                            <div class="keybinding"><kbd>Tab</kbd><span>Switch panes</span></div>
                            <div class="keybinding"><kbd>Enter</kbd><span>Select item</span></div>
                            <div class="keybinding"><kbd>Alt+a</kbd><span>View attachments</span></div>
                            <div class="keybinding"><kbd>?</kbd><span>Show help</span></div>
                            <div class="keybinding"><kbd>q</kbd><span>Quit</span></div>
                        </div>
                    </div>
                </main>
                
                <footer class="app-footer">
                    <p>Served by <strong>Vulthor</strong> - TUI Email Client</p>
                </footer>
            `;
        }}
        
        // Load initial content when page loads
        window.addEventListener('load', loadEmailContent);
    </script>
</head>
<body>
    <div class="app-banner">
        <img src="/vulthor_head.png" alt="Vulthor Bird" class="logo-bird">
        <img src="/vulthor_letters.png" alt="Vulthor" class="logo-text">
    </div>
    <div class="container">
        <header class="email-header">
            <h1 class="email-subject">{}</h1>
            <div class="email-meta">
                <div class="email-from">
                    <strong>From:</strong> {}
                </div>
                <div class="email-to">
                    <strong>To:</strong> {}
                </div>
                <div class="email-date">
                    <strong>Date:</strong> {}
                </div>
            </div>
        </header>
        
        <main class="email-content">
            {}
        </main>
        
        {}
        
        <footer class="app-footer">
            <p>Served by <strong>Vulthor</strong> - TUI Email Client</p>
        </footer>
    </div>
</body>
</html>"#,
        escape_html(&email.headers.subject),
        escape_html(&email.headers.subject),
        escape_html(&email.headers.from),
        escape_html(&email.headers.to),
        escape_html(&email.headers.date),
        body_content,
        attachments_html
    )
}

fn generate_welcome_html() -> String {
    r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Vulthor - Email Client</title>
    <link rel="stylesheet" href="/styles.css">
    <link rel="manifest" href="/manifest.json">
    <meta name='theme-color' content='#2c4f5d'>
    <script>
        if ('serviceWorker' in navigator) {
            window.addEventListener('load', function() {
                navigator.serviceWorker.register('/sw.js').catch(function(err) {
                    console.log('SW registration failed:', err);
                });
            });
        }

        let currentEmailId = null;
        let isLoading = false;

        const eventSource = new EventSource('/events');
        eventSource.addEventListener('email-changed', function(event) {
            const newEmailId = event.data;
            if (newEmailId !== currentEmailId && !isLoading) {
                loadEmailContent();
            }
        });
        eventSource.onerror = function(event) {
            console.log('SSE connection error:', event);
        };
        
        async function loadEmailContent() {
            if (isLoading) return;
            isLoading = true;
            
            try {
                const response = await fetch('/api/current-email');
                const emailData = await response.json();
                
                if (emailData.has_email) {
                    updateEmailDisplay(emailData);
                    currentEmailId = emailData.email_id;
                } else {
                    showWelcomeMessage();
                    currentEmailId = emailData.email_id;
                }
            } catch (error) {
                console.error('Error loading email:', error);
            } finally {
                isLoading = false;
            }
        }
        
        function updateEmailDisplay(emailData) {
            document.title = 'Vulthor - ' + emailData.subject;
            
            // Check if we need to create the email layout (transitioning from welcome screen)
            if (!document.querySelector('.email-header')) {
                // Add banner if not present
                if (!document.querySelector('.app-banner')) {
                    const banner = document.createElement('div');
                    banner.className = 'app-banner';
                    banner.innerHTML = `
                        <img src="/vulthor_head.png" alt="Vulthor Bird" class="logo-bird">
                        <img src="/vulthor_letters.png" alt="Vulthor" class="logo-text">
                    `;
                    document.body.insertBefore(banner, document.body.firstChild);
                }
                
                document.querySelector('.container').className = 'container email-view';
                document.querySelector('.container').innerHTML = `
                    <header class="email-header">
                        <h1 class="email-subject"></h1>
                        <div class="email-meta">
                            <div class="email-from"></div>
                            <div class="email-to"></div>
                            <div class="email-date"></div>
                        </div>
                    </header>
                    
                    <main class="email-content"></main>
                    
                    <footer class="app-footer">
                        <p>Served by <strong>Vulthor</strong> - TUI Email Client</p>
                    </footer>
                `;
            }
            
            // Update individual elements (preserving JavaScript connections)
            document.querySelector('.email-subject').textContent = emailData.subject;
            document.querySelector('.email-from').innerHTML = '<strong>From:</strong> ' + emailData.from;
            document.querySelector('.email-to').innerHTML = '<strong>To:</strong> ' + emailData.to;
            document.querySelector('.email-date').innerHTML = '<strong>Date:</strong> ' + emailData.date;
            document.querySelector('.email-content').innerHTML = emailData.body_html;
            
            // Update attachments
            const attachmentsSection = document.querySelector('.attachments-section');
            if (emailData.attachments.length > 0) {
                let attachmentsHtml = '<div class="attachments-section"><h3>Attachments</h3><ul class="attachments-list">';
                emailData.attachments.forEach(attachment => {
                    attachmentsHtml += `<li class="attachment-item">
                        <span class="attachment-icon">📎</span>
                        <span class="attachment-name">${attachment.filename}</span>
                        <span class="attachment-type">(${attachment.content_type})</span>
                        <span class="attachment-size">${attachment.size}</span>
                    </li>`;
                });
                attachmentsHtml += '</ul></div>';
                
                if (attachmentsSection) {
                    attachmentsSection.outerHTML = attachmentsHtml;
                } else {
                    document.querySelector('.email-content').insertAdjacentHTML('afterend', attachmentsHtml);
                }
            } else if (attachmentsSection) {
                attachmentsSection.remove();
            }
            
            // Show email layout
            document.querySelector('.container').className = 'container email-view';
        }
        
        function showWelcomeMessage() {
            document.title = 'Vulthor - Email Client';
            
            // Remove banner when showing welcome screen
            const banner = document.querySelector('.app-banner');
            if (banner) {
                banner.remove();
            }
            
            // Check if we need to create the welcome layout (transitioning from email view)
            if (!document.querySelector('.welcome-header')) {
                document.querySelector('.container').className = 'container welcome-view';
                document.querySelector('.container').innerHTML = `
                    <header class="welcome-header">
                        <img src="/vulthor_bird.png" alt="Vulthor Logo" class="welcome-logo">
                        <h1>Vulthor</h1>
                        <h2>TUI Email Client</h2>
                    </header>
                    
                    <main class="welcome-content">
                        <div class="welcome-message">
                            <h3>Welcome to Vulthor</h3>
                            <p>No email is currently selected in the terminal interface.</p>
                            <p>To view an email here:</p>
                            <ol>
                                <li>Navigate to an email in the terminal</li>
                                <li>Select it with <kbd>Enter</kbd></li>
                                <li>The email will appear on this page</li>
                            </ol>
                        </div>
                        
                        <div class="keybindings">
                            <h3>Key Bindings</h3>
                            <div class="keybinding-grid">
                                <div class="keybinding"><kbd>j</kbd> / <kbd>k</kbd><span>Navigate up/down</span></div>
                                <div class="keybinding"><kbd>h</kbd> / <kbd>l</kbd><span>Switch views</span></div>
                                <div class="keybinding"><kbd>Tab</kbd><span>Switch panes</span></div>
                                <div class="keybinding"><kbd>Enter</kbd><span>Select item</span></div>
                                <div class="keybinding"><kbd>Alt+a</kbd><span>View attachments</span></div>
                                <div class="keybinding"><kbd>?</kbd><span>Show help</span></div>
                                <div class="keybinding"><kbd>q</kbd><span>Quit</span></div>
                            </div>
                        </div>
                    </main>
                    
                    <footer class="app-footer">
                        <p>Served by <strong>Vulthor</strong> - TUI Email Client</p>
                    </footer>
                `;
            } else {
                // Welcome layout already exists, just ensure correct styling
                document.querySelector('.container').className = 'container welcome-view';
            }
        }
        
        // Load initial content when page loads
        window.addEventListener('load', loadEmailContent);
    </script>
</head>
<body>
    <div class="container">
        <header class="welcome-header">
            <img src="/vulthor_bird.png" alt="Vulthor Logo" class="welcome-logo">
            <h1>Vulthor</h1>
            <h2>TUI Email Client</h2>
        </header>
        
        <main class="welcome-content">
            <div class="welcome-message">
                <h3>Welcome to Vulthor</h3>
                <p>No email is currently selected in the terminal interface.</p>
                <p>To view an email here:</p>
                <ol>
                    <li>Navigate to an email in the terminal</li>
                    <li>Select it with <kbd>Enter</kbd></li>
                    <li>The email will appear on this page</li>
                </ol>
            </div>
            
            <div class="keybindings">
                <h3>Key Bindings</h3>
                <div class="keybinding-grid">
                    <div class="keybinding">
                        <kbd>j</kbd> / <kbd>k</kbd>
                        <span>Navigate up/down</span>
                    </div>
                    <div class="keybinding">
                        <kbd>h</kbd> / <kbd>l</kbd>
                        <span>Switch views</span>
                    </div>
                    <div class="keybinding">
                        <kbd>Tab</kbd>
                        <span>Switch panes</span>
                    </div>
                    <div class="keybinding">
                        <kbd>Enter</kbd>
                        <span>Select item</span>
                    </div>
                    <div class="keybinding">
                        <kbd>Alt+a</kbd>
                        <span>View attachments</span>
                    </div>
                    <div class="keybinding">
                        <kbd>?</kbd>
                        <span>Show help</span>
                    </div>
                    <div class="keybinding">
                        <kbd>q</kbd>
                        <span>Quit</span>
                    </div>
                </div>
            </div>
        </main>
        
        <footer class="app-footer">
            <p>Served by <strong>Vulthor</strong> - TUI Email Client</p>
        </footer>
    </div>
</body>
</html>"#.to_string()
}

fn markdown_to_html(markdown: &str) -> String {
    // Simple markdown to HTML conversion
    // In a real implementation, you might want to use a proper markdown parser
    let mut html = String::new();
    let lines: Vec<&str> = markdown.lines().collect();

    let mut in_paragraph = false;

    for line in lines {
        let trimmed = line.trim();

        if trimmed.is_empty() {
            if in_paragraph {
                html.push_str("</p>\n");
                in_paragraph = false;
            }
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("# ") {
            if in_paragraph {
                html.push_str("</p>\n");
                in_paragraph = false;
            }
            html.push_str(&format!("<h1>{}</h1>\n", escape_html(rest)));
        } else if let Some(rest) = trimmed.strip_prefix("## ") {
            if in_paragraph {
                html.push_str("</p>\n");
                in_paragraph = false;
            }
            html.push_str(&format!("<h2>{}</h2>\n", escape_html(rest)));
        } else if let Some(rest) = trimmed.strip_prefix("### ") {
            if in_paragraph {
                html.push_str("</p>\n");
                in_paragraph = false;
            }
            html.push_str(&format!("<h3>{}</h3>\n", escape_html(rest)));
        } else if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
            if in_paragraph {
                html.push_str("</p>\n");
                in_paragraph = false;
            }
            html.push_str(&format!(
                "<ul><li>{}</li></ul>\n",
                escape_html(&trimmed[2..])
            ));
        } else {
            if !in_paragraph {
                html.push_str("<p>");
                in_paragraph = true;
            } else {
                html.push_str("<br>");
            }
            html.push_str(&escape_html(trimmed));
        }
    }

    if in_paragraph {
        html.push_str("</p>\n");
    }

    html
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

/// Result of rewriting `<img>` tags in an HTML body for privacy. See
/// [`sanitize_html_images`] for the contract.
pub(crate) struct SanitizedHtml {
    /// HTML with remote `<img>` tags replaced (or untouched, when
    /// `show_images` is true) and the count of blocked tags.
    pub html: String,
    pub blocked_count: usize,
}

/// Rewrite `<img>` tags whose `src` is a remote `http(s)://` URL into
/// `<span class="vulthor-blocked-image" data-original-src="…">[image
/// blocked]</span>` placeholders. Same-origin (`localhost`, `127.0.0.1`,
/// `[::1]`) and non-`http(s)` URIs (`data:`, `cid:`, relative paths)
/// pass through untouched.
///
/// `show_images = true` short-circuits the rewrite — the HTML is
/// returned verbatim and `blocked_count` is 0. The viewer flips this
/// when the user opts in per-message via the `ToggleImages` action.
///
/// Implemented with a tiny tag-aware scanner rather than a real HTML
/// parser: the goal is to keep `http(s)` image fetches off the wire,
/// not to be a content-security gateway. The mail body is already
/// trusted enough to render as innerHTML in the embedded viewer.
pub(crate) fn sanitize_html_images(html: &str, show_images: bool) -> SanitizedHtml {
    if show_images {
        return SanitizedHtml {
            html: html.to_string(),
            blocked_count: 0,
        };
    }

    let mut out = String::with_capacity(html.len());
    let mut blocked: usize = 0;
    let mut cursor = 0usize;

    while cursor < html.len() {
        let Some(tag_start) = find_img_start(html, cursor) else {
            out.push_str(&html[cursor..]);
            break;
        };
        out.push_str(&html[cursor..tag_start]);

        let Some(rel_end) = find_tag_end(&html[tag_start..]) else {
            // Malformed (`<img …` with no `>`): emit the rest verbatim.
            out.push_str(&html[tag_start..]);
            break;
        };
        let tag_end = tag_start + rel_end; // index of '>'
        let tag = &html[tag_start..=tag_end];

        let src = extract_attr_value(tag, "src");
        let is_blocked = src
            .as_deref()
            .map(is_remote_image_url)
            .unwrap_or(false);
        if is_blocked {
            let src_str = src.unwrap_or_default();
            let alt = extract_attr_value(tag, "alt").unwrap_or_default();
            let label = if alt.trim().is_empty() {
                "image blocked".to_string()
            } else {
                format!("image blocked: {}", alt)
            };
            out.push_str(&format!(
                r#"<span class="vulthor-blocked-image" data-original-src="{}">[{}]</span>"#,
                escape_html(&src_str),
                escape_html(&label),
            ));
            blocked += 1;
        } else {
            out.push_str(tag);
        }
        cursor = tag_end + 1;
    }

    SanitizedHtml {
        html: out,
        blocked_count: blocked,
    }
}

/// Locate the next `<img` tag opening at or after `from`, requiring the
/// next byte to be a tag-name boundary so a tag like `<image>` (the
/// SVG element) is not misread as `<img>`.
fn find_img_start(s: &str, from: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = from;
    while i + 4 <= bytes.len() {
        if bytes[i] == b'<'
            && bytes[i + 1].eq_ignore_ascii_case(&b'i')
            && bytes[i + 2].eq_ignore_ascii_case(&b'm')
            && bytes[i + 3].eq_ignore_ascii_case(&b'g')
        {
            let after = bytes.get(i + 4).copied().unwrap_or(b' ');
            if matches!(after, b' ' | b'\t' | b'\n' | b'\r' | b'/' | b'>') {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Find the byte offset of the first `>` that closes the tag starting
/// at `tag[0] == '<'`, ignoring `>` characters inside double or single
/// quoted attribute values. Returns `None` for malformed input.
fn find_tag_end(tag: &str) -> Option<usize> {
    let bytes = tag.as_bytes();
    let mut i = 0;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let c = bytes[i];
        match quote {
            Some(q) if c == q => quote = None,
            None => match c {
                b'"' | b'\'' => quote = Some(c),
                b'>' => return Some(i),
                _ => {}
            },
            _ => {}
        }
        i += 1;
    }
    None
}

/// Return the value of `name` inside `tag` (e.g. `src="…"`), or `None`
/// when the attribute is absent. Case-insensitive on the attribute
/// name; accepts double-quoted, single-quoted, and unquoted values.
fn extract_attr_value(tag: &str, name: &str) -> Option<String> {
    let lower_tag = tag.to_ascii_lowercase();
    let lower_name = name.to_ascii_lowercase();
    let bytes = tag.as_bytes();

    // Search from after the `<imgX` so we don't catch a literal `src=` in
    // the tag name. Start at index 1 to skip the `<`.
    let mut search_from = 1usize;
    loop {
        let rest = &lower_tag[search_from..];
        let needle_pos = rest.find(&lower_name)?;
        let abs = search_from + needle_pos;

        // Confirm this is an attribute name boundary: previous char must
        // be whitespace or `<` + tag-name (i.e. start of attribute). The
        // simple check is: preceding byte is whitespace, '"', '\'' (after a
        // quoted value), or '/'. We require ASCII whitespace immediately
        // before the match to keep false positives out of attribute values.
        let prev_ok = abs == 0
            || matches!(
                bytes[abs - 1],
                b' ' | b'\t' | b'\n' | b'\r' | b'"' | b'\'' | b'/'
            );

        let after_idx = abs + lower_name.len();
        let after_ok = bytes
            .get(after_idx)
            .map(|c| matches!(*c, b'=' | b' ' | b'\t' | b'\n' | b'\r'))
            .unwrap_or(false);

        if !prev_ok || !after_ok {
            search_from = abs + lower_name.len();
            if search_from >= bytes.len() {
                return None;
            }
            continue;
        }

        // Skip whitespace, expect '='.
        let mut i = after_idx;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if bytes.get(i).copied() != Some(b'=') {
            search_from = abs + lower_name.len();
            continue;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            return None;
        }

        let q = bytes[i];
        if q == b'"' || q == b'\'' {
            i += 1;
            let start = i;
            while i < bytes.len() && bytes[i] != q {
                i += 1;
            }
            return Some(tag[start..i].to_string());
        }
        // Unquoted value.
        let start = i;
        while i < bytes.len()
            && !bytes[i].is_ascii_whitespace()
            && bytes[i] != b'>'
            && bytes[i] != b'/'
        {
            i += 1;
        }
        return Some(tag[start..i].to_string());
    }
}

/// True when `url` is a remote `http(s)://` reference. Same-origin
/// hosts (`localhost`, `127.0.0.1`, `[::1]`) are not considered remote
/// — they hit the embedded server, not the public internet. Returns
/// false for `data:`, `cid:`, and relative paths.
pub(crate) fn is_remote_image_url(url: &str) -> bool {
    let trimmed = url.trim();
    let lower = trimmed.to_ascii_lowercase();
    let rest = if let Some(r) = lower.strip_prefix("http://") {
        r
    } else if let Some(r) = lower.strip_prefix("https://") {
        r
    } else {
        return false;
    };
    let host_end = rest
        .find(|c: char| matches!(c, '/' | ':' | '?' | '#'))
        .unwrap_or(rest.len());
    let host = &rest[..host_end];
    !matches!(host, "localhost" | "127.0.0.1" | "[::1]")
}

fn format_file_size(bytes: usize) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit_index = 0;

    while size >= 1024.0 && unit_index < UNITS.len() - 1 {
        size /= 1024.0;
        unit_index += 1;
    }

    if unit_index == 0 {
        format!("{} {}", size as usize, UNITS[unit_index])
    } else {
        format!("{:.1} {}", size, UNITS[unit_index])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_html() {
        assert_eq!(escape_html("Hello & <World>"), "Hello &amp; &lt;World&gt;");
        assert_eq!(escape_html("\"quoted\""), "&quot;quoted&quot;");
    }

    #[test]
    fn test_format_file_size() {
        assert_eq!(format_file_size(512), "512 B");
        assert_eq!(format_file_size(1024), "1.0 KB");
        assert_eq!(format_file_size(1536), "1.5 KB");
        assert_eq!(format_file_size(1048576), "1.0 MB");
    }

    // --- Image sanitizer (vu-aoy) ---
    //
    // Security 5.x.a: remote <img> tags must be replaced with inert
    // placeholders by default. The user opts in per-message via the
    // ToggleImages action; until they do, the rendered HTML must not
    // emit a single http(s) image request.

    #[test]
    fn sanitize_blocks_three_remote_images_by_default() {
        let html = r#"<p>Hi</p>
<img src="https://tracker.example/pixel.gif" alt="tracking pixel">
<img src='http://cdn.example/banner.png'>
<img src="HTTPS://other.example/logo.svg" alt="logo">"#;
        let result = sanitize_html_images(html, false);
        assert_eq!(
            result.blocked_count, 3,
            "expected 3 remote images blocked, got {} in:\n{}",
            result.blocked_count, result.html,
        );
        assert!(
            !result.html.contains("<img "),
            "no <img> tags should remain when blocked:\n{}",
            result.html,
        );
        assert_eq!(
            result.html.matches("vulthor-blocked-image").count(),
            3,
            "should emit one placeholder per blocked image:\n{}",
            result.html,
        );
        // Original src preserved on the placeholder so the toggle can
        // restore it client-side without a round-trip.
        assert!(result.html.contains(r#"data-original-src="https://tracker.example/pixel.gif""#));
        assert!(result.html.contains(r#"data-original-src="http://cdn.example/banner.png""#));
        // Alt text surfaces in the placeholder label.
        assert!(result.html.contains("tracking pixel"));
        assert!(result.html.contains("logo"));
    }

    #[test]
    fn sanitize_shows_three_remote_images_when_opted_in() {
        let html = r#"<img src="https://tracker.example/pixel.gif" alt="pixel">
<img src="http://cdn.example/banner.png">
<img src="https://other.example/logo.svg">"#;
        let result = sanitize_html_images(html, true);
        assert_eq!(result.blocked_count, 0);
        assert_eq!(
            result.html.matches("<img ").count(),
            3,
            "show_images=true should leave every <img> intact:\n{}",
            result.html,
        );
        assert!(!result.html.contains("vulthor-blocked-image"));
    }

    #[test]
    fn sanitize_does_not_block_same_origin_or_data_uris() {
        let html = r#"<img src="data:image/png;base64,AAA" alt="inline">
<img src="http://localhost:8080/cid/abc" alt="local">
<img src="http://127.0.0.1/foo.png">
<img src="cid:embedded@msg" alt="cid">
<img src="/relative/path.png">"#;
        let result = sanitize_html_images(html, false);
        assert_eq!(
            result.blocked_count, 0,
            "same-origin and non-http(s) URIs must NOT be blocked:\n{}",
            result.html,
        );
        assert_eq!(result.html.matches("<img ").count(), 5);
    }

    #[test]
    fn sanitize_preserves_non_img_html_unchanged() {
        let html = r#"<p>Read the <a href="https://example.com/x">link</a>.</p>
<style>img { width: 100% }</style>"#;
        let result = sanitize_html_images(html, false);
        assert_eq!(result.blocked_count, 0);
        // Anchor href is NOT rewritten — we only intercept <img>.
        assert!(result.html.contains(r#"href="https://example.com/x""#));
        // The literal `img` token inside a <style> block must not be
        // mistaken for a tag.
        assert!(result.html.contains("img { width: 100% }"));
    }

    #[test]
    fn sanitize_handles_mixed_quote_styles_and_attribute_order() {
        let html = r#"<img alt='banner' src="https://example.com/a.png" />
<IMG SRC='https://example.com/b.png' ALT="caps">"#;
        let result = sanitize_html_images(html, false);
        assert_eq!(result.blocked_count, 2);
        assert!(result.html.contains(r#"data-original-src="https://example.com/a.png""#));
        assert!(result.html.contains(r#"data-original-src="https://example.com/b.png""#));
        assert!(result.html.contains("banner"));
        assert!(result.html.contains("caps"));
    }

    #[test]
    fn is_remote_image_url_classifies_origins_correctly() {
        // Public origins are remote.
        assert!(is_remote_image_url("https://example.com/x.png"));
        assert!(is_remote_image_url("http://tracker.example/pixel"));
        assert!(is_remote_image_url("HTTPS://EXAMPLE.com/x"));
        // Same-origin hosts are not remote.
        assert!(!is_remote_image_url("http://localhost/x"));
        assert!(!is_remote_image_url("http://localhost:8080/x"));
        assert!(!is_remote_image_url("http://127.0.0.1/x"));
        assert!(!is_remote_image_url("http://[::1]/x"));
        // Non-http(s) URIs are not remote.
        assert!(!is_remote_image_url("data:image/png;base64,AAA"));
        assert!(!is_remote_image_url("cid:abc@msg"));
        assert!(!is_remote_image_url("/relative.png"));
        assert!(!is_remote_image_url(""));
    }

    // --- PWA install surface (vu-cyj) ---
    //
    // VISION.md §HTML Viewer §PWA bonus: the rendered shell must
    // advertise a manifest and register a service worker so Chrome /
    // Edge expose an "Install Vulthor" entry. These tests pin the
    // four moving parts: the manifest route, the service-worker
    // route, and the two HTML shells linking them.

    use axum::body::to_bytes;

    #[tokio::test(flavor = "current_thread")]
    async fn manifest_route_returns_valid_json_with_pwa_fields() {
        let response = serve_manifest().await;
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            content_type.starts_with("application/manifest+json"),
            "manifest content-type was {:?}",
            content_type,
        );

        let body_bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let body = std::str::from_utf8(&body_bytes).unwrap();

        // The Chrome/Edge installability heuristic needs these four
        // fields plus at least one icon entry — assert each so a
        // partial rewrite can't silently break the install prompt.
        for required in [
            "\"name\"",
            "\"short_name\"",
            "\"start_url\"",
            "\"display\"",
            "\"icons\"",
            "/vulthor_bird.png",
        ] {
            assert!(
                body.contains(required),
                "manifest missing {}:\n{}",
                required,
                body,
            );
        }

        // Theme colors must track the VulthorTheme palette, not be
        // hardcoded in the route handler.
        assert!(
            body.contains(crate::theme::VulthorTheme::PRIMARY_HEX),
            "manifest theme_color must use VulthorTheme::PRIMARY_HEX",
        );
        assert!(
            body.contains(crate::theme::VulthorTheme::DARK_HEX),
            "manifest background_color must use VulthorTheme::DARK_HEX",
        );

        // Round-trip through a JSON parse to catch syntactic damage
        // (trailing commas, unbalanced braces) that a substring check
        // would miss.
        let parsed: serde::de::IgnoredAny =
            serde::de::Deserialize::deserialize(&mut serde_json::Deserializer::from_str(body))
                .expect("manifest must be valid JSON");
        let _ = parsed;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn service_worker_route_serves_javascript_with_install_handler() {
        let response = serve_service_worker().await;
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            content_type.contains("javascript"),
            "service worker content-type was {:?}",
            content_type,
        );

        let body_bytes = to_bytes(response.into_body(), 16 * 1024).await.unwrap();
        let body = std::str::from_utf8(&body_bytes).unwrap();

        // The install handler with `caches.open(...).addAll(...)` is
        // what registers the worker as "real" enough for the install
        // prompt. Without these, the SW is a no-op and Chrome won't
        // surface the install entry.
        for token in [
            "addEventListener('install'",
            "caches.open",
            "addAll",
            "/styles.css",
            "/vulthor_bird.png",
        ] {
            assert!(
                body.contains(token),
                "service worker missing `{}`:\n{}",
                token,
                body,
            );
        }
    }

    #[test]
    fn welcome_html_head_advertises_pwa_install_hooks() {
        let html = generate_welcome_html();
        let head_end = html.find("</head>").expect("welcome HTML must have a head");
        let head = &html[..head_end];
        assert!(
            head.contains(r#"<link rel="manifest" href="/manifest.json">"#),
            "welcome <head> must link the manifest",
        );
        assert!(
            head.contains("navigator.serviceWorker.register('/sw.js')"),
            "welcome <head> must register the service worker",
        );
    }

    #[test]
    fn email_html_head_advertises_pwa_install_hooks() {
        let email = crate::email::Email::new(PathBuf::from("/tmp/fake.eml"));
        let html = generate_email_html(&email);
        let head_end = html.find("</head>").expect("email HTML must have a head");
        let head = &html[..head_end];
        assert!(
            head.contains(r#"<link rel="manifest" href="/manifest.json">"#),
            "email <head> must link the manifest",
        );
        assert!(
            head.contains("navigator.serviceWorker.register('/sw.js')"),
            "email <head> must register the service worker",
        );
    }

    #[test]
    fn test_markdown_to_html() {
        let markdown = "# Title\nThis is a paragraph.\n\n## Subtitle\nAnother paragraph.";
        let html = markdown_to_html(markdown);
        assert!(html.contains("<h1>Title</h1>"));
        assert!(html.contains("<h2>Subtitle</h2>"));
        assert!(html.contains("<p>This is a paragraph.</p>"));
    }

    // --- Web server contention on `Mutex<EmailStore>` ---
    //
    // These tests pin the contract that web handlers never hold the
    // `EmailStore` lock across an `fs::read` and never block on disk on
    // their executor threads.

    use crate::email::{Email, EmailLoadState, Folder};
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::time::Instant;

    /// Helper: build a `WebState` wired to a real `EmailStore` containing a
    /// single `HeadersOnly` email at a non-existent path, focused on the
    /// Messages pane. Any code path that calls `parse_from_file` on this
    /// email would either error or block on a missing-file stat.
    fn webstate_with_one_headers_only_email() -> (WebState, mpsc::Receiver<PathBuf>) {
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

        let (tx, rx) = mpsc::channel::<PathBuf>();
        let state = WebState {
            email_store: Arc::new(Mutex::new(store)),
            focused_pane: Arc::new(AtomicU8::new(ActivePane::Messages.to_u8())),
            body_request_tx: tx,
        };
        (state, rx)
    }

    /// D1-D3 contract: `current_email_for_web` is purely observational. It
    /// must not transition `load_state`, must not touch disk, and must
    /// borrow the store immutably so it cannot accidentally regress to an
    /// in-line `ensure_fully_loaded` again.
    #[test]
    fn current_email_for_web_is_non_blocking_observer() {
        let (state, _rx) = webstate_with_one_headers_only_email();
        let store = state.email_store.lock().unwrap();
        let email = store
            .current_email_for_web(ActivePane::Messages)
            .expect("messages pane must surface the selected email");
        assert!(
            matches!(email.load_state, EmailLoadState::HeadersOnly),
            "web observer must not transition load_state",
        );
        assert!(
            email.body_text.is_empty(),
            "body_text must be empty until BodyLoader fills it in",
        );
        assert!(
            email.body_html.is_none(),
            "body_html must be None until BodyLoader fills it in",
        );
    }

    /// D1: `serve_email` returns within a bounded time even when the
    /// selected email's underlying file does not exist. Previously this
    /// handler called `ensure_fully_loaded` inline, which would have spent
    /// time on the missing-file stat and (in production) blocked on the
    /// MIME parse. Bounded latency proves the executor thread is no
    /// longer doing disk I/O under the lock.
    #[tokio::test(flavor = "current_thread")]
    async fn serve_email_does_not_block_on_disk() {
        let (state, rx) = webstate_with_one_headers_only_email();
        let start = Instant::now();
        let _response = serve_email(axum::extract::State(state)).await;
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(200),
            "serve_email must not block on disk; took {:?}",
            elapsed,
        );
        // The handler must have dispatched the body load to the worker.
        let dispatched = rx
            .recv_timeout(Duration::from_millis(50))
            .expect("serve_email must dispatch a body-load request for HeadersOnly emails");
        assert_eq!(
            dispatched,
            PathBuf::from("/definitely/does/not/exist/email.eml"),
        );
    }

    /// D3: `get_current_email_json` returns within a bounded time and
    /// reports the email as `loading` (placeholder body) when the body
    /// is not yet available. Mirrors the `serve_email` contract.
    #[tokio::test(flavor = "current_thread")]
    async fn get_current_email_json_does_not_block_on_disk() {
        let (state, rx) = webstate_with_one_headers_only_email();
        let start = Instant::now();
        let _response = get_current_email_json(axum::extract::State(state)).await;
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(200),
            "get_current_email_json must not block on disk; took {:?}",
            elapsed,
        );
        let dispatched = rx.recv_timeout(Duration::from_millis(50)).expect(
            "get_current_email_json must dispatch a body-load request for HeadersOnly emails",
        );
        assert_eq!(
            dispatched,
            PathBuf::from("/definitely/does/not/exist/email.eml"),
        );
    }

    /// D1-D3 contention test: while another thread is holding the
    /// `EmailStore` mutex (simulating the TUI render path mid-frame), a
    /// web request must still acquire it and return promptly once the
    /// holder releases. With the legacy code that called
    /// `ensure_fully_loaded` under the lock, the *opposite* direction
    /// (TUI waiting on web) was unbounded; this test pins the symmetric
    /// half — handlers don't hold the lock longer than a cheap snapshot.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn web_handler_does_not_deadlock_with_concurrent_lock_holder() {
        let (state, _rx) = webstate_with_one_headers_only_email();
        let store = state.email_store.clone();

        // Background thread that grabs the lock, holds it ~50ms, releases.
        // The web handler should be free to proceed immediately after.
        let holder = std::thread::spawn(move || {
            let _guard = store.lock().unwrap();
            std::thread::sleep(Duration::from_millis(50));
        });

        // Give the holder a moment to actually acquire the lock.
        tokio::time::sleep(Duration::from_millis(10)).await;

        let start = Instant::now();
        let _response = serve_email(axum::extract::State(state)).await;
        let elapsed = start.elapsed();
        holder.join().unwrap();

        // Generous bound: 50ms wait + the cheap snapshot itself. If the
        // handler did any disk I/O under the lock we'd see seconds here.
        assert!(
            elapsed < Duration::from_millis(500),
            "web handler must not deadlock with a concurrent lock holder; took {:?}",
            elapsed,
        );
    }
}
