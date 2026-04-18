//! Per-remote detail page: header with Refresh now, (future) sync-dirs list,
//! and the Sync Settings panel.

use std::{collections::HashMap, time::Duration};

use iced::{
    widget::{button, column, container, row, scrollable, text_input, tooltip, Rule, Space},
    Element, Length,
};

use crate::{
    domain::{
        remote::{Remote, RemoteId},
        sync::{SyncDir, SyncDirId, SyncError},
    },
    screens::settings,
    theme::{PAGE_PADDING, ROW_SPACING, SECTION_SPACING},
    widgets::text,
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
    next_sync_eta: Option<(Duration, bool)>,
) -> Element<'a, Msg> {
    let countdown: Element<'a, Msg> = match next_sync_eta {
        Some((remaining, in_backoff)) => {
            let label = format!("next sync in {}", format_duration(remaining));
            let countdown_text = text(label).size(12);
            if in_backoff {
                tooltip(
                    row![
                        countdown_text,
                        Space::with_width(Length::Fixed(4.0)),
                        text("⚠").size(14),
                    ]
                    .align_items(iced::Alignment::Center),
                    text(
                        "Backoff active — the provider returned rate-limit \
                         warnings on the last pass, so Celeste will skip \
                         the next cycles before trying again. The more \
                         consecutive degraded passes, the more cycles are \
                         skipped. Resets on the next clean pass.",
                    )
                    .size(12),
                    tooltip::Position::Bottom,
                )
                .gap(8)
                .padding(8)
                .into()
            } else {
                countdown_text.into()
            }
        }
        None => text("paused").size(12).into(),
    };

    let header = row![
        button(text("←")).on_press(Msg::Back),
        text(&remote.name).size(22),
        Space::with_width(Length::Fixed(12.0)),
        countdown,
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

/// Humanise a `Duration` for the sync countdown: "0s" when we're due
/// right now, compact "Xs" / "XmYs" / "XhYm" otherwise.
fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs == 0 {
        return "0s".to_owned();
    }
    let minutes = secs / 60;
    let seconds = secs % 60;
    let hours = minutes / 60;
    let minutes_rem = minutes % 60;
    if hours > 0 {
        format!("{hours}h{minutes_rem:02}m")
    } else if minutes > 0 {
        format!("{minutes}m{seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

#[cfg(test)]
mod tests {
    use super::format_duration;
    use std::time::Duration;

    #[test]
    fn format_duration_shapes() {
        assert_eq!(format_duration(Duration::ZERO), "0s");
        assert_eq!(format_duration(Duration::from_secs(9)), "9s");
        assert_eq!(format_duration(Duration::from_secs(65)), "1m05s");
        assert_eq!(format_duration(Duration::from_secs(3_900)), "1h05m");
    }
}
