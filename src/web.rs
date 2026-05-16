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
    port: u16,
    state: WebState,
}

impl WebServer {
    /// Construct (but do not start) the server. `email_store` and
    /// `focused_pane` are shared with the TUI; `body_request_tx` is
    /// the request side of the same `BodyLoader` worker the TUI feeds,
    /// so the web handlers never do `fs::read` themselves.
    pub fn new(
        port: u16,
        email_store: Arc<Mutex<EmailStore>>,
        focused_pane: Arc<AtomicU8>,
        body_request_tx: Sender<PathBuf>,
    ) -> Self {
        Self {
            port,
            state: WebState {
                email_store,
                focused_pane,
                body_request_tx,
            },
        }
    }

    /// Bind to `127.0.0.1:<port>` and serve until the listener errors.
    /// Returns the underlying I/O error wrapped in [`VulthorError`] on
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
            .route("/events", get(email_events))
            .route("/api/current-email", get(get_current_email_json))
            .with_state(self.state.clone());

        let addr = format!("127.0.0.1:{}", self.port);
        println!("Web server starting on http://{}", addr);

        let listener = TcpListener::bind(&addr).await?;
        axum::serve(listener, app).await?;

        Ok(())
    }
}

async fn serve_email(State(state): State<WebState>) -> Response {
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
    <script>
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
    <script>
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
