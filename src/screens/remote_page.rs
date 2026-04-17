//! Per-remote detail page: header with Refresh now, (future) sync-dirs list,
//! and the Sync Settings panel.

use std::collections::HashMap;

use iced::{
    widget::{button, column, container, row, scrollable, text, Rule, Space},
    Element, Length,
};

use crate::{
    domain::{
        remote::{Remote, RemoteId},
        sync::{SyncDir, SyncDirId, SyncError},
    },
    screens::settings,
    theme::{PAGE_PADDING, ROW_SPACING, SECTION_SPACING},
};

#[derive(Debug, Clone)]
pub enum Msg {
    RefreshNow(RemoteId),
    Back,
    Settings(settings::Msg),
}

pub fn view<'a>(
    remote: &'a Remote,
    sync_dirs: &'a [SyncDir],
    status: &'a HashMap<SyncDirId, String>,
    errors: &'a HashMap<SyncDirId, Vec<SyncError>>,
) -> Element<'a, Msg> {
    let header = row![
        button(text("←")).on_press(Msg::Back),
        text(&remote.name).size(22),
        Space::with_width(Length::Fill),
        button(text("Refresh now")).on_press(Msg::RefreshNow(remote.id)),
    ]
    .spacing(ROW_SPACING)
    .align_items(iced::Alignment::Center);

    let sync_dirs_section: Element<'a, Msg> = if sync_dirs.is_empty() {
        text("No sync directories yet — add one in the existing GTK UI.")
            .size(14)
            .into()
    } else {
        let mut col = column![text("Sync directories").size(16)].spacing(ROW_SPACING);
        for sd in sync_dirs {
            let mut row = iced::widget::Row::new().spacing(8);
            row = row.push(text(&sd.local_path).size(13));
            row = row.push(text("→").size(13));
            row = row.push(text(&sd.remote_path).size(13));
            if let Some(status_text) = status.get(&sd.id) {
                row = row.push(Space::with_width(Length::Fill));
                row = row.push(text(status_text).size(12));
            }
            col = col.push(row);
            if let Some(errs) = errors.get(&sd.id) {
                for err in errs {
                    let line = match err {
                        SyncError::General(path, msg) => {
                            format!("  ⚠ {path}: {msg}")
                        }
                        SyncError::BothMoreCurrent(local, remote) => {
                            format!("  ⚠ Conflict: '{local}' vs '{remote}'")
                        }
                    };
                    col = col.push(text(line).size(12));
                }
            }
        }
        scrollable(col).height(Length::FillPortion(2)).into()
    };

    let settings_panel = settings::view(remote).map(Msg::Settings);

    container(
        column![
            header,
            Rule::horizontal(1),
            sync_dirs_section,
            Rule::horizontal(1),
            settings_panel,
        ]
        .spacing(SECTION_SPACING),
    )
    .padding(PAGE_PADDING)
    .into()
}
