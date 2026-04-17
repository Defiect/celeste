//! Interval picker for the Sync Settings panel. Presets plus a raw seconds
//! entry for custom values.

use iced::{
    widget::{pick_list, row, text},
    Element,
};

/// Named presets the user can pick without typing a number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    ThirtySeconds,
    OneMinute,
    FiveMinutes,
    FifteenMinutes,
    OneHour,
    HalfHour,
}

impl Preset {
    pub const ALL: [Preset; 6] = [
        Preset::ThirtySeconds,
        Preset::OneMinute,
        Preset::FiveMinutes,
        Preset::FifteenMinutes,
        Preset::HalfHour,
        Preset::OneHour,
    ];

    pub fn seconds(self) -> u64 {
        match self {
            Preset::ThirtySeconds => 30,
            Preset::OneMinute => 60,
            Preset::FiveMinutes => 300,
            Preset::FifteenMinutes => 900,
            Preset::HalfHour => 1_800,
            Preset::OneHour => 3_600,
        }
    }

    pub fn from_seconds(secs: u64) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.seconds() == secs)
    }
}

impl std::fmt::Display for Preset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            Preset::ThirtySeconds => "30 s",
            Preset::OneMinute => "1 min",
            Preset::FiveMinutes => "5 min",
            Preset::FifteenMinutes => "15 min",
            Preset::HalfHour => "30 min",
            Preset::OneHour => "1 hour",
        };
        f.write_str(label)
    }
}

pub fn view<Msg: 'static + Clone>(
    current_secs: u64,
    on_change: impl Fn(u64) -> Msg + 'static,
) -> Element<'static, Msg> {
    let selected = Preset::from_seconds(current_secs);
    let label = text(format!("Interval: {} s", current_secs)).size(14);
    row![
        label,
        pick_list(&Preset::ALL[..], selected, move |p| on_change(p.seconds()))
            .placeholder("Custom"),
    ]
    .spacing(8)
    .align_items(iced::Alignment::Center)
    .into()
}
