use crate::app::{ActivePane, App, AppState, View};
use crate::components::{Component, ContentComponent, Ctx, FoldersComponent, MessagesComponent};
use crate::config::Config;
use crate::theme::VulthorTheme;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

/// Render orchestrator. Post-Phase-0.2.3 it owns no widget state —
/// the per-pane `Component`s carry their own `RefCell<ListState>`s
/// (see `FoldersComponent`, `MessagesComponent`). `UI` exists for
/// layout-rect computation, status-bar rendering, and the help
/// overlay until those move into components in later phases.
#[derive(Default)]
pub struct UI;

impl UI {
    pub fn new() -> Self {
        Self
    }

    pub fn draw(
        &mut self,
        f: &mut Frame,
        app: &mut App,
        folders: &FoldersComponent,
        messages: &MessagesComponent,
        content: &ContentComponent,
    ) {
        let size = f.area();

        match app.state {
            AppState::Help => {
                self.draw_help_screen(f, size);
            }
            _ => {
                self.draw_main_layout(f, app, folders, messages, content, size);
            }
        }
    }

    fn draw_main_layout(
        &mut self,
        f: &mut Frame,
        app: &mut App,
        folders: &FoldersComponent,
        messages: &MessagesComponent,
        content: &ContentComponent,
        area: Rect,
    ) {
        // Compute the layout split for the current view so we can
        // calculate the messages pane's visible rows, then perform
        // initial-load (which mutates app) BEFORE constructing `Ctx`
        // (which borrows app.email_store immutably).
        let view = app.current_view.clone();
        let active_pane = app.active_pane.clone();
        let chunks: Vec<Rect> = match view {
            View::FolderMessages | View::MessagesContent | View::MessagesAttachments => {
                Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .split(area)
                    .to_vec()
            }
            View::Content | View::Messages => vec![area],
        };

        let messages_area = match view {
            View::FolderMessages => Some(chunks[1]),
            View::MessagesContent => Some(chunks[0]),
            View::Messages => Some(chunks[0]),
            View::MessagesAttachments => Some(chunks[0]),
            View::Content => None,
        };

        if let Some(area) = messages_area {
            let visible_rows = area.height.saturating_sub(2) as usize;
            app.message_pane_visible_rows = visible_rows;
            app.perform_initial_loading_if_needed();
        }

        // App mutations done — build Ctx and dispatch component renders.
        let theme = VulthorTheme;
        let config = Config::default();
        let folder_index = folders.folder_index;
        let ctx = Ctx {
            theme: &theme,
            config: &config,
            store: &app.email_store,
            view: view.clone(),
            folder_index,
        };

        match view {
            View::FolderMessages => {
                let is_folders_active = matches!(active_pane, ActivePane::Folders);
                let is_messages_active = matches!(active_pane, ActivePane::Messages);
                folders.render(f, chunks[0], is_folders_active, &ctx);
                messages.render(f, chunks[1], is_messages_active, &ctx);
            }
            View::MessagesContent => {
                let is_messages_active = matches!(active_pane, ActivePane::Messages);
                let is_content_active = matches!(active_pane, ActivePane::Content);
                messages.render(f, chunks[0], is_messages_active, &ctx);
                content.render(f, chunks[1], is_content_active, &ctx);
            }
            View::Content => {
                let is_content_active = matches!(active_pane, ActivePane::Content);
                content.render(f, chunks[0], is_content_active, &ctx);
            }
            View::Messages => {
                let is_messages_active = matches!(active_pane, ActivePane::Messages);
                messages.render(f, chunks[0], is_messages_active, &ctx);
            }
            View::MessagesAttachments => {
                let is_messages_active = matches!(active_pane, ActivePane::Messages);
                let is_attachments_active = matches!(active_pane, ActivePane::Attachments);
                messages.render(f, chunks[0], is_messages_active, &ctx);
                messages.render_attachments(f, chunks[1], is_attachments_active, &ctx);
            }
        }

        self.draw_status_bar(f, app, area);
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
