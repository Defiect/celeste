//! Interval picker — presents the full set of supported [`Interval`]
//! choices. The pick_list shows the current selection on its own, so
//! there's no separate label.

use iced::{widget::pick_list, Element};

use crate::domain::remote::Interval;

impl std::fmt::Display for Interval {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            Interval::FiveSeconds => "5 s",
            Interval::FifteenSeconds => "15 s",
            Interval::ThirtySeconds => "30 s",
            Interval::OneMinute => "1 min",
            Interval::FiveMinutes => "5 min",
            Interval::FifteenMinutes => "15 min",
            Interval::ThirtyMinutes => "30 min",
            Interval::OneHour => "1 hour",
        };
        f.write_str(label)
    }
}

pub fn view<Msg: 'static + Clone>(
    current: Interval,
    on_change: impl Fn(Interval) -> Msg + 'static,
) -> Element<'static, Msg> {
    pick_list(&Interval::ALL[..], Some(current), on_change).into()
}
