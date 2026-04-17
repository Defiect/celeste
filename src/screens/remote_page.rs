//! Per-remote detail page: header with Refresh now, (future) sync-dirs list,
//! and the Sync Settings panel.

use std::collections::HashMap;

use iced::{
    widget::{button, column, container, row, scrollable, text, text_input, Rule, Space},
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
    DraftLocalPathChanged(String),
    DraftRemotePathChanged(String),
    AddSyncDir,
    DeleteSyncDir(String, String),
    DeleteRemote(RemoteId, String),
}

pub fn view<'a>(
    remote: &'a Remote,
    sync_dirs: &'a [SyncDir],
    status: &'a HashMap<SyncDirId, String>,
    pending: &'a HashMap<SyncDirId, String>,
    errors: &'a HashMap<SyncDirId, Vec<SyncError>>,
    draft: (&'a str, &'a str),
) -> Element<'a, Msg> {
    let header = row![
        button(text("←")).on_press(Msg::Back),
        text(&remote.name).size(22),
        Space::with_width(Length::Fill),
        button(text("Refresh now")).on_press(Msg::RefreshNow(remote.id)),
        button(text("Delete remote"))
            .on_press(Msg::DeleteRemote(remote.id, remote.name.clone())),
    ]
    .spacing(ROW_SPACING)
    .align_items(iced::Alignment::Center);

    let sync_dirs_section: Element<'a, Msg> = {
        let mut col = column![text("Sync directories").size(16)].spacing(ROW_SPACING);
        if sync_dirs.is_empty() {
            col = col.push(text("No sync directories yet.").size(13));
        }
        for sd in sync_dirs {
            let mut header = iced::widget::Row::new().spacing(8);
            header = header.push(text(&sd.local_path).size(13));
            header = header.push(text("→").size(13));
            header = header.push(text(&sd.remote_path).size(13));
            header = header.push(Space::with_width(Length::Fill));
            if let Some(status_text) = status.get(&sd.id) {
                header = header.push(text(status_text).size(12));
            }
            header = header.push(
                button(text("Delete").size(12)).on_press(Msg::DeleteSyncDir(
                    sd.local_path.clone(),
                    sd.remote_path.clone(),
                )),
            );
            col = col.push(header);
            // Pending-event line: transient state that's not the
            // primary status (e.g. "Checking for changes…",
            // "Refresh queued…"). Rendered dim, indented.
            if let Some(pending_text) = pending.get(&sd.id) {
                col = col.push(text(format!("  · {pending_text}")).size(12));
            }
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

        // Inline "Add sync dir" form.
        let (draft_local, draft_remote) = draft;
        let form = row![
            text_input("Local path…", draft_local)
                .on_input(Msg::DraftLocalPathChanged)
                .padding(6)
                .size(13),
            text_input("Remote path…", draft_remote)
                .on_input(Msg::DraftRemotePathChanged)
                .padding(6)
                .size(13),
            button(text("Add")).on_press(Msg::AddSyncDir),
        ]
        .spacing(8);
        col = col.push(form);

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
