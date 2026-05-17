// `?` help overlay (vu-dzm).
//
// Renders a centered, sectioned cheatsheet of every resolved keymap
// binding. Bindings are grouped by [`PaneScope`] (Global, Folders,
// Messages, Content, Compose) so the user can scan for the pane they're
// in. Each row is `<key>  <description>`, with the key column padded to
// a uniform width so the descriptions line up.
//
// The data comes from [`Keymap::bindings`] + [`Action::scope`] +
// [`Action::description`] — there is no hand-maintained string list, so
// the overlay can never drift from the live keymap.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use crate::keymap::{Action, Keymap, PaneScope};
use crate::theme::Theme;

/// Centered overlay rectangle covering ~60% of the screen, clamped so
/// it always fits inside `area`. The clamp matters at very small
/// terminals (snapshot tests at 60×20, the legacy 40×10 folder test
/// harness, etc.) where 60% rounds to fewer cells than the overlay's
/// minimum legible width.
pub fn centered_overlay_rect(area: Rect) -> Rect {
    let w = ((area.width as u32 * 60) / 100).clamp(20, area.width as u32) as u16;
    let h = ((area.height as u32 * 80) / 100).clamp(10, area.height as u32) as u16;
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}

/// Build the lines rendered inside the help overlay. Public for testing
/// (snapshot + unit tests assert the line layout directly without
/// needing a `Frame`).
pub fn help_lines(keymap: &Keymap, theme: &Theme) -> Vec<Line<'static>> {
    let bindings: Vec<(Action, String)> = keymap
        .bindings()
        .map(|(a, k)| (a, k.to_string()))
        .collect();

    let key_width = bindings
        .iter()
        .map(|(_, k)| k.chars().count())
        .max()
        .unwrap_or(6)
        .max(6);

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(Span::styled(
        "Vulthor — keyboard bindings  (press ?, Esc, or q to close)",
        Style::default()
            .fg(theme.cyan)
            .add_modifier(Modifier::BOLD),
    )));

    for scope in PaneScope::all() {
        let rows: Vec<&(Action, String)> = bindings
            .iter()
            .filter(|(a, _)| a.scope() == *scope)
            .collect();
        if rows.is_empty() {
            continue;
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            scope.title().to_string(),
            Style::default()
                .fg(theme.yellow)
                .add_modifier(Modifier::BOLD),
        )));
        for (action, key) in rows {
            let padded_key = format!("{:<width$}", key, width = key_width);
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    padded_key,
                    Style::default()
                        .fg(theme.green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::raw(action.description().to_string()),
            ]));
        }
    }

    lines
}

/// Draw the help overlay into `area`. `Clear` wipes whatever was
/// painted under the overlay so the cheatsheet renders cleanly on top
/// of the normal pane layout.
pub fn render_help_overlay(f: &mut Frame, area: Rect, keymap: &Keymap, theme: &Theme) {
    let rect = centered_overlay_rect(area);
    let block = Block::default()
        .borders(Borders::ALL)
        .style(Style::default().fg(theme.cyan))
        .title(" Help ");
    let lines = help_lines(keymap, theme);
    let paragraph = Paragraph::new(lines).block(block);
    f.render_widget(Clear, rect);
    f.render_widget(paragraph, rect);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::resolve_keymap;
    use std::collections::BTreeMap;

    fn defaults() -> Keymap {
        resolve_keymap(&BTreeMap::new()).expect("defaults resolve")
    }

    fn rendered_text(keymap: &Keymap) -> String {
        help_lines(keymap, &Theme::default())
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn help_lines_include_every_pane_scope_header_with_bindings() {
        let keymap = defaults();
        let text = rendered_text(&keymap);
        for scope in PaneScope::all() {
            assert!(
                text.contains(scope.title()),
                "missing section header `{}` in:\n{text}",
                scope.title()
            );
        }
    }

    #[test]
    fn help_lines_include_every_resolved_action_description() {
        let keymap = defaults();
        let text = rendered_text(&keymap);
        for action in Action::all() {
            assert!(
                text.contains(action.description()),
                "missing description `{}` for {:?} in:\n{text}",
                action.description(),
                action
            );
        }
    }

    #[test]
    fn help_lines_show_user_override_keystring() {
        // Rebinding archive → e must surface `e` (not the default `a`)
        // in the help overlay. The overlay is driven entirely off the
        // resolved keymap, so this is a regression test for the
        // bindings()-passthrough.
        let mut overrides = BTreeMap::new();
        overrides.insert("archive".to_string(), "e".to_string());
        overrides.insert("draft_edit".to_string(), "E".to_string());
        let keymap = resolve_keymap(&overrides).unwrap();
        let text = rendered_text(&keymap);
        // The Archive row prints the user-chosen key.
        assert!(
            text.lines()
                .any(|l| l.contains("Archive email") && l.contains('e')),
            "Archive row should show user-chosen `e`:\n{text}"
        );
        // The pre-override default `a` no longer carries Archive.
        assert!(
            !text
                .lines()
                .any(|l| l.contains("Archive email") && l.contains(" a ")),
            "Archive row must not still print the retired `a`:\n{text}"
        );
    }

    #[test]
    fn centered_overlay_rect_is_bounded_by_input_area() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        let r = centered_overlay_rect(area);
        assert!(r.x + r.width <= area.x + area.width);
        assert!(r.y + r.height <= area.y + area.height);
        assert!(r.width > 0 && r.height > 0);
    }
}
