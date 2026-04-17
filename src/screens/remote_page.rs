//! Per-remote detail page: header with Refresh now, (future) sync-dirs list,
//! and the Sync Settings panel.

use iced::{
    widget::{button, column, container, row, text, Rule, Space},
    Element, Length,
};

use crate::{
    domain::remote::{Remote, RemoteId},
    screens::settings,
    theme::{SECTION_SPACING, PAGE_PADDING, ROW_SPACING},
};

#[derive(Debug, Clone)]
pub enum Msg {
    RefreshNow(RemoteId),
    Back,
    Settings(settings::Msg),
}

pub fn view(remote: &Remote) -> Element<'_, Msg> {
    let header = row![
        button(text("←")).on_press(Msg::Back),
        text(&remote.name).size(22),
        Space::with_width(Length::Fill),
        button(text("Refresh now")).on_press(Msg::RefreshNow(remote.id)),
    ]
    .spacing(ROW_SPACING)
    .align_items(iced::Alignment::Center);

    let sync_dirs_placeholder =
        text("Sync directories will appear here once the list widget is wired.");

    let settings_panel = settings::view(remote).map(Msg::Settings);

    container(
        column![
            header,
            Rule::horizontal(1),
            sync_dirs_placeholder,
            Rule::horizontal(1),
            settings_panel,
        ]
        .spacing(SECTION_SPACING),
    )
    .padding(PAGE_PADDING)
    .into()
}
