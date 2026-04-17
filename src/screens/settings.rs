//! Per-remote "Sync Settings" panel (Enabled + interval).

use iced::{
    widget::{checkbox, column, row, text},
    Element,
};

use crate::{
    domain::remote::{Interval, Remote, SyncPolicy},
    theme::{ROW_SPACING, SECTION_SPACING},
    widgets::duration_picker,
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
    let interval = duration_picker::view(remote.policy.interval, Msg::IntervalChanged);

    column![
        heading,
        row![enabled].spacing(ROW_SPACING * 2),
        interval,
    ]
    .spacing(SECTION_SPACING)
    .into()
}
