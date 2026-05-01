//! Per-remote detail page: sync-dir cards with log view and exclusion panel.

use std::{collections::HashMap, time::Duration};

use iced::{
    widget::{
        button, column, container, row, scrollable, svg, text_editor, text_input, Rule, Space,
    },
    Alignment, Element, Length,
};

use crate::{
    domain::{
        events::SyncDirRunState,
        remote::{Remote, RemoteId},
        sync::{SyncDir, SyncDirExclusion, SyncDirExclusionId, SyncDirId},
    },
    screens::settings,
    theme::{PAGE_PADDING, ROW_SPACING, SECTION_SPACING},
    widgets::text,
};

/// Fixed height of the per-sync-dir log editor.
const LOG_HEIGHT: f32 = 100.0;
/// Maximum log lines retained per sync_dir before the oldest are dropped
/// to keep memory bounded across long-running sessions.
pub const MAX_LOG_LINES: usize = 200;
/// Side length of the per-card status icon.
const STATUS_ICON_SIZE: f32 = 16.0;

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
    Reauthenticate(RemoteId, String),
    /// Read-only log editor swallows edits but forwards scroll/select
    /// actions so users can drag through history.
    LogEditorAction(SyncDirId, text_editor::Action),
}

pub fn view<'a>(
    remote: &'a Remote,
    sync_dirs: &'a [SyncDir],
    log: &'a HashMap<SyncDirId, text_editor::Content>,
    status: &'a HashMap<SyncDirId, SyncDirRunState>,
    all_known_sync_dirs: &'a [SyncDir],
    exclusion_panel: Option<SyncDirId>,
    exclusions: &'a HashMap<SyncDirId, Vec<SyncDirExclusion>>,
    draft_exclusion: &'a HashMap<SyncDirId, String>,
    draft: (&'a str, &'a str),
    next_sync_eta: Option<(Duration, bool)>,
    needs_reauth: bool,
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
        button(text("Reauthenticate"))
            .on_press(Msg::Reauthenticate(remote.id, remote.name.clone())),
        button(text("Delete remote"))
            .on_press(Msg::DeleteRemote(remote.id, remote.name.clone())),
    ]
    .spacing(ROW_SPACING)
    .align_items(Alignment::Center);

    // ── Re-auth banner (native-proton session missing / expired) ───────────
    let reauth_banner: Option<Element<'a, Msg>> = if needs_reauth {
        let msg = "This remote's session isn't loaded. Sync is paused \
                   until you re-authenticate. Your sync directories, \
                   exclusions, and schedule will be preserved.";
        Some(
            container(
                row![
                    text(format!("⚠ {msg}")).size(13),
                    Space::with_width(Length::Fill),
                    button(text("Reauthenticate"))
                        .on_press(Msg::Reauthenticate(remote.id, remote.name.clone())),
                ]
                .align_items(Alignment::Center)
                .spacing(ROW_SPACING),
            )
            .padding(8)
            .width(Length::Fill)
            .style(iced::theme::Container::Box)
            .into(),
        )
    } else {
        None
    };

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
        let path_label = format!("\"{}\" → \"{}\"", sd.local_path, remote_display);

        // Count auto-excluded descendants + user exclusions for the badge.
        let auto_excl = auto_excluded_for(sd, all_known_sync_dirs);
        let custom_excl_count = exclusions.get(&sd.id).map(|v| v.len()).unwrap_or(0);
        let total_excl = auto_excl.len() + custom_excl_count;
        let excl_label = format!("Excluded ({})", total_excl);

        // Re-auth requested at the remote level wins over any per-dir
        // run-state — the engine can't make progress until the user
        // signs in again, so surface a red error icon on every card.
        let icon_state = if needs_reauth {
            Some(SyncDirRunState::Error)
        } else {
            status.get(&sd.id).copied()
        };

        // Top row: [icon] paths | [Excluded (n)] [Delete].
        // Wrap the path label in a Fill-width container so a long
        // `"local" → "remote"` string consumes the row's slack instead
        // of pushing the trailing buttons off the right edge. iced
        // wraps the text within the container's bounds.
        let top_row = row![
            status_icon(icon_state),
            container(text(path_label).size(13)).width(Length::Fill),
            button(text(excl_label).size(12)).on_press(Msg::ToggleExclusions(sd.id)),
            button(text("Delete").size(12)).on_press(Msg::DeleteSyncDir(
                sd.local_path.clone(),
                sd.remote_path.clone(),
            )),
        ]
        .spacing(ROW_SPACING)
        .align_items(Alignment::Center);

        // Log area: read-only multi-line editor stretched to the card
        // width with a small horizontal inset so its frame doesn't merge
        // with the card edge. Lines are capped at MAX_LOG_LINES upstream
        // to keep RAM bounded.
        let sd_id_for_editor = sd.id;
        let log_area: Element<'a, Msg> = match log.get(&sd.id) {
            Some(content) => container(
                text_editor(content)
                    .height(Length::Fixed(LOG_HEIGHT))
                    .padding(4)
                    .on_action(move |action| {
                        Msg::LogEditorAction(sd_id_for_editor, action)
                    }),
            )
            .padding([0, 6])
            .width(Length::Fill)
            .into(),
            None => container(Space::with_height(Length::Fixed(LOG_HEIGHT)))
                .padding([0, 6])
                .width(Length::Fill)
                .into(),
        };

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

    let mut page = column![header, Rule::horizontal(1)].spacing(SECTION_SPACING);
    if let Some(banner) = reauth_banner {
        page = page.push(banner);
    }
    page = page
        .push(sync_dirs_section)
        .push(Rule::horizontal(1))
        .push(settings_panel);

    container(page).padding(PAGE_PADDING).into()
}

/// Build the exclusion panel for one sync_dir card.
fn exclusion_panel_view<'a>(
    sd: &'a SyncDir,
    auto_excl: &[&'a SyncDir],
    custom_excl: &'a [SyncDirExclusion],
    draft: &'a str,
) -> Element<'a, Msg> {
    let mut col = column![].spacing(ROW_SPACING / 2);

    // Auto-excluded (descendant sync_dirs) — read-only. Show the
    // descendant's remote path so it lines up with what the provider
    // sees, not the local mirror.
    if !auto_excl.is_empty() {
        col = col.push(text("Auto-excluded:").size(12));
        for desc in auto_excl {
            let remote_relative = if sd.remote_path.is_empty() {
                desc.remote_path.as_str()
            } else {
                desc.remote_path
                    .strip_prefix(&format!("{}/", sd.remote_path))
                    .unwrap_or(&desc.remote_path)
            };
            col = col.push(text(format!("  \"{remote_relative}\"")).size(12));
        }
    }

    // User-defined exclusions — deletable.
    if !custom_excl.is_empty() {
        col = col.push(text("Custom excluded:").size(12));
        for excl in custom_excl {
            let excl_id = excl.id;
            let sd_id = sd.id;
            col = col.push(
                row![
                    text(format!("  \"{}\"", excl.remote_path)).size(12),
                    Space::with_width(Length::Fill),
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

/// Sync_dirs from `all` that are auto-excluded under `sd` — i.e. another
/// sync_dir on the same provider whose remote path is nested under
/// `sd.remote_path`. Local-tree overlaps are blocked at AddSyncDir time,
/// so the badge only needs to surface the remote-tree case here.
fn auto_excluded_for<'a>(sd: &SyncDir, all: &'a [SyncDir]) -> Vec<&'a SyncDir> {
    all.iter()
        .filter(|d| d.id != sd.id && d.remote_id == sd.remote_id)
        .filter(|d| is_remote_descendant(sd, d))
        .collect()
}

fn is_remote_descendant(ancestor: &SyncDir, candidate: &SyncDir) -> bool {
    if ancestor.remote_path.is_empty() {
        !candidate.remote_path.is_empty()
    } else {
        candidate
            .remote_path
            .starts_with(&format!("{}/", ancestor.remote_path))
    }
}

/// Render the per-card status icon, or a same-sized blank when there's
/// no run-state yet so the path label keeps the same horizontal offset
/// across cards.
fn status_icon<'a>(state: Option<SyncDirRunState>) -> Element<'a, Msg> {
    let placeholder = || -> Element<'a, Msg> {
        Space::with_width(Length::Fixed(STATUS_ICON_SIZE)).into()
    };
    match state {
        // MdiSync rendered blue while a pass is in flight.
        Some(SyncDirRunState::Syncing) => icon_svg(icondata::MdiSync, "#3b82f6"),
        // AiCheckCircleTwotone in green for a clean last pass.
        Some(SyncDirRunState::Synced) => icon_svg(icondata::AiCheckCircleTwotone, "#22c55e"),
        // AiWarningOutlined in amber for backoff / per-file errors / conflicts.
        Some(SyncDirRunState::Warning) => icon_svg(icondata::AiWarningOutlined, "#eab308"),
        // BiErrorAltRegular in red for hard failures (snapshot fail, re-auth needed).
        Some(SyncDirRunState::Error) => icon_svg(icondata::BiErrorAltRegular, "#ef4444"),
        None => placeholder(),
    }
}

/// Wrap an [`icondata::Icon`] (raw inner SVG path data plus a viewBox)
/// in a real `<svg>` document and hand it to iced's SVG widget. The
/// outer `fill` cascades into any `<path>` that doesn't set its own,
/// which gives us a one-call recolor for the monochrome icons we use
/// (the twotone variant still keeps its hard-coded secondary tone).
fn icon_svg<'a>(icon: icondata::Icon, color: &str) -> Element<'a, Msg> {
    let view_box = icon.view_box.unwrap_or("0 0 24 24");
    let svg_doc = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="{view_box}" fill="{color}">{data}</svg>"##,
        data = icon.data,
    );
    svg(svg::Handle::from_memory(svg_doc.into_bytes()))
        .width(Length::Fixed(STATUS_ICON_SIZE))
        .height(Length::Fixed(STATUS_ICON_SIZE))
        .into()
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
