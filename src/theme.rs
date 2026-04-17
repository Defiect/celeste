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
pub const PAGE_PADDING: u16 = 16;

/// Spacing between stacked sections within a page.
pub const SECTION_SPACING: u16 = 12;

/// Spacing between items inside a row.
pub const ROW_SPACING: u16 = 8;
