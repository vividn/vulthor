use ratatui::style::Color;

/// Vulthor color theme matching the bird logo
pub struct VulthorTheme;

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
}
