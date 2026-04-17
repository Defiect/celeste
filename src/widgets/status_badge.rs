//! A small coloured indicator of a sync-dir's current status.

use iced::{
    widget::{container, text, Row},
    Element, Length,
};

use crate::domain::sync::SyncStatus;

pub fn view<'a, Msg: 'a>(status: &SyncStatus) -> Element<'a, Msg> {
    let label = match status {
        SyncStatus::Idle => "Idle",
        SyncStatus::Syncing => "Syncing…",
        SyncStatus::Ok { .. } => "Synced",
        SyncStatus::Error { .. } => "Error",
    };

    container(
        Row::new().push(text(label).size(12)),
    )
    .padding([2, 8])
    .width(Length::Shrink)
    .into()
}
