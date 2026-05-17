//! Snapshot tests for built-in theme presets (vu-62n).
//!
//! Each preset's resolved `Theme` is printed deterministically and
//! compared against a saved snapshot under `tests/snapshots/`. The
//! palette is treated as opaque data — any change to a preset's
//! colors will show up in the diff and require explicit acceptance
//! (`cargo insta accept`).

use insta::assert_snapshot;
use ratatui::style::Color;
use vulthor::theme::{Theme, ThemePreset};

/// Render a `Color` as a stable, copy-pasteable string. Snapshot
/// readability matters more here than matching `Debug`'s output
/// exactly; RGB triples land as `#RRGGBB` and named ratatui colors
/// keep their `Debug` form.
fn color_str(c: Color) -> String {
    match c {
        Color::Rgb(r, g, b) => format!("#{:02X}{:02X}{:02X}", r, g, b),
        other => format!("{:?}", other),
    }
}

fn theme_lines(theme: &Theme) -> String {
    [
        ("dark", theme.dark),
        ("primary", theme.primary),
        ("light", theme.light),
        ("accent", theme.accent),
        ("accent_light", theme.accent_light),
        ("cyan", theme.cyan),
        ("cyan_light", theme.cyan_light),
        ("gray_dark", theme.gray_dark),
        ("gray_light", theme.gray_light),
    ]
    .iter()
    .map(|(role, c)| format!("{role:<13} {}", color_str(*c)))
    .collect::<Vec<_>>()
    .join("\n")
}

#[test]
fn preset_snapshot_default_dark() {
    assert_snapshot!(theme_lines(&ThemePreset::DefaultDark.theme()));
}

#[test]
fn preset_snapshot_default_light() {
    assert_snapshot!(theme_lines(&ThemePreset::DefaultLight.theme()));
}

#[test]
fn preset_snapshot_solarized_dark() {
    assert_snapshot!(theme_lines(&ThemePreset::SolarizedDark.theme()));
}

#[test]
fn preset_snapshot_nord() {
    assert_snapshot!(theme_lines(&ThemePreset::Nord.theme()));
}

/// `default-dark` is the implicit base — preset(None) must resolve
/// to it so existing configs render unchanged.
#[test]
fn default_dark_matches_built_in_default() {
    assert_eq!(ThemePreset::DefaultDark.theme(), Theme::default());
}

/// `Ctrl+T` rotation goes default-dark → default-light → solarized-dark
/// → nord → default-dark. `next()` must wrap.
#[test]
fn cycle_order_wraps() {
    let mut seen = Vec::new();
    let mut p = ThemePreset::DefaultDark;
    for _ in 0..ThemePreset::all().len() {
        seen.push(p);
        p = p.next();
    }
    assert_eq!(seen, ThemePreset::all().to_vec());
    // After a full lap we are back at the start.
    assert_eq!(p, ThemePreset::DefaultDark);
}
