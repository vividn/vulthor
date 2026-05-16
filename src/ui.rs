use crate::app::{ActivePane, App, AppState, View};
use crate::components::{Component, Ctx, FoldersComponent, MessagesComponent};
use crate::config::Config;
use crate::email::EmailLoadState;
use crate::theme::VulthorTheme;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation,
        ScrollbarState, Wrap,
    },
};

pub struct UI {
    attachment_list_state: ListState,
}

impl Default for UI {
    fn default() -> Self {
        Self::new()
    }
}

impl UI {
    pub fn new() -> Self {
        Self {
            attachment_list_state: ListState::default(),
        }
    }

    pub fn draw(
        &mut self,
        f: &mut Frame,
        app: &mut App,
        folders: &FoldersComponent,
        messages: &MessagesComponent,
    ) {
        let size = f.area();

        match app.state {
            AppState::Help => {
                self.draw_help_screen(f, size);
            }
            _ => {
                self.draw_main_layout(f, app, folders, messages, size);
            }
        }
    }

    fn draw_main_layout(
        &mut self,
        f: &mut Frame,
        app: &mut App,
        folders: &FoldersComponent,
        messages: &MessagesComponent,
        area: Rect,
    ) {
        match app.current_view {
            View::FolderMessages => {
                // Two panes: folders and messages
                let chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .split(area);

                let is_folders_active = matches!(app.active_pane, ActivePane::Folders);
                let is_messages_active = matches!(app.active_pane, ActivePane::Messages);

                let theme = VulthorTheme;
                let config = Config::default();
                let ctx = Ctx {
                    theme: &theme,
                    config: &config,
                    store: &app.email_store,
                };
                folders.render(f, chunks[0], is_folders_active, &ctx);
                Self::draw_messages_pane(f, app, messages, chunks[1], is_messages_active);
            }
            View::MessagesContent => {
                // Two panes: messages and content
                let chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .split(area);

                let is_messages_active = matches!(app.active_pane, ActivePane::Messages);
                let is_content_active = matches!(app.active_pane, ActivePane::Content);

                Self::draw_messages_pane(f, app, messages, chunks[0], is_messages_active);
                self.draw_content_pane(f, app, chunks[1], is_content_active);
            }
            View::Content => {
                // Single pane: content only
                let is_content_active = matches!(app.active_pane, ActivePane::Content);
                self.draw_content_pane(f, app, area, is_content_active);
            }
            View::Messages => {
                // Single pane: messages only (when content hidden)
                let is_messages_active = matches!(app.active_pane, ActivePane::Messages);
                Self::draw_messages_pane(f, app, messages, area, is_messages_active);
            }
            View::MessagesAttachments => {
                // Two panes: messages and attachments
                let chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .split(area);

                let is_messages_active = matches!(app.active_pane, ActivePane::Messages);
                let is_attachments_active = matches!(app.active_pane, ActivePane::Attachments);

                Self::draw_messages_pane(f, app, messages, chunks[0], is_messages_active);
                self.draw_attachments_pane(f, app, chunks[1], is_attachments_active);
            }
        }

        // Draw status bar
        self.draw_status_bar(f, app, area);
    }

    /// Delegate Messages-pane drawing to `MessagesComponent`. Stays a
    /// `UI`-side helper so the view-aware folder pick (`FolderMessages`
    /// shows the *selected* folder; every other view shows the current
    /// one) lives next to the rest of the layout decisions in `ui.rs`.
    fn draw_messages_pane(
        f: &mut Frame,
        app: &mut App,
        messages: &MessagesComponent,
        area: Rect,
        is_active: bool,
    ) {
        let folder_to_display = match app.current_view {
            View::FolderMessages => app
                .get_selected_folder()
                .unwrap_or_else(|| app.email_store.get_current_folder()),
            _ => app.email_store.get_current_folder(),
        };

        let folder_path = match app.current_view {
            View::FolderMessages => {
                let root_folder = &app.email_store.root_folder;
                if let Some(path_indices) = crate::input::get_folder_path_from_display_index(
                    root_folder,
                    app.selection.folder_index,
                ) {
                    app.email_store.get_folder_path_for_indices(&path_indices)
                } else {
                    app.email_store.get_folder_path()
                }
            }
            _ => app.email_store.get_folder_path(),
        };

        messages.render_with_folder(f, area, is_active, folder_to_display, &folder_path);
    }

    fn draw_content_pane(&mut self, f: &mut Frame, app: &mut App, area: Rect, is_active: bool) {
        let border_style = if is_active {
            Style::default().fg(VulthorTheme::CYAN_LIGHT)
        } else {
            Style::default()
        };

        // Get email info first for headers (no markdown conversion needed)
        let email_info = app.email_store.get_selected_email_headers();

        if let Some(email) = email_info {
            // Split content pane into header and body
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(6), Constraint::Min(0)])
                .split(area);

            // Draw headers
            let header_block = Block::default()
                .borders(Borders::ALL)
                .style(border_style)
                .title("Headers");

            let header_text = email.get_header_display();
            let header_paragraph = Paragraph::new(header_text.as_str())
                .block(header_block)
                .wrap(Wrap { trim: true });

            f.render_widget(header_paragraph, chunks[0]);

            // Draw body - only convert to markdown when content pane is visible
            let mut body_title = "Content".to_string();
            if email.has_attachments() {
                body_title = format!("Content ({} attachments)", email.attachment_count());
            }

            let body_block = Block::default()
                .borders(Borders::ALL)
                .style(border_style)
                .title(body_title);

            // Non-blocking read: the body loader (`BodyLoader` in `components::body_loader`)
            // parses bodies off-thread. Until it lands a result, `body_text` is empty and
            // `load_state` is `HeadersOnly` — show a placeholder so the user knows
            // selection succeeded.
            let body_text = match email.load_state {
                EmailLoadState::HeadersOnly => "Loading body…".to_string(),
                EmailLoadState::FullyLoaded => app
                    .email_store
                    .get_selected_email_markdown()
                    .unwrap_or_default(),
            };

            let body_paragraph = Paragraph::new(body_text.as_str())
                .block(body_block)
                .wrap(Wrap { trim: true })
                .scroll((app.selection.scroll_offset as u16, 0));

            f.render_widget(body_paragraph, chunks[1]);

            // Draw scrollbar if content is scrollable
            if is_active {
                let scrollbar = Scrollbar::default()
                    .orientation(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(Some("↑"))
                    .end_symbol(Some("↓"));

                let mut scrollbar_state = ScrollbarState::default()
                    .content_length(body_text.lines().count())
                    .position(app.selection.scroll_offset);

                f.render_stateful_widget(
                    scrollbar,
                    chunks[1].inner(Margin {
                        vertical: 1,
                        horizontal: 1,
                    }),
                    &mut scrollbar_state,
                );
            }
        } else {
            // No email selected
            let block = Block::default()
                .borders(Borders::ALL)
                .style(border_style)
                .title("Content");

            let current_folder = app.email_store.get_current_folder();
            let text = if current_folder.emails.is_empty() {
                "No emails in this folder"
            } else {
                "Select an email to view its content"
            };

            let paragraph = Paragraph::new(text)
                .block(block)
                .style(Style::default().fg(VulthorTheme::GRAY_DARK));

            f.render_widget(paragraph, area);
        }
    }

    fn draw_attachments_pane(&mut self, f: &mut Frame, app: &mut App, area: Rect, is_active: bool) {
        let border_style = if is_active {
            Style::default().fg(VulthorTheme::ACCENT_LIGHT)
        } else {
            Style::default()
        };

        if let Some(email) = app.email_store.get_selected_email() {
            if !email.attachments.is_empty() {
                // Create attachment list
                let attachment_items: Vec<ListItem> = email
                    .attachments
                    .iter()
                    .enumerate()
                    .map(|(i, attachment)| {
                        let size_str = if attachment.size < 1024 {
                            format!("{} B", attachment.size)
                        } else if attachment.size < 1024 * 1024 {
                            format!("{:.1} KB", attachment.size as f64 / 1024.0)
                        } else {
                            format!("{:.1} MB", attachment.size as f64 / (1024.0 * 1024.0))
                        };

                        let content = format!(
                            "{:2}. {} ({}) - {}",
                            i + 1,
                            attachment.filename,
                            attachment.content_type,
                            size_str
                        );

                        ListItem::new(content)
                    })
                    .collect();

                let block = Block::default()
                    .borders(Borders::ALL)
                    .style(border_style)
                    .title("Attachments");

                let list = List::new(attachment_items).block(block).highlight_style(
                    Style::default()
                        .bg(VulthorTheme::SELECTION_BG)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                );

                // Update selection state
                self.attachment_list_state
                    .select(Some(app.selection.attachment_index));

                f.render_stateful_widget(list, area, &mut self.attachment_list_state);
            } else {
                // `email.attachments` is populated as a side effect of full-body parse.
                // Distinguish "still loading" from "no attachments" so the user doesn't
                // see a false negative for a multipart message in flight.
                let block = Block::default()
                    .borders(Borders::ALL)
                    .style(border_style)
                    .title("Attachments");

                let text = match email.load_state {
                    EmailLoadState::HeadersOnly => "Loading attachments…",
                    EmailLoadState::FullyLoaded => "No attachments in this email",
                };
                let paragraph = Paragraph::new(text)
                    .block(block)
                    .style(Style::default().fg(VulthorTheme::GRAY_DARK));

                f.render_widget(paragraph, area);
            }
        } else {
            // No email selected
            let block = Block::default()
                .borders(Borders::ALL)
                .style(border_style)
                .title("Attachments");

            let paragraph = Paragraph::new("Select an email to view attachments")
                .block(block)
                .style(Style::default().fg(VulthorTheme::GRAY_DARK));

            f.render_widget(paragraph, area);
        }
    }

    fn draw_status_bar(&mut self, f: &mut Frame, app: &App, area: Rect) {
        let status_area = Rect {
            x: area.x,
            y: area.bottom() - 1,
            width: area.width,
            height: 1,
        };

        let mut status_text = vec![];

        // Add key bindings help
        let help_text = {
            let base_help = "j/k: Navigate | Tab: Switch Pane | h/l: Switch View";
            let content_toggle = if app.content_pane_hidden {
                " | M-c: Show Content"
            } else {
                " | M-c: Hide Content"
            };
            format!("{}{} | q: Quit", base_help, content_toggle)
        };

        status_text.push(Span::styled(
            help_text,
            Style::default().fg(VulthorTheme::GRAY_DARK),
        ));

        // Add status message if present
        if let Some(ref message) = app.status_message {
            status_text.push(Span::raw(" | "));
            status_text.push(Span::styled(
                message,
                Style::default().fg(VulthorTheme::WARNING),
            ));
        }

        let status_line = Line::from(status_text);
        let status_paragraph = Paragraph::new(status_line).style(
            Style::default()
                .bg(VulthorTheme::STATUS_BG)
                .fg(Color::White),
        );

        f.render_widget(status_paragraph, status_area);
    }

    fn draw_help_screen(&mut self, f: &mut Frame, area: Rect) {
        let help_text = vec![
            Line::from("Vulthor - TUI Email Client"),
            Line::from(""),
            Line::from("Navigation:"),
            Line::from("  j/k        - Move up/down in current pane"),
            Line::from("  Tab        - Switch between panes"),
            Line::from("  Enter      - Select folder or email"),
            Line::from("  Backspace  - Go back to parent folder"),
            Line::from(""),
            Line::from("View Control:"),
            Line::from("  h          - Switch to folder/message view"),
            Line::from("  l          - Switch to message/content view"),
            Line::from(""),
            Line::from("Other:"),
            Line::from("  ?          - Show this help"),
            Line::from("  q          - Quit application"),
            Line::from(""),
            Line::from("Press any key to return..."),
        ];

        let block = Block::default()
            .borders(Borders::ALL)
            .style(Style::default().fg(VulthorTheme::CYAN))
            .title("Help");

        let paragraph = Paragraph::new(help_text)
            .block(block)
            .wrap(Wrap { trim: true });

        f.render_widget(paragraph, area);
    }
}
