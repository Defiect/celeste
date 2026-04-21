//! Per-remote detail page: sync-dir cards with log view and exclusion panel.

use std::{collections::HashMap, time::Duration};

use iced::{
    widget::{button, column, container, responsive, row, scrollable, text_input, Rule, Space},
    Alignment, Element, Length,
};

use crate::{
    domain::{
        remote::{Remote, RemoteId},
        sync::{SyncDir, SyncDirExclusion, SyncDirExclusionId, SyncDirId},
    },
    screens::settings,
    theme::{PAGE_PADDING, ROW_SPACING, SECTION_SPACING},
    widgets::text,
};

/// Log scrollable sizing — height grows with the card width so wider
/// windows get a taller log pane. Clamped to keep cards usable on both
/// narrow and very wide layouts.
const LOG_HEIGHT_RATIO: f32 = 0.35;
const LOG_HEIGHT_MIN: f32 = 120.0;
const LOG_HEIGHT_MAX: f32 = 360.0;

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
    ToggleExclusions(SyncDirId),
    DraftExclusionChanged(SyncDirId, String),
    AddExclusion(SyncDirId),
    RemoveExclusion(SyncDirExclusionId, SyncDirId),
    DeleteLocalFiles(String),
}

pub fn view<'a>(
    remote: &'a Remote,
    sync_dirs: &'a [SyncDir],
    log: &'a HashMap<SyncDirId, Vec<String>>,
    all_known_sync_dirs: &'a [SyncDir],
    exclusion_panel: Option<SyncDirId>,
    exclusions: &'a HashMap<SyncDirId, Vec<SyncDirExclusion>>,
    draft_exclusion: &'a HashMap<SyncDirId, String>,
    draft: (&'a str, &'a str),
    next_sync_eta: Option<(Duration, bool)>,
) -> Element<'a, Msg> {
    let countdown: Element<'a, Msg> = match next_sync_eta {
        Some((remaining, in_backoff)) => {
            let label = format!("next sync in {}", format_duration(remaining));
            let countdown_text = text(label).size(12);
            if in_backoff {
                use iced::widget::tooltip;
                tooltip(
                    row![
                        countdown_text,
                        Space::with_width(Length::Fixed(4.0)),
                        text("⚠").size(14),
                    ]
                    .align_items(Alignment::Center),
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
    .align_items(Alignment::Center);

    // ── Sync directory cards ────────────────────────────────────────────────
    let mut cards_col = column![text("Sync directories").size(16)].spacing(ROW_SPACING);

    if sync_dirs.is_empty() {
        cards_col = cards_col.push(text("No sync directories yet.").size(13));
    }

    for sd in sync_dirs {
        let remote_display = if sd.remote_path.is_empty() {
            "/"
        } else {
            &sd.remote_path
        };
        let path_label = format!("{} → {}", sd.local_path, remote_display);

        // Count auto-excluded descendants + user exclusions for the badge.
        let auto_excl = auto_excluded_for(sd, all_known_sync_dirs);
        let custom_excl_count = exclusions.get(&sd.id).map(|v| v.len()).unwrap_or(0);
        let total_excl = auto_excl.len() + custom_excl_count;
        let excl_label = format!("Excluded ({})", total_excl);

        // Top row: paths | [Excluded (n)] [Delete]
        let top_row = row![
            text(path_label).size(13),
            Space::with_width(Length::Fill),
            button(text(excl_label).size(12)).on_press(Msg::ToggleExclusions(sd.id)),
            button(text("Delete").size(12)).on_press(Msg::DeleteSyncDir(
                sd.local_path.clone(),
                sd.remote_path.clone(),
            )),
        ]
        .spacing(ROW_SPACING)
        .align_items(Alignment::Center);

        // Log area: newest entry first so the most recent is always visible.
        // `responsive` lets us scale the pane height with the actual rendered
        // width, so wider cards get a taller log instead of a fixed strip.
        let entries_opt = log.get(&sd.id);
        let log_area = container(responsive(move |size| {
            let height = (size.width * LOG_HEIGHT_RATIO)
                .clamp(LOG_HEIGHT_MIN, LOG_HEIGHT_MAX);
            let mut log_col = column![].spacing(2);
            if let Some(entries) = entries_opt {
                for entry in entries.iter().rev() {
                    log_col = log_col.push(text(entry.as_str()).size(12));
                }
            }
            scrollable(log_col).height(Length::Fixed(height)).into()
        }))
        .height(Length::Fixed(LOG_HEIGHT_MAX));

        let mut card_col = column![top_row, log_area].spacing(ROW_SPACING / 2);

        // ── Exclusion panel (shown when toggled) ────────────────────────────
        if exclusion_panel == Some(sd.id) {
            card_col = card_col.push(Rule::horizontal(1));
            card_col = card_col.push(exclusion_panel_view(
                sd,
                &auto_excl,
                exclusions.get(&sd.id).map(|v| v.as_slice()).unwrap_or(&[]),
                draft_exclusion.get(&sd.id).map(|s| s.as_str()).unwrap_or(""),
            ));
        }

        cards_col = cards_col.push(
            container(card_col)
                .padding(8)
                .width(Length::Fill)
                .style(iced::theme::Container::Box),
        );
    }

    // Inline "Add sync dir" form.
    let (draft_local, draft_remote) = draft;
    let add_form = row![
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
    .spacing(ROW_SPACING);
    cards_col = cards_col.push(add_form);

    let sync_dirs_section = scrollable(cards_col).height(Length::FillPortion(2));

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

/// Build the exclusion panel for one sync_dir card.
fn exclusion_panel_view<'a>(
    sd: &'a SyncDir,
    auto_excl: &[&'a SyncDir],
    custom_excl: &'a [SyncDirExclusion],
    draft: &'a str,
) -> Element<'a, Msg> {
    let mut col = column![].spacing(ROW_SPACING / 2);

    // Auto-excluded (descendant sync_dirs) — read-only, no delete button.
    if !auto_excl.is_empty() {
        col = col.push(text("Auto-excluded:").size(12));
        for desc in auto_excl {
            let relative = desc
                .local_path
                .strip_prefix(&format!("{}/", sd.local_path))
                .unwrap_or(&desc.local_path);
            let local_path = desc.local_path.clone();
            col = col.push(
                row![
                    text(format!("  {relative}")).size(12),
                    Space::with_width(Length::Fill),
                    button(text("Delete local files").size(11))
                        .on_press(Msg::DeleteLocalFiles(local_path)),
                ]
                .align_items(iced::Alignment::Center)
                .spacing(ROW_SPACING),
            );
        }
    }

    // User-defined exclusions — deletable.
    if !custom_excl.is_empty() {
        col = col.push(text("Custom excluded:").size(12));
        for excl in custom_excl {
            let local_path = format!("{}/{}", sd.local_path, excl.remote_path);
            let excl_id = excl.id;
            let sd_id = sd.id;
            col = col.push(
                row![
                    text(format!("  {}", excl.remote_path)).size(12),
                    Space::with_width(Length::Fill),
                    button(text("Delete local files").size(11))
                        .on_press(Msg::DeleteLocalFiles(local_path)),
                    button(text("×").size(11))
                        .on_press(Msg::RemoveExclusion(excl_id, sd_id)),
                ]
                .align_items(iced::Alignment::Center)
                .spacing(ROW_SPACING),
            );
        }
    }

    if auto_excl.is_empty() && custom_excl.is_empty() {
        col = col.push(text("No exclusions.").size(12));
    }

    // Add-exclusion form (remote sub-path input).
    let sd_id = sd.id;
    col = col.push(
        row![
            text_input("Remote sub-path…", draft)
                .on_input(move |s| Msg::DraftExclusionChanged(sd_id, s))
                .padding(4)
                .size(12),
            button(text("Exclude").size(12)).on_press(Msg::AddExclusion(sd_id)),
        ]
        .spacing(ROW_SPACING),
    );

    col.into()
}

/// Sync_dirs from `all` whose local_path is a direct descendant of `sd`.
fn auto_excluded_for<'a>(sd: &SyncDir, all: &'a [SyncDir]) -> Vec<&'a SyncDir> {
    let prefix = format!("{}/", sd.local_path);
    all.iter()
        .filter(|d| d.id != sd.id && d.local_path.starts_with(&prefix))
        .collect()
}

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
