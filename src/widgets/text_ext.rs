//! `text()` drop-in that always opts into `Shaping::Advanced`.
//!
//! iced 0.12's default `Text` widget uses `Shaping::Basic` to save a
//! shaping pass — but that path also skips cosmic-text's font fallback,
//! so any glyph missing from the primary font renders as tofu (`[]`)
//! regardless of what `Settings::fonts` loaded. Every piece of our UI
//! that might contain a non-Latin glyph (⚠ in the picker, emoji in
//! user-facing names, Cyrillic / CJK file paths) should route through
//! here instead of calling `iced::widget::text` directly.

use std::borrow::Cow;

use iced::widget::text::{Shaping, Text};

pub fn text<'a>(content: impl Into<Cow<'a, str>>) -> Text<'a, iced::Theme, iced::Renderer> {
    Text::new(content).shaping(Shaping::Advanced)
}
