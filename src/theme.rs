//! Iced theme + style constants.
//!
//! Kept minimal for now — expand as the Iced screens land.

use iced::Theme;

/// The default theme. Dark to match the GNOME/libadwaita default the GTK
/// version runs under.
pub fn celeste_theme() -> Theme {
    Theme::Dark
}

/// Standard outer padding for pages.
pub const PAGE_PADDING: f32 = 16.0;

/// Spacing between stacked sections within a page.
pub const SECTION_SPACING: f32 = 12.0;

/// Spacing between items inside a row.
pub const ROW_SPACING: f32 = 8.0;
