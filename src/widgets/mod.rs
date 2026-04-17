//! Reusable Iced widgets. Each module exports small building-block helpers
//! (functions returning `Element<Message>`); widgets never touch services
//! or domain state directly — they emit `Msg` values that screens map onto
//! service calls.

pub mod duration_picker;
pub mod status_badge;
