//! Per-remote "Sync Settings" panel (Enabled / Instant sync / Interval).

use iced::{
    widget::{checkbox, column, row, text},
    Element,
};

use crate::{
    domain::remote::{Remote, SyncPolicy},
    theme::{ROW_SPACING, SECTION_SPACING},
    widgets::duration_picker,
};

#[derive(Debug, Clone)]
pub enum Msg {
    EnabledToggled(bool),
    InstantSyncToggled(bool),
    IntervalChanged(u64),
}

/// Turn a Msg back into the full updated SyncPolicy. The caller passes the
/// current policy; we only mutate the field the message concerns.
pub fn policy_from(msg: &Msg, current: &SyncPolicy) -> SyncPolicy {
    let mut policy = current.clone();
    match msg {
        Msg::EnabledToggled(v) => policy.enabled = *v,
        Msg::InstantSyncToggled(v) => policy.instant_sync = *v,
        Msg::IntervalChanged(secs) => {
            policy.interval = std::time::Duration::from_secs(*secs);
        }
    }
    policy
}

pub fn view(remote: &Remote) -> Element<'_, Msg> {
    let heading = text("Sync Settings").size(18);

    let enabled = checkbox("Enabled", remote.policy.enabled).on_toggle(Msg::EnabledToggled);
    let instant = checkbox("Instant sync", remote.policy.instant_sync)
        .on_toggle(Msg::InstantSyncToggled);

    let interval_secs = remote.policy.interval.as_secs();
    let interval = duration_picker::view(interval_secs, Msg::IntervalChanged);

    column![
        heading,
        row![enabled, instant].spacing(ROW_SPACING * 2),
        interval,
    ]
    .spacing(SECTION_SPACING)
    .into()
}
