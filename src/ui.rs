use crate::components::{
    AccountsComponent, Component, ContentComponent, Ctx, DraftComponent, FoldersComponent,
    MessagesComponent, ModalComponent,
};
use crate::config::Config;
use crate::email::{EmailLoadState, EmailStore};
use crate::layout::{self, ActivePane, Layout, View};
use crate::theme::VulthorTheme;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout as RLayout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
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

    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        f: &mut Frame,
        store: &mut EmailStore,
        layout: &Layout,
        status_message: &Option<String>,
        help_visible: bool,
        folders: &FoldersComponent,
        messages: &MessagesComponent,
        content: &ContentComponent,
        accounts: &AccountsComponent,
        draft: &DraftComponent,
        modal: &ModalComponent,
    ) {
        let size = f.area();
        if help_visible {
            self.draw_help_screen(f, size);
            return;
        }
        self.draw_main_layout(
            f, store, layout, folders, messages, content, accounts, draft, size,
        );
        self.draw_status_bar(f, layout, status_message, size);
        // Modal overlay draws last so it sits above panes and status bar.
        // Phase 1.d (vu-3e0). No-op when no modal is open.
        modal.render_overlay(f, size);
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_main_layout(
        &mut self,
        f: &mut Frame,
        store: &mut EmailStore,
        lay: &Layout,
        folders: &FoldersComponent,
        messages: &MessagesComponent,
        content: &ContentComponent,
        accounts: &AccountsComponent,
        draft: &DraftComponent,
        area: Rect,
    ) {
        match lay.current_view {
            View::FolderMessages => {
                let chunks = RLayout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .split(area);

                let is_folders_active = matches!(lay.active_pane, ActivePane::Folders);
                let is_messages_active = matches!(lay.active_pane, ActivePane::Messages);

                let theme = VulthorTheme;
                let config = Config::default();
                let ctx = Ctx {
                    theme: &theme,
                    config: &config,
                    store,
                };
                folders.render(f, chunks[0], is_folders_active, &ctx);
                Self::draw_messages_pane(f, store, lay, messages, chunks[1], is_messages_active);
            }
            View::MessagesContent => {
                let chunks = RLayout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .split(area);

                let is_messages_active = matches!(lay.active_pane, ActivePane::Messages);
                let is_content_active = matches!(lay.active_pane, ActivePane::Content);

                Self::draw_messages_pane(f, store, lay, messages, chunks[0], is_messages_active);
                Self::render_content_pane(f, store, content, chunks[1], is_content_active);
            }
            View::Content => {
                let is_content_active = matches!(lay.active_pane, ActivePane::Content);
                Self::render_content_pane(f, store, content, area, is_content_active);
            }
            View::Messages => {
                let is_messages_active = matches!(lay.active_pane, ActivePane::Messages);
                Self::draw_messages_pane(f, store, lay, messages, area, is_messages_active);
            }
            View::MessagesAttachments => {
                let chunks = RLayout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .split(area);

                let is_messages_active = matches!(lay.active_pane, ActivePane::Messages);
                let is_attachments_active = matches!(lay.active_pane, ActivePane::Attachments);

                Self::draw_messages_pane(f, store, lay, messages, chunks[0], is_messages_active);
                self.draw_attachments_pane(f, store, lay, chunks[1], is_attachments_active);
            }
            View::AccountsFolders => {
                let chunks = RLayout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .split(area);

                let is_accounts_active = matches!(lay.active_pane, ActivePane::Accounts);
                let is_folders_active = matches!(lay.active_pane, ActivePane::Folders);

                let theme = VulthorTheme;
                let config = Config::default();
                let ctx = Ctx {
                    theme: &theme,
                    config: &config,
                    store,
                };
                accounts.render(f, chunks[0], is_accounts_active, &ctx);
                folders.render(f, chunks[1], is_folders_active, &ctx);
            }
            View::ContentDraft => {
                let chunks = RLayout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .split(area);

                let is_content_active = matches!(lay.active_pane, ActivePane::Content);
                let is_draft_active = matches!(lay.active_pane, ActivePane::Draft);

                Self::render_content_pane(f, store, content, chunks[0], is_content_active);
                let theme = VulthorTheme;
                let config = Config::default();
                let ctx = Ctx {
                    theme: &theme,
                    config: &config,
                    store,
                };
                draft.render(f, chunks[1], is_draft_active, &ctx);
            }
        }
    }

    fn draw_messages_pane(
        f: &mut Frame,
        store: &EmailStore,
        lay: &Layout,
        messages: &MessagesComponent,
        area: Rect,
        is_active: bool,
    ) {
        let selected_folder = match lay.current_view {
            View::FolderMessages => {
                let root = &store.root_folder;
                let folder_path =
                    layout::get_folder_path_from_display_index(root, lay.selection.folder_index);
                folder_path.and_then(|p| store.get_folder_at_path(&p))
            }
            _ => None,
        };
        let folder_to_display = selected_folder.unwrap_or_else(|| store.get_current_folder());

        let folder_path_str = match lay.current_view {
            View::FolderMessages => {
                let root = &store.root_folder;
                if let Some(path_indices) =
                    layout::get_folder_path_from_display_index(root, lay.selection.folder_index)
                {
                    store.get_folder_path_for_indices(&path_indices)
                } else {
                    store.get_folder_path()
                }
            }
            _ => store.get_folder_path(),
        };

        messages.render_with_folder(f, area, is_active, folder_to_display, &folder_path_str);
    }

    fn render_content_pane(
        f: &mut Frame,
        store: &EmailStore,
        content: &ContentComponent,
        area: Rect,
        is_active: bool,
    ) {
        let theme = VulthorTheme;
        let config = Config::default();
        let ctx = Ctx {
            theme: &theme,
            config: &config,
            store,
        };
        content.render(f, area, is_active, &ctx);
    }

    fn draw_attachments_pane(
        &mut self,
        f: &mut Frame,
        store: &EmailStore,
        lay: &Layout,
        area: Rect,
        is_active: bool,
    ) {
        let border_style = if is_active {
            Style::default().fg(VulthorTheme::ACCENT_LIGHT)
        } else {
            Style::default()
        };

        if let Some(email) = store.get_selected_email() {
            if !email.attachments.is_empty() {
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

                self.attachment_list_state
                    .select(Some(lay.selection.attachment_index));

                f.render_stateful_widget(list, area, &mut self.attachment_list_state);
            } else {
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

    fn draw_status_bar(
        &mut self,
        f: &mut Frame,
        lay: &Layout,
        status_message: &Option<String>,
        area: Rect,
    ) {
        let status_area = Rect {
            x: area.x,
            y: area.bottom() - 1,
            width: area.width,
            height: 1,
        };

        let mut status_text = vec![];

        let help_text = {
            let base_help = "j/k: Navigate | Tab: Switch Pane | h/l: Switch View";
            let content_toggle = if lay.content_pane_hidden {
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

        if let Some(message) = status_message {
            status_text.push(Span::raw(" | "));
            status_text.push(Span::styled(
                message.clone(),
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
            Line::from("  m          - Move email to folder (filterable picker)"),
            Line::from("  u          - Undo last action (session-only)"),
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
