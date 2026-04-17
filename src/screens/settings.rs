//! Per-remote "Sync Settings" panel (Enabled / Instant sync / Interval).
//! Placeholder until the widget wiring lands.

use iced::{widget::text, Element};

use crate::domain::remote::{Remote, SyncPolicy};

#[derive(Debug, Clone)]
pub enum Msg {
    PolicyChanged(SyncPolicy),
}

pub fn view(_remote: &Remote) -> Element<'_, Msg> {
    text("Sync settings panel coming soon.").into()
}
