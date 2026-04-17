//! The landing page: sidebar listing every configured remote, plus a
//! placeholder while Phase D fills in the remote detail pane.

use std::collections::HashSet;

use iced::{
    widget::{button, column, container, row, scrollable, text, Space},
    Element, Length,
};

use crate::{
    domain::remote::{Remote, RemoteId},
    theme::{PAGE_PADDING, ROW_SPACING, SECTION_SPACING},
};

#[derive(Debug, Clone)]
pub enum Msg {
    Selected(RemoteId),
    RefreshAll,
    AddRemote,
}

pub fn view<'a>(
    remotes: &'a [Remote],
    selected: Option<RemoteId>,
    syncing: &'a HashSet<RemoteId>,
) -> Element<'a, Msg> {
    let header = row![
        text("Celeste").size(24),
        Space::with_width(Length::Fill),
        button(text("Refresh all")).on_press(Msg::RefreshAll),
        button(text("Add remote")).on_press(Msg::AddRemote),
    ]
    .spacing(ROW_SPACING)
    .align_items(iced::Alignment::Center);

    let sidebar = {
        let mut col = column![text("Remotes").size(16)].spacing(ROW_SPACING);
        for remote in remotes {
            let label = if syncing.contains(&remote.id) {
                format!("{}  (syncing…)", remote.name)
            } else if !remote.policy.enabled {
                format!("{}  (paused)", remote.name)
            } else {
                remote.name.clone()
            };
            let btn = button(text(label))
                .width(Length::Fill)
                .on_press(Msg::Selected(remote.id));
            col = col.push(btn);
        }
        scrollable(col).width(Length::Fixed(240.0))
    };

    let body: Element<Msg> = match selected {
        Some(_) => text("Remote details coming soon.").into(),
        None => text("Select a remote in the sidebar.").into(),
    };

    container(
        column![
            header,
            row![sidebar, container(body).width(Length::Fill).padding(PAGE_PADDING)]
                .spacing(PAGE_PADDING),
        ]
        .spacing(SECTION_SPACING),
    )
    .padding(PAGE_PADDING)
    .into()
}
