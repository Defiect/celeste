//! Per-remote detail page. Placeholder until the sync-dir list, settings
//! panel and refresh button are ported.

use iced::{widget::text, Element};

use crate::domain::remote::{Remote, RemoteId};

#[derive(Debug, Clone)]
pub enum Msg {
    RefreshNow(RemoteId),
    Back,
}

pub fn view(remote: &Remote) -> Element<'_, Msg> {
    text(format!("Remote: {}", remote.name)).into()
}
