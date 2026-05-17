// Color palette + user-loadable themes.
//
// Two surfaces live here:
//   - `VulthorTheme` — the historical unit struct exposing the built-in
//     palette as compile-time `Color` constants. Rendering code still
//     reads `VulthorTheme::PRIMARY` etc. directly; refactoring those to
//     read from the runtime `Theme` is tracked separately.
//   - `Theme` — a runtime, per-role color struct that can be loaded from
//     `~/.config/vulthor/themes/<name>.toml` and tweaked via the
//     `[theme].overrides` map in `vulthor.toml`. `build_theme(&config)`
//     resolves built-in → user-file → overrides into a final `Theme`.

use crate::config::Config;
use crate::error::{Result, VulthorError};
use ratatui::style::Color;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Built-in preset themes (vu-62n). Each variant maps to a concrete
/// [`Theme`] palette via [`ThemePreset::theme`]. Presets are the base
/// layer in [`build_theme`]'s resolution chain: built-in default →
/// preset → user theme file → `[theme].overrides`. `Ctrl+T` cycles
/// through presets at runtime (transient — not persisted).
///
/// `default-dark` is the implicit base when `[theme].preset` is unset,
/// so existing configs keep rendering the same palette they always have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThemePreset {
    DefaultDark,
    DefaultLight,
    SolarizedDark,
    Nord,
}

impl ThemePreset {
    /// Canonical TOML name (kebab-case), used as the value of
    /// `[theme].preset` in `vulthor.toml`.
    pub const fn name(self) -> &'static str {
        match self {
            ThemePreset::DefaultDark => "default-dark",
            ThemePreset::DefaultLight => "default-light",
            ThemePreset::SolarizedDark => "solarized-dark",
            ThemePreset::Nord => "nord",
        }
    }

    /// All presets in cycle order (matches declaration order).
    pub const fn all() -> &'static [ThemePreset] {
        &[
            ThemePreset::DefaultDark,
            ThemePreset::DefaultLight,
            ThemePreset::SolarizedDark,
            ThemePreset::Nord,
        ]
    }

    /// Parse a TOML preset name back into a variant. `None` for typos so
    /// the config validator can raise a structured error.
    pub fn from_name(name: &str) -> Option<ThemePreset> {
        ThemePreset::all().iter().copied().find(|p| p.name() == name)
    }

    /// Next preset in cycle order, wrapping past the last variant.
    /// Drives `Ctrl+T` at runtime.
    pub fn next(self) -> ThemePreset {
        let all = ThemePreset::all();
        let idx = all.iter().position(|p| *p == self).unwrap_or(0);
        all[(idx + 1) % all.len()]
    }

    /// Resolve to a concrete [`Theme`] palette. Each preset returns a
    /// freshly built `Theme` — no shared state, no caching.
    pub fn theme(self) -> Theme {
        match self {
            ThemePreset::DefaultDark => Theme::default(),
            ThemePreset::DefaultLight => Theme {
                // Inverted: paper-light backgrounds, dark text via `dark`.
                dark: Color::Rgb(0xF7, 0xF4, 0xEC),
                primary: Color::Rgb(0xE3, 0xDA, 0xC4),
                light: Color::Rgb(0xC9, 0xBE, 0xA1),
                accent: Color::Rgb(0xC0, 0x4A, 0x16),
                accent_light: Color::Rgb(0xE0, 0x7A, 0x3F),
                cyan: Color::Rgb(0x18, 0x8B, 0xC2),
                cyan_light: Color::Rgb(0x4A, 0xB0, 0xDC),
                gray_dark: Color::Rgb(0x55, 0x5B, 0x6C),
                gray_light: Color::Rgb(0x2C, 0x30, 0x3A),
            },
            ThemePreset::SolarizedDark => Theme {
                // Solarized dark base palette (Ethan Schoonover).
                dark: Color::Rgb(0x00, 0x2B, 0x36),       // base03
                primary: Color::Rgb(0x07, 0x36, 0x42),    // base02
                light: Color::Rgb(0x58, 0x6E, 0x75),      // base01
                accent: Color::Rgb(0xCB, 0x4B, 0x16),     // orange
                accent_light: Color::Rgb(0xB5, 0x89, 0x00), // yellow
                cyan: Color::Rgb(0x2A, 0xA1, 0x98),       // cyan
                cyan_light: Color::Rgb(0x26, 0x8B, 0xD2), // blue
                gray_dark: Color::Rgb(0x65, 0x7B, 0x83),  // base00
                gray_light: Color::Rgb(0xEE, 0xE8, 0xD5), // base2
            },
            ThemePreset::Nord => Theme {
                // Nord palette (Arctic Ice Studio).
                dark: Color::Rgb(0x2E, 0x34, 0x40),       // nord0
                primary: Color::Rgb(0x3B, 0x42, 0x52),    // nord1
                light: Color::Rgb(0x43, 0x4C, 0x5E),      // nord2
                accent: Color::Rgb(0xD0, 0x87, 0x70),     // nord12 (orange)
                accent_light: Color::Rgb(0xEB, 0xCB, 0x8B), // nord13 (yellow)
                cyan: Color::Rgb(0x88, 0xC0, 0xD0),       // nord8
                cyan_light: Color::Rgb(0x8F, 0xBC, 0xBB), // nord7
                gray_dark: Color::Rgb(0x4C, 0x56, 0x6A),  // nord3
                gray_light: Color::Rgb(0xEC, 0xEF, 0xF4), // nord6
            },
        }
    }
}

/// Vulthor color theme matching the bird logo
pub struct VulthorTheme;

#[allow(dead_code)]
impl VulthorTheme {
    /// Dark teal/navy from bird body
    pub const DARK: Color = Color::Rgb(26, 47, 58);

    /// Main teal color
    pub const PRIMARY: Color = Color::Rgb(44, 79, 93);

    /// Lighter teal
    pub const LIGHT: Color = Color::Rgb(61, 98, 112);

    /// Orange from bird's neck
    pub const ACCENT: Color = Color::Rgb(255, 140, 66);

    /// Lighter orange
    pub const ACCENT_LIGHT: Color = Color::Rgb(255, 170, 90);

    /// Light cyan from goggles/lightning
    pub const CYAN: Color = Color::Rgb(125, 211, 192);

    /// Lighter cyan
    pub const CYAN_LIGHT: Color = Color::Rgb(165, 230, 215);

    /// Dark gray
    pub const GRAY_DARK: Color = Color::Rgb(74, 85, 104);

    /// Light gray
    pub const GRAY_LIGHT: Color = Color::Rgb(226, 232, 240);

    /// Active pane border color
    pub const ACTIVE_BORDER: Color = Self::CYAN;

    /// Inactive text color
    pub const INACTIVE: Color = Self::GRAY_DARK;

    /// Selection highlight background
    pub const SELECTION_BG: Color = Self::PRIMARY;

    /// Warning/status color
    pub const WARNING: Color = Self::ACCENT;

    /// Status bar background
    pub const STATUS_BG: Color = Self::DARK;

    /// Hex form of [`Self::DARK`] (26,47,58) — used by web surfaces
    /// (PWA manifest `background_color`) that need a CSS color string,
    /// not a ratatui `Color`. Kept beside `DARK` so a future palette
    /// rotation updates both at once.
    pub const DARK_HEX: &'static str = "#1a2f3a";

    /// Hex form of [`Self::PRIMARY`] (44,79,93) — paired with
    /// [`Self::DARK_HEX`] for the PWA manifest `theme_color`.
    pub const PRIMARY_HEX: &'static str = "#2c4f5d";
}

/// Runtime color theme. Each role is a single `ratatui::Color`. The
/// built-in default is sourced from [`VulthorTheme`]; user themes and
/// per-role overrides replace individual fields.
///
/// Override keys (TOML field names) match the snake_case role names
/// below verbatim — e.g. `primary = "#2C4F5D"` or
/// `accent = "#FF8C42"`. Unknown keys are rejected (typo guard).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    pub dark: Color,
    pub primary: Color,
    pub light: Color,
    pub accent: Color,
    pub accent_light: Color,
    pub cyan: Color,
    pub cyan_light: Color,
    pub gray_dark: Color,
    pub gray_light: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            dark: VulthorTheme::DARK,
            primary: VulthorTheme::PRIMARY,
            light: VulthorTheme::LIGHT,
            accent: VulthorTheme::ACCENT,
            accent_light: VulthorTheme::ACCENT_LIGHT,
            cyan: VulthorTheme::CYAN,
            cyan_light: VulthorTheme::CYAN_LIGHT,
            gray_dark: VulthorTheme::GRAY_DARK,
            gray_light: VulthorTheme::GRAY_LIGHT,
        }
    }
}

/// On-disk schema for `~/.config/vulthor/themes/<name>.toml`. Every
/// field is optional — missing roles fall back to the built-in default.
#[derive(Debug, Default, Deserialize)]
struct ThemeFile {
    #[serde(default)]
    dark: Option<String>,
    #[serde(default)]
    primary: Option<String>,
    #[serde(default)]
    light: Option<String>,
    #[serde(default)]
    accent: Option<String>,
    #[serde(default)]
    accent_light: Option<String>,
    #[serde(default)]
    cyan: Option<String>,
    #[serde(default)]
    cyan_light: Option<String>,
    #[serde(default)]
    gray_dark: Option<String>,
    #[serde(default)]
    gray_light: Option<String>,
}

impl ThemeFile {
    fn into_override_map(self) -> BTreeMap<String, String> {
        let mut map = BTreeMap::new();
        let entries = [
            ("dark", self.dark),
            ("primary", self.primary),
            ("light", self.light),
            ("accent", self.accent),
            ("accent_light", self.accent_light),
            ("cyan", self.cyan),
            ("cyan_light", self.cyan_light),
            ("gray_dark", self.gray_dark),
            ("gray_light", self.gray_light),
        ];
        for (k, v) in entries {
            if let Some(s) = v {
                map.insert(k.to_string(), s);
            }
        }
        map
    }
}

/// Parse a color string — `#RRGGBB`, `#RGB`, or a named ratatui color
/// (case-insensitive: `red`, `light_blue`, `dark_gray`, etc.).
fn parse_color(input: &str) -> Result<Color> {
    let s = input.trim();
    if let Some(hex) = s.strip_prefix('#') {
        return parse_hex(hex).ok_or_else(|| VulthorError::Config {
            message: format!("invalid hex color: {input:?}"),
        });
    }
    parse_named(s).ok_or_else(|| VulthorError::Config {
        message: format!("unknown color name: {input:?}"),
    })
}

fn parse_hex(hex: &str) -> Option<Color> {
    let bytes = hex.as_bytes();
    match bytes.len() {
        6 => {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            Some(Color::Rgb(r, g, b))
        }
        3 => {
            let r = u8::from_str_radix(&hex[0..1], 16).ok()?;
            let g = u8::from_str_radix(&hex[1..2], 16).ok()?;
            let b = u8::from_str_radix(&hex[2..3], 16).ok()?;
            Some(Color::Rgb(r * 0x11, g * 0x11, b * 0x11))
        }
        _ => None,
    }
}

fn parse_named(name: &str) -> Option<Color> {
    // Normalize so `LightBlue`, `light_blue`, `light-blue`, and
    // `lightblue` all map to the same arm: lowercase + drop word
    // separators. Matches are written in the compact form.
    let n: String = name
        .chars()
        .filter(|c| *c != '_' && *c != '-')
        .flat_map(|c| c.to_lowercase())
        .collect();
    Some(match n.as_str() {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "gray" | "grey" => Color::Gray,
        "darkgray" | "darkgrey" => Color::DarkGray,
        "lightred" => Color::LightRed,
        "lightgreen" => Color::LightGreen,
        "lightyellow" => Color::LightYellow,
        "lightblue" => Color::LightBlue,
        "lightmagenta" => Color::LightMagenta,
        "lightcyan" => Color::LightCyan,
        "white" => Color::White,
        "reset" => Color::Reset,
        _ => return None,
    })
}

/// Apply a role-name → color-string map onto `theme`. Unknown role
/// keys are rejected (typo guard) and invalid color strings fail with
/// `VulthorError::Config`.
pub fn apply_overrides(mut theme: Theme, overrides: &BTreeMap<String, String>) -> Result<Theme> {
    for (role, value) in overrides {
        let color = parse_color(value)?;
        match role.as_str() {
            "dark" => theme.dark = color,
            "primary" => theme.primary = color,
            "light" => theme.light = color,
            "accent" => theme.accent = color,
            "accent_light" => theme.accent_light = color,
            "cyan" => theme.cyan = color,
            "cyan_light" => theme.cyan_light = color,
            "gray_dark" => theme.gray_dark = color,
            "gray_light" => theme.gray_light = color,
            unknown => {
                return Err(VulthorError::Config {
                    message: format!("unknown theme role: {unknown:?}"),
                });
            }
        }
    }
    Ok(theme)
}

/// Load a user theme by name from
/// `~/.config/vulthor/themes/<name>.toml`. Missing fields fall back to
/// the built-in default; invalid colors / unknown role keys fail loud.
pub fn load_user_theme(name: &str) -> Result<Theme> {
    let dir = user_themes_dir().ok_or_else(|| VulthorError::Config {
        message: "could not resolve home directory for theme lookup".into(),
    })?;
    let path = dir.join(format!("{name}.toml"));
    load_user_theme_from_path(&path)
}

/// Test/internal entry-point: load a user theme from an explicit path.
/// `load_user_theme` resolves the home-relative path and calls into
/// here. Public so the integration tests can drive it without mucking
/// with `$HOME`.
pub fn load_user_theme_from_path(path: &Path) -> Result<Theme> {
    let contents = std::fs::read_to_string(path)?;
    let file: ThemeFile = toml::from_str(&contents).map_err(|e| VulthorError::Config {
        message: format!("failed to parse theme file {}: {e}", path.display()),
    })?;
    apply_overrides(Theme::default(), &file.into_override_map())
}

fn user_themes_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".config/vulthor/themes"))
}

/// Resolve a final [`Theme`] from a [`Config`]: preset (default
/// `default-dark`) → user theme file (if `config.theme.name` is set) →
/// `config.theme.overrides`. `[theme].preset` is the base palette;
/// `[theme].name` (when set) replaces it; `[theme].overrides` win over
/// both.
pub fn build_theme(config: &Config) -> Result<Theme> {
    build_theme_with(config, load_user_theme)
}

/// Internal variant that lets tests inject a loader closure so they
/// don't have to touch `$HOME`. Resolution order is identical to
/// [`build_theme`].
pub fn build_theme_with<F>(config: &Config, mut loader: F) -> Result<Theme>
where
    F: FnMut(&str) -> Result<Theme>,
{
    let base = match &config.theme.name {
        Some(name) => loader(name)?,
        None => preset_from_config(&config.theme.preset)?.theme(),
    };
    apply_overrides(base, &config.theme.overrides)
}

/// Resolve `[theme].preset` to a [`ThemePreset`]. `None` → default-dark
/// (the implicit base). Unknown names fail loud so typos don't silently
/// fall back to the built-in palette.
pub fn preset_from_config(name: &Option<String>) -> Result<ThemePreset> {
    match name {
        None => Ok(ThemePreset::DefaultDark),
        Some(n) => ThemePreset::from_name(n).ok_or_else(|| VulthorError::Config {
            message: format!(
                "unknown [theme].preset {n:?} (valid: default-dark, default-light, solarized-dark, nord)"
            ),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ThemeConfig;
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    fn cfg_with_theme(theme: ThemeConfig) -> Config {
        Config {
            theme,
            ..Config::default()
        }
    }

    #[test]
    fn theme_default_matches_built_in_palette() {
        // Built-in default surfaces the same colors `VulthorTheme`'s
        // constants do — so the existing render code keeps rendering
        // the same palette when no user theme/override is configured.
        let t = Theme::default();
        assert_eq!(t.primary, VulthorTheme::PRIMARY);
        assert_eq!(t.accent, VulthorTheme::ACCENT);
        assert_eq!(t.cyan, VulthorTheme::CYAN);
        assert_eq!(t.gray_dark, VulthorTheme::GRAY_DARK);
    }

    #[test]
    fn build_theme_returns_built_in_when_unset() {
        // name=None + overrides empty → identical to `Theme::default`.
        let cfg = cfg_with_theme(ThemeConfig::default());
        let resolved = build_theme(&cfg).expect("builds");
        assert_eq!(resolved, Theme::default());
    }

    #[test]
    fn parse_color_accepts_six_digit_hex() {
        assert_eq!(parse_color("#FF8C42").unwrap(), Color::Rgb(255, 140, 66));
        // Case-insensitive on the hex digits.
        assert_eq!(parse_color("#ff8c42").unwrap(), Color::Rgb(255, 140, 66));
    }

    #[test]
    fn parse_color_accepts_three_digit_hex() {
        // `#f80` expands like CSS: each nibble is repeated → 0xFF, 0x88, 0x00.
        assert_eq!(parse_color("#f80").unwrap(), Color::Rgb(0xFF, 0x88, 0x00));
    }

    #[test]
    fn parse_color_accepts_named_ratatui_colors() {
        assert_eq!(parse_color("red").unwrap(), Color::Red);
        assert_eq!(parse_color("LightBlue").unwrap(), Color::LightBlue);
        assert_eq!(parse_color("dark-gray").unwrap(), Color::DarkGray);
    }

    #[test]
    fn parse_color_rejects_invalid_hex() {
        // Wrong digit count / non-hex chars surface as `Config { .. }`.
        assert!(matches!(
            parse_color("#ZZZ"),
            Err(VulthorError::Config { .. })
        ));
        assert!(matches!(
            parse_color("#12345"),
            Err(VulthorError::Config { .. })
        ));
    }

    #[test]
    fn parse_color_rejects_unknown_name() {
        assert!(matches!(
            parse_color("chartreuse"),
            Err(VulthorError::Config { .. })
        ));
    }

    #[test]
    fn apply_overrides_replaces_named_roles() {
        let mut overrides = BTreeMap::new();
        overrides.insert("primary".into(), "#2C4F5D".into());
        overrides.insert("accent".into(), "#FF8C42".into());
        let out = apply_overrides(Theme::default(), &overrides).unwrap();
        assert_eq!(out.primary, Color::Rgb(0x2C, 0x4F, 0x5D));
        assert_eq!(out.accent, Color::Rgb(0xFF, 0x8C, 0x42));
        // Untouched roles keep their built-in value.
        assert_eq!(out.cyan, VulthorTheme::CYAN);
    }

    #[test]
    fn apply_overrides_rejects_unknown_role_typo() {
        // Typo guard: `primry` is not a valid role — better to fail
        // loud than silently drop the user's intent.
        let mut overrides = BTreeMap::new();
        overrides.insert("primry".into(), "#000000".into());
        let err = apply_overrides(Theme::default(), &overrides).unwrap_err();
        assert!(matches!(err, VulthorError::Config { .. }));
        assert!(err.to_string().contains("primry"));
    }

    #[test]
    fn apply_overrides_rejects_invalid_color() {
        let mut overrides = BTreeMap::new();
        overrides.insert("primary".into(), "not-a-color".into());
        let err = apply_overrides(Theme::default(), &overrides).unwrap_err();
        assert!(matches!(err, VulthorError::Config { .. }));
    }

    #[test]
    fn load_user_theme_from_path_round_trips_colors() {
        // Writing a theme file then loading it back yields the
        // configured colors verbatim. Unset roles inherit from the
        // built-in default.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("solarized.toml");
        std::fs::write(
            &path,
            r##"
primary = "#268BD2"
accent  = "#CB4B16"
"##,
        )
        .unwrap();

        let loaded = load_user_theme_from_path(&path).unwrap();
        assert_eq!(loaded.primary, Color::Rgb(0x26, 0x8B, 0xD2));
        assert_eq!(loaded.accent, Color::Rgb(0xCB, 0x4B, 0x16));
        assert_eq!(loaded.cyan, VulthorTheme::CYAN); // inherited
    }

    #[test]
    fn load_user_theme_from_path_rejects_invalid_hex() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("broken.toml");
        std::fs::write(&path, r##"primary = "#nothex""##).unwrap();
        let err = load_user_theme_from_path(&path).unwrap_err();
        assert!(matches!(err, VulthorError::Config { .. }));
    }

    #[test]
    fn build_theme_layers_file_then_overrides() {
        // Resolution order: built-in → user theme file → [theme].overrides.
        // The override for `primary` must beat the file's `primary`.
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("solarized.toml");
        std::fs::write(
            &file_path,
            r##"
primary = "#268BD2"
accent  = "#CB4B16"
"##,
        )
        .unwrap();

        let mut overrides = BTreeMap::new();
        overrides.insert("primary".into(), "#000000".into());
        let cfg = cfg_with_theme(ThemeConfig {
            name: Some("solarized".into()),
            overrides,
        });

        let resolved = build_theme_with(&cfg, |name| {
            assert_eq!(name, "solarized");
            load_user_theme_from_path(&file_path)
        })
        .unwrap();

        // Override wins over file.
        assert_eq!(resolved.primary, Color::Rgb(0, 0, 0));
        // File value comes through where override is silent.
        assert_eq!(resolved.accent, Color::Rgb(0xCB, 0x4B, 0x16));
        // Built-in default comes through where both are silent.
        assert_eq!(resolved.cyan, VulthorTheme::CYAN);
    }

    #[test]
    fn build_theme_propagates_loader_errors() {
        // If a theme name is configured but loading fails, build_theme
        // surfaces the error rather than silently dropping back to the
        // built-in default — failing loud beats a confusing palette.
        let cfg = cfg_with_theme(ThemeConfig {
            name: Some("does-not-exist".into()),
            overrides: BTreeMap::new(),
        });
        let err = build_theme_with(&cfg, |_| {
            Err(VulthorError::Config {
                message: "boom".into(),
            })
        })
        .unwrap_err();
        assert!(matches!(err, VulthorError::Config { .. }));
    }
}
