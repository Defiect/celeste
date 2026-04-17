//! Per-remote "Sync Settings" panel (Enabled + interval + hoverable
//! warning for provider-specific rate-limit tripwires).

use iced::{
    widget::{checkbox, column, row, tooltip, Space},
    Element, Length,
};

use crate::{
    domain::remote::{Interval, Remote, SyncPolicy},
    theme::{ROW_SPACING, SECTION_SPACING},
    widgets::{duration_picker, text},
};

#[derive(Debug, Clone)]
pub enum Msg {
    EnabledToggled(bool),
    IntervalChanged(Interval),
}

/// Turn a Msg back into the full updated SyncPolicy. The caller passes the
/// current policy; we only mutate the field the message concerns.
pub fn policy_from(msg: &Msg, current: &SyncPolicy) -> SyncPolicy {
    let mut policy = current.clone();
    match msg {
        Msg::EnabledToggled(v) => policy.enabled = *v,
        Msg::IntervalChanged(i) => policy.interval = *i,
    }
    policy
}

pub fn view(remote: &Remote) -> Element<'_, Msg> {
    let heading = text("Sync Settings").size(18);
    let enabled = checkbox("Enabled", remote.policy.enabled).on_toggle(Msg::EnabledToggled);

    let warn_below = remote
        .provider_kind
        .and_then(|k| k.short_interval_threshold());
    let warning = remote
        .provider_kind
        .and_then(|k| k.short_interval_warning());

    let picker = duration_picker::view(
        remote.policy.interval,
        warn_below,
        Msg::IntervalChanged,
    );

    // When the provider has a short-interval warning, append a hoverable
    // ⚠ next to the picker — the options themselves already show the
    // glyph per-item, this gives the user somewhere to hover for the
    // full explanation.
    let picker_row: Element<'_, Msg> = if let Some(msg) = warning {
        row![
            picker,
            Space::with_width(Length::Fixed(8.0)),
            tooltip(
                text("⚠").size(16),
                text(msg).size(12),
                tooltip::Position::Right,
            )
            .gap(8)
            .padding(8),
        ]
        .align_items(iced::Alignment::Center)
        .into()
    } else {
        picker
    };

    column![
        heading,
        row![enabled].spacing(ROW_SPACING * 2),
        picker_row,
    ]
    .spacing(SECTION_SPACING)
    .into()
}
