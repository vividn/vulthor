use crate::app::SharedAppState;
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
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::time::sleep;

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

pub struct WebServer {
    port: u16,
    app_state: SharedAppState,
}

impl WebServer {
    pub fn new(port: u16, app_state: SharedAppState) -> Self {
        Self { port, app_state }
    }

    pub async fn start(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let app = Router::new()
            .route("/", get(serve_email))
            .route("/health", get(health_check))
            .route("/styles.css", get(serve_styles))
            .route("/vulthor_bird.png", get(serve_bird))
            .route("/vulthor_head.png", get(serve_head))
            .route("/vulthor_letters.png", get(serve_letters))
            .route("/events", get(email_events))
            .route("/api/current-email", get(get_current_email_json))
            .with_state(self.app_state.clone());

        let addr = format!("127.0.0.1:{}", self.port);
        println!("Web server starting on http://{}", addr);

        let listener = TcpListener::bind(&addr).await?;
        axum::serve(listener, app).await?;

        Ok(())
    }
}

async fn serve_email(State(app_state): State<SharedAppState>) -> Response {
    let app = match app_state.lock() {
        Ok(app) => app,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Html("<h1>Error: Could not access application state</h1>"),
            )
                .into_response();
        }
    };

    if let Some(email) = app.get_current_email_for_web() {
        let html = generate_email_html(email);
        Html(html).into_response()
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
    State(app_state): State<SharedAppState>,
) -> Sse<impl Stream<Item = Result<axum::response::sse::Event, Infallible>>> {
    let stream = stream::unfold(None, move |last_email_id| {
        let app_state = app_state.clone();
        async move {
            loop {
                sleep(Duration::from_millis(200)).await; // Faster polling for better responsiveness

                let current_email_id = {
                    let app = app_state.lock().ok()?;
                    // Create a unique identifier that changes when displayable content changes
                    let folder_index = app.selection.folder_index;
                    let email_index = app.selection.email_index;
                    let has_email = app.get_current_email_for_web().is_some();

                    format!("{}:{}:{}", folder_index, email_index, has_email)
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

async fn get_current_email_json(State(app_state): State<SharedAppState>) -> Response {
    let app = match app_state.lock() {
        Ok(app) => app,
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

    let folder_index = app.selection.folder_index;
    let email_index = app.selection.email_index;
    let current_email = app.get_current_email_for_web();
    let has_email = current_email.is_some();
    let email_id = format!("{}:{}:{}", folder_index, email_index, has_email);

    if let Some(email) = current_email {
        let body_content = if let Some(html) = &email.body_html {
            html.clone()
        } else {
            // Convert plain text to HTML
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

        if trimmed.starts_with("# ") {
            if in_paragraph {
                html.push_str("</p>\n");
                in_paragraph = false;
            }
            html.push_str(&format!("<h1>{}</h1>\n", escape_html(&trimmed[2..])));
        } else if trimmed.starts_with("## ") {
            if in_paragraph {
                html.push_str("</p>\n");
                in_paragraph = false;
            }
            html.push_str(&format!("<h2>{}</h2>\n", escape_html(&trimmed[3..])));
        } else if trimmed.starts_with("### ") {
            if in_paragraph {
                html.push_str("</p>\n");
                in_paragraph = false;
            }
            html.push_str(&format!("<h3>{}</h3>\n", escape_html(&trimmed[4..])));
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
}
