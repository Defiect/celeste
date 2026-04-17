//! Two-choice interval picker — 5 s or 15 s. We dropped the custom-
//! seconds flow along with instant sync; the sync algorithm's listing
//! cost is what drives the lower bound here, and 5/15 covers both
//! "I want changes right away" and "don't hammer the API" without
//! bringing back the chatter from picking arbitrary values.

use iced::{
    widget::{pick_list, row, text},
    Element,
};

use crate::domain::remote::Interval;

impl std::fmt::Display for Interval {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            Interval::FiveSeconds => "5 s",
            Interval::FifteenSeconds => "15 s",
        };
        f.write_str(label)
    }
}

pub fn view<Msg: 'static + Clone>(
    current: Interval,
    on_change: impl Fn(Interval) -> Msg + 'static,
) -> Element<'static, Msg> {
    const ALL: [Interval; 2] = [Interval::FiveSeconds, Interval::FifteenSeconds];
    row![
        text("Interval:").size(14),
        pick_list(&ALL[..], Some(current), on_change),
    ]
    .spacing(8)
    .align_items(iced::Alignment::Center)
    .into()
}
