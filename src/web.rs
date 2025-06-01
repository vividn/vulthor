use crate::app::SharedAppState;
use axum::{
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::get,
    Router,
};
use tokio::net::TcpListener;

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
            .with_state(self.app_state.clone());

        let addr = format!("127.0.0.1:{}", self.port);
        println!("Web server starting on http://{}", addr);

        let listener = TcpListener::bind(&addr).await?;
        axum::serve(listener, app).await?;

        Ok(())
    }
}

async fn serve_email(State(app_state): State<SharedAppState>) -> Response {
    let mut app = match app_state.lock() {
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
</head>
<body>
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
</head>
<body>
    <div class="container">
        <header class="welcome-header">
            <h1>📧 Vulthor</h1>
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
                        <kbd>Alt+h</kbd> / <kbd>Alt+l</kbd>
                        <span>Switch panes</span>
                    </div>
                    <div class="keybinding">
                        <kbd>Enter</kbd>
                        <span>Select item</span>
                    </div>
                    <div class="keybinding">
                        <kbd>Alt+e</kbd>
                        <span>Toggle folders</span>
                    </div>
                    <div class="keybinding">
                        <kbd>Alt+c</kbd>
                        <span>Toggle content</span>
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
            html.push_str(&format!("<ul><li>{}</li></ul>\n", escape_html(&trimmed[2..])));
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