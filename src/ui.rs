use crate::app::{ActivePane, App, AppState, View};
use crate::email::{Email, Folder};
use crate::theme::VulthorTheme;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Clear, List, ListItem, ListState, Paragraph, Scrollbar,
        ScrollbarOrientation, ScrollbarState, Wrap,
    },
};

pub struct UI {
    folder_list_state: ListState,
    email_list_state: ListState,
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
            folder_list_state: ListState::default(),
            email_list_state: ListState::default(),
            attachment_list_state: ListState::default(),
        }
    }

    pub fn draw(&mut self, f: &mut Frame, app: &mut App) {
        let size = f.area();

        match app.state {
            AppState::AttachmentView => {
                self.draw_main_layout(f, app, size);
                self.draw_attachment_popup(f, app, size);
            }
            AppState::Help => {
                self.draw_help_screen(f, size);
            }
            _ => {
                self.draw_main_layout(f, app, size);
            }
        }
    }

    fn draw_main_layout(&mut self, f: &mut Frame, app: &mut App, area: Rect) {
        match app.current_view {
            View::FolderMessages => {
                // Two panes: folders and messages
                let chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .split(area);

                let is_folders_active = matches!(app.active_pane, ActivePane::Folders);
                let is_list_active = matches!(app.active_pane, ActivePane::List);

                self.draw_folder_pane(f, app, chunks[0], is_folders_active);
                self.draw_email_list_pane(f, app, chunks[1], is_list_active);
            }
            View::MessagesContent => {
                // Two panes: messages and content
                let chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .split(area);

                let is_list_active = matches!(app.active_pane, ActivePane::List);
                let is_content_active = matches!(app.active_pane, ActivePane::Content);

                self.draw_email_list_pane(f, app, chunks[0], is_list_active);
                self.draw_content_pane(f, app, chunks[1], is_content_active);
            }
            View::Content => {
                // Single pane: content only
                let is_content_active = matches!(app.active_pane, ActivePane::Content);
                self.draw_content_pane(f, app, area, is_content_active);
            }
            View::Messages => {
                // Single pane: messages only (when content hidden)
                let is_list_active = matches!(app.active_pane, ActivePane::List);
                self.draw_email_list_pane(f, app, area, is_list_active);
            }
            View::MessagesAttachments => {
                // Two panes: messages and attachments
                let chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .split(area);

                let is_list_active = matches!(app.active_pane, ActivePane::List);
                let is_attachments_active = matches!(app.active_pane, ActivePane::Attachments);

                self.draw_email_list_pane(f, app, chunks[0], is_list_active);
                self.draw_attachments_pane(f, app, chunks[1], is_attachments_active);
            }
        }

        // Draw status bar
        self.draw_status_bar(f, app, area);
    }

    fn draw_folder_pane(&mut self, f: &mut Frame, app: &App, area: Rect, is_active: bool) {
        // Always show the root folder structure, not the current folder's subfolders
        let root_folder = &app.email_store.root_folder;
        let folder_items = Self::build_folder_list_static(root_folder, 0);

        let style = if is_active {
            Style::default().fg(VulthorTheme::ACCENT)
        } else {
            Style::default()
        };

        let border_style = if is_active {
            Style::default().fg(VulthorTheme::ACCENT)
        } else {
            Style::default()
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .style(border_style)
            .title("Folders");

        let list = List::new(folder_items)
            .block(block)
            .style(style)
            .highlight_style(
                Style::default()
                    .bg(VulthorTheme::SELECTION_BG)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            );

        // Update selection state
        self.folder_list_state
            .select(Some(app.selection.folder_index));

        f.render_stateful_widget(list, area, &mut self.folder_list_state);
    }

    fn draw_email_list_pane(&mut self, f: &mut Frame, app: &mut App, area: Rect, is_active: bool) {
        // Calculate visible rows for dynamic loading (area height minus borders)
        let visible_rows = (area.height.saturating_sub(2)) as usize; // -2 for top and bottom borders

        // Update app's visible rows for use in navigation
        app.message_pane_visible_rows = visible_rows;

        // Perform initial loading if this is the first render with proper dimensions
        app.perform_initial_loading_if_needed();

        // In folder/message view mode, show messages from the selected folder
        // In other view modes, show messages from the current folder
        let folder_to_display = match app.current_view {
            View::FolderMessages => {
                // Show messages from the selected folder in the folder pane
                app.get_selected_folder()
                    .unwrap_or_else(|| app.email_store.get_current_folder())
            }
            _ => {
                // Show messages from the current folder we've navigated into
                app.email_store.get_current_folder()
            }
        };

        let email_items = Self::build_email_list_static(&folder_to_display.emails);

        let style = if is_active {
            Style::default().fg(VulthorTheme::CYAN)
        } else {
            Style::default()
        };

        let border_style = if is_active {
            Style::default().fg(VulthorTheme::CYAN)
        } else {
            Style::default()
        };

        let folder_path = match app.current_view {
            View::FolderMessages => {
                // Show path of the selected folder
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
            _ => {
                // Show path of the current folder
                app.email_store.get_folder_path()
            }
        };

        let title = if folder_to_display.is_loaded {
            format!(
                "Emails - {} ({})",
                folder_path,
                folder_to_display.emails.len()
            )
        } else {
            format!(
                "Emails - {} ({}/...)",
                folder_path,
                folder_to_display.emails.len()
            )
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .style(border_style)
            .title(title);

        let list = List::new(email_items)
            .block(block)
            .style(style)
            .highlight_style(
                Style::default()
                    .bg(VulthorTheme::SELECTION_BG)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            );

        // Update selection state
        self.email_list_state
            .select(Some(app.selection.email_index));

        f.render_stateful_widget(list, area, &mut self.email_list_state);
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

            // Get markdown content lazily only when content pane is being drawn
            let body_text = app
                .email_store
                .get_selected_email_markdown()
                .unwrap_or_else(|| "Error loading email content".to_string());

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
                // No attachments
                let block = Block::default()
                    .borders(Borders::ALL)
                    .style(border_style)
                    .title("Attachments");

                let paragraph = Paragraph::new("No attachments in this email")
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

    fn draw_attachment_popup(&mut self, f: &mut Frame, app: &mut App, area: Rect) {
        if let Some(email) = app.email_store.get_selected_email() {
            if !email.attachments.is_empty() {
                // Calculate popup size
                let popup_area = self.centered_rect(60, 70, area);

                // Clear the area
                f.render_widget(Clear, popup_area);

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
                    .style(Style::default().fg(VulthorTheme::ACCENT_LIGHT))
                    .title("Attachments - Enter: Open, Shift+Enter: Custom Command, Esc: Close");

                let list = List::new(attachment_items).block(block).highlight_style(
                    Style::default()
                        .bg(VulthorTheme::SELECTION_BG)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                );

                // Update selection state
                self.attachment_list_state
                    .select(Some(app.selection.attachment_index));

                f.render_stateful_widget(list, popup_area, &mut self.attachment_list_state);
            }
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
        let help_text = match app.state {
            AppState::AttachmentView => {
                "Enter: Open | Shift+Enter: Custom | Esc: Close".to_string()
            }
            _ => {
                let base_help = "j/k: Navigate | Tab: Switch Pane | h/l: Switch View";
                let content_toggle = if app.content_pane_hidden {
                    ""
                } else {
                    " | M-c: Hide Content"
                };
                format!(
                    "{}{} | M-a: Attachments | q: Quit",
                    base_help, content_toggle
                )
            }
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
            Line::from("Email Actions:"),
            Line::from("  M-a        - View attachments"),
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

        let help_area = self.centered_rect(60, 80, area);
        f.render_widget(Clear, help_area);
        f.render_widget(paragraph, help_area);
    }

    fn build_folder_list_static(folder: &Folder, depth: usize) -> Vec<ListItem> {
        let mut items = Vec::new();

        // Add current folder if not root
        if depth > 0 {
            let indent = "  ".repeat(depth - 1);
            let display_name = folder.get_display_name();
            let content = format!("{}{}", indent, display_name);
            items.push(ListItem::new(content));
        }

        // Add subfolders in sorted order
        for subfolder in folder.get_sorted_subfolders() {
            items.extend(Self::build_folder_list_static(subfolder, depth + 1));
        }

        items
    }

    fn build_email_list_static(emails: &[Email]) -> Vec<ListItem> {
        emails
            .iter()
            .enumerate()
            .map(|(i, email)| {
                let mut style = Style::default();
                if email.is_unread {
                    style = style.add_modifier(Modifier::BOLD);
                }

                let attachment_indicator = if email.has_attachments() { "📎 " } else { "" };
                let subject = if email.headers.subject.is_empty() {
                    "(No Subject)"
                } else {
                    &email.headers.subject
                };

                let content = format!(
                    "{:3}. {}{} - {}",
                    i + 1,
                    attachment_indicator,
                    subject,
                    email.headers.from
                );

                ListItem::new(content).style(style)
            })
            .collect()
    }

    /// Helper function to create a centered rect
    fn centered_rect(&self, percent_x: u16, percent_y: u16, r: Rect) -> Rect {
        let popup_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage((100 - percent_y) / 2),
                Constraint::Percentage(percent_y),
                Constraint::Percentage((100 - percent_y) / 2),
            ])
            .split(r);

        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage((100 - percent_x) / 2),
                Constraint::Percentage(percent_x),
                Constraint::Percentage((100 - percent_x) / 2),
            ])
            .split(popup_layout[1])[1]
    }
}
