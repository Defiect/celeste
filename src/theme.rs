//! Iced theme + style constants.
//!
//! Kept minimal for now — expand as the Iced screens land.

use iced::{theme, Theme};

/// Pick the iced [`Theme`] that mirrors the system colour-scheme iced
/// reports (via `system::theme_changes`, ultimately the freedesktop
/// `org.freedesktop.appearance.color-scheme` portal). `Mode::None`
/// falls back to `Dark` to keep parity with the GNOME/libadwaita
/// default the GTK build inherited from upstream.
pub fn celeste_theme(mode: theme::Mode) -> Theme {
    match mode {
        theme::Mode::Light => Theme::Light,
        theme::Mode::Dark | theme::Mode::None => Theme::Dark,
    }
}

/// Standard outer padding for pages.
pub const PAGE_PADDING: f32 = 16.0;

/// Spacing between stacked sections within a page.
pub const SECTION_SPACING: f32 = 12.0;

/// Spacing between items inside a row.
pub const ROW_SPACING: f32 = 8.0;
