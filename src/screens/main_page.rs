//! The landing page: sidebar listing every configured remote, plus a
//! placeholder while Phase D fills in the remote detail pane.

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

pub fn view<'a>(remotes: &'a [Remote], selected: Option<RemoteId>) -> Element<'a, Msg> {
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
            let row = button(text(&remote.name))
                .width(Length::Fill)
                .on_press(Msg::Selected(remote.id));
            col = col.push(row);
        }
        scrollable(col).width(Length::Fixed(220.0))
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
