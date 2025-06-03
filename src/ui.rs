use crate::app::{ActivePane, App, AppState, View};
use crate::email::{Email, Folder};
use crate::theme::VulthorTheme;
use chrono::{DateTime, Local};
use unicode_width::UnicodeWidthStr;
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
                let is_messages_active = matches!(app.active_pane, ActivePane::Messages);

                self.draw_folder_pane(f, app, chunks[0], is_folders_active);
                self.draw_messages_pane(f, app, chunks[1], is_messages_active);
            }
            View::MessagesContent => {
                // Two panes: messages and content
                let chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .split(area);

                let is_messages_active = matches!(app.active_pane, ActivePane::Messages);
                let is_content_active = matches!(app.active_pane, ActivePane::Content);

                self.draw_messages_pane(f, app, chunks[0], is_messages_active);
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
                self.draw_messages_pane(f, app, area, is_messages_active);
            }
            View::MessagesAttachments => {
                // Two panes: messages and attachments
                let chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .split(area);

                let is_messages_active = matches!(app.active_pane, ActivePane::Messages);
                let is_attachments_active = matches!(app.active_pane, ActivePane::Attachments);

                self.draw_messages_pane(f, app, chunks[0], is_messages_active);
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

    fn draw_messages_pane(&mut self, f: &mut Frame, app: &mut App, area: Rect, is_active: bool) {
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

        // Check if current folder is Sent to show To field instead of From
        let is_sent_folder = folder_to_display.name == "Sent" || 
                            folder_to_display.name.to_lowercase().contains("sent");

        let email_items = Self::build_email_list_with_truncation(
            &folder_to_display.emails, 
            area.width.saturating_sub(2) as usize, // -2 for borders
            is_sent_folder
        );

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

    fn format_email_date(date_str: &str) -> String {
        // Try to parse the RFC3339 date
        if let Ok(date_time) = DateTime::parse_from_rfc3339(date_str) {
            let local_time = date_time.with_timezone(&Local);
            let today = Local::now().date_naive();
            
            if local_time.date_naive() == today {
                // Today: show time in 24-hour format
                local_time.format("%H:%M").to_string()
            } else {
                // Other days: show date
                local_time.format("%Y-%m-%d").to_string()
            }
        } else {
            // Fallback: try to show something meaningful from the original string
            date_str.chars().take(10).collect()
        }
    }

    fn truncate_with_ellipsis(text: &str, max_width: usize) -> String {
        let text_width = text.width();
        
        if text_width <= max_width {
            text.to_string()
        } else if max_width > 3 {
            // We need to find the right truncation point by iterating through grapheme clusters
            let mut current_width = 0;
            let mut truncation_point = 0;
            
            for (idx, ch) in text.char_indices() {
                let ch_str = &text[idx..idx + ch.len_utf8()];
                let ch_width = ch_str.width();
                
                if current_width + ch_width > max_width - 3 {
                    break;
                }
                
                current_width += ch_width;
                truncation_point = idx + ch.len_utf8();
            }
            
            format!("{}...", &text[..truncation_point])
        } else {
            // For very small widths, just take what we can
            let mut current_width = 0;
            let mut truncation_point = 0;
            
            for (idx, ch) in text.char_indices() {
                let ch_str = &text[idx..idx + ch.len_utf8()];
                let ch_width = ch_str.width();
                
                if current_width + ch_width > max_width {
                    break;
                }
                
                current_width += ch_width;
                truncation_point = idx + ch.len_utf8();
            }
            
            text[..truncation_point].to_string()
        }
    }

    fn pad_to_width(text: &str, target_width: usize) -> String {
        let text_width = text.width();
        if text_width >= target_width {
            text.to_string()
        } else {
            let padding = target_width - text_width;
            format!("{}{}", text, " ".repeat(padding))
        }
    }

    fn extract_email_address(from_field: &str) -> String {
        // Extract just the name or email part for better display
        if let Some(name_end) = from_field.find(" <") {
            // Has format "Name <email@example.com>"
            from_field[..name_end].to_string()
        } else if from_field.contains('@') {
            // Just an email address, extract the part before @
            if let Some(at_pos) = from_field.find('@') {
                from_field[..at_pos].to_string()
            } else {
                from_field.to_string()
            }
        } else {
            from_field.to_string()
        }
    }

    fn build_email_list_with_truncation(emails: &[Email], available_width: usize, is_sent_folder: bool) -> Vec<ListItem> {
        // Column widths:
        // - Unread indicator: 2 chars (• + space)
        // - From/To: 20 chars min, 30% of available space max
        // - Subject: flexible (remaining space)
        // - Attachment icon: 3 chars (📎 takes 2 + space) - always reserved for alignment
        // - Date: 10 chars (YYYY-MM-DD or HH:MM)
        
        const UNREAD_WIDTH: usize = 2;
        const DATE_WIDTH: usize = 10;
        const ATTACHMENT_WIDTH: usize = 3; // emoji takes 2 columns + 1 space
        const SEPARATORS: usize = 8; // spaces between columns
        
        let min_from_width = 15;
        let max_from_width = (available_width * 30) / 100; // 30% max
        let from_width = min_from_width.max(max_from_width).min(25); // Cap at 25 chars
        
        emails
            .iter()
            .enumerate()
            .map(|(_i, email)| {
                let mut style = Style::default();
                if email.is_unread {
                    style = style.add_modifier(Modifier::BOLD);
                }

                // Calculate subject width - always account for attachment column
                let subject_width = available_width
                    .saturating_sub(UNREAD_WIDTH)
                    .saturating_sub(from_width)
                    .saturating_sub(DATE_WIDTH)
                    .saturating_sub(ATTACHMENT_WIDTH)
                    .saturating_sub(SEPARATORS);

                let mut spans = vec![];
                
                // Column 1: Unread indicator
                spans.push(Span::styled(
                    if email.is_unread { "•" } else { " " },
                    style,
                ));
                spans.push(Span::raw(" "));
                
                // Column 2: From/To field
                let sender = if is_sent_folder {
                    Self::extract_email_address(&email.headers.to)
                } else {
                    Self::extract_email_address(&email.headers.from)
                };
                let truncated_sender = Self::truncate_with_ellipsis(&sender, from_width);
                // Pad to ensure consistent column width using visual width
                let padded_sender = Self::pad_to_width(&truncated_sender, from_width);
                spans.push(Span::styled(padded_sender, style));
                spans.push(Span::raw("  "));
                
                // Column 3: Subject
                let subject = if email.headers.subject.is_empty() {
                    "(No Subject)"
                } else {
                    &email.headers.subject
                };
                let truncated_subject = Self::truncate_with_ellipsis(subject, subject_width);
                let padded_subject = Self::pad_to_width(&truncated_subject, subject_width);
                spans.push(Span::styled(padded_subject, style));
                spans.push(Span::raw("  "));
                
                // Column 4: Status icons (always present for alignment)
                spans.push(Span::styled(
                    if email.has_attachments() { "📎" } else { "  " }, // Two spaces to match emoji width
                    style,
                ));
                spans.push(Span::raw(" "));
                
                // Column 5: Date
                let date_str = Self::format_email_date(&email.headers.date);
                spans.push(Span::styled(date_str, style));

                ListItem::new(Line::from(spans))
            })
            .collect()
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::email::{Email, EmailHeaders};
    use std::path::PathBuf;

    #[test]
    fn test_format_email_date_today() {
        // Get current time and format as RFC3339
        let now = Local::now();
        let date_str = now.to_rfc3339();
        
        let formatted = UI::format_email_date(&date_str);
        
        // Should show time in HH:MM format
        assert_eq!(formatted.len(), 5);
        assert!(formatted.contains(':'));
    }

    #[test]
    fn test_format_email_date_past() {
        // Create a date from 2024
        let date_str = "2024-01-15T10:30:00+00:00";
        
        let formatted = UI::format_email_date(&date_str);
        
        // Should show date in YYYY-MM-DD format
        assert_eq!(formatted, "2024-01-15");
    }

    #[test]
    fn test_format_email_date_invalid() {
        let date_str = "invalid date";
        
        let formatted = UI::format_email_date(&date_str);
        
        // Should return first 10 chars
        assert_eq!(formatted, "invalid da");
    }

    #[test]
    fn test_truncate_with_ellipsis() {
        // Test no truncation needed
        let text = "Short text";
        assert_eq!(UI::truncate_with_ellipsis(text, 20), "Short text");
        
        // Test truncation
        let text = "This is a very long subject line that needs truncation";
        assert_eq!(UI::truncate_with_ellipsis(text, 20), "This is a very lo...");
        
        // Test edge case
        assert_eq!(UI::truncate_with_ellipsis(text, 3), "Thi");
        
        // Test with emoji
        let text_emoji = "Hello 🌍 World 🚀 Test";
        // Each emoji takes 2 visual columns
        assert_eq!(UI::truncate_with_ellipsis(text_emoji, 15), "Hello 🌍 Wor...");
        
        // Test emoji at truncation boundary
        let text_emoji2 = "Test 🎉🎊🎈";
        assert_eq!(UI::truncate_with_ellipsis(text_emoji2, 8), "Test ...");
    }

    #[test]
    fn test_pad_to_width() {
        // Test regular text
        let text = "Hello";
        assert_eq!(UI::pad_to_width(text, 10), "Hello     ");
        assert_eq!(UI::pad_to_width(text, 10).width(), 10);
        
        // Test text with emoji
        let text_emoji = "Hi 🌍";
        // "Hi " = 3, emoji = 2, total = 5
        assert_eq!(UI::pad_to_width(text_emoji, 10).width(), 10);
        
        // Test when text is already wider
        assert_eq!(UI::pad_to_width(text_emoji, 3), text_emoji);
    }

    #[test]
    fn test_extract_email_address() {
        // Test name with email format
        let from = "John Doe <john@example.com>";
        assert_eq!(UI::extract_email_address(from), "John Doe");
        
        // Test plain email
        let from = "jane@example.com";
        assert_eq!(UI::extract_email_address(from), "jane");
        
        // Test just name
        let from = "Bob Smith";
        assert_eq!(UI::extract_email_address(from), "Bob Smith");
    }

    #[test]
    fn test_build_email_list_with_truncation() {
        let mut email = Email::new(PathBuf::from("/test/email"));
        email.headers = EmailHeaders {
            from: "Alice Johnson <alice@example.com>".to_string(),
            to: "Bob Smith <bob@example.com>".to_string(),
            subject: "This is a very long subject line that will need to be truncated for display".to_string(),
            date: Local::now().to_rfc3339(),
            message_id: "123".to_string(),
        };
        email.is_unread = true;
        
        let emails = vec![email];
        
        // Test with various widths
        let items = UI::build_email_list_with_truncation(&emails, 80, false);
        assert_eq!(items.len(), 1);
        
        // Test Sent folder (should use To field)
        let items_sent = UI::build_email_list_with_truncation(&emails, 80, true);
        assert_eq!(items_sent.len(), 1);
    }

    #[test]
    fn test_build_email_list_with_emoji() {
        let mut email = Email::new(PathBuf::from("/test/email"));
        email.headers = EmailHeaders {
            from: "Alice 🦄 <alice@example.com>".to_string(),
            to: "Bob 🚀 <bob@example.com>".to_string(),
            subject: "Meeting tomorrow 📅 Important! 🔥🔥🔥".to_string(),
            date: Local::now().to_rfc3339(),
            message_id: "456".to_string(),
        };
        email.is_unread = false;
        
        let emails = vec![email];
        
        // Test that it handles emoji without crashing
        let items = UI::build_email_list_with_truncation(&emails, 60, false);
        assert_eq!(items.len(), 1);
        
        // The item should be properly formatted despite emojis
        // This test mainly ensures no panic occurs
    }
}
