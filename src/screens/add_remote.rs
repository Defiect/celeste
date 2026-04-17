//! WebDAV-family remote provisioning. OAuth providers still go through
//! the GTK login flow — this screen only handles raw-credential ones
//! (WebDAV / Nextcloud / Owncloud).

use iced::{
    widget::{button, column, pick_list, row, text, text_input, Space},
    Element, Length,
};

use crate::{
    services::auth_service::WebDavVendor,
    theme::{PAGE_PADDING, ROW_SPACING, SECTION_SPACING},
};

#[derive(Debug, Clone)]
pub enum Msg {
    NameChanged(String),
    UrlChanged(String),
    UserChanged(String),
    PassChanged(String),
    VendorChanged(VendorChoice),
    Submit,
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VendorChoice {
    WebDav,
    Nextcloud,
    Owncloud,
}

impl VendorChoice {
    pub const ALL: [VendorChoice; 3] = [
        VendorChoice::WebDav,
        VendorChoice::Nextcloud,
        VendorChoice::Owncloud,
    ];

    pub fn as_vendor(self) -> WebDavVendor {
        match self {
            VendorChoice::WebDav => WebDavVendor::WebDav,
            VendorChoice::Nextcloud => WebDavVendor::Nextcloud,
            VendorChoice::Owncloud => WebDavVendor::Owncloud,
        }
    }
}

impl std::fmt::Display for VendorChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            VendorChoice::WebDav => "Generic WebDAV",
            VendorChoice::Nextcloud => "Nextcloud",
            VendorChoice::Owncloud => "Owncloud",
        };
        f.write_str(label)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Draft {
    pub name: String,
    pub url: String,
    pub user: String,
    pub pass: String,
    pub vendor: Option<VendorChoice>,
    pub error: Option<String>,
}

pub fn view(draft: &Draft) -> Element<'_, Msg> {
    let heading = text("Add WebDAV remote").size(22);
    let subtitle = text(
        "OAuth providers (Dropbox, Google Drive, pCloud) are not yet supported here — run `celeste run-gui` for those."
    ).size(12);

    fn label(l: &'static str) -> Element<'static, Msg> {
        text(l).width(Length::Fixed(110.0)).size(13).into()
    }

    let name_row = row![
        label("Name"),
        text_input("My Nextcloud", &draft.name).on_input(Msg::NameChanged).padding(6),
    ]
    .align_items(iced::Alignment::Center)
    .spacing(8);
    let vendor_row = row![
        label("Type"),
        pick_list(&VendorChoice::ALL[..], draft.vendor, Msg::VendorChanged)
            .placeholder("Pick a type"),
    ]
    .align_items(iced::Alignment::Center)
    .spacing(8);
    let url_row = row![
        label("URL"),
        text_input("https://cloud.example.org", &draft.url)
            .on_input(Msg::UrlChanged)
            .padding(6),
    ]
    .align_items(iced::Alignment::Center)
    .spacing(8);
    let user_row = row![
        label("Username"),
        text_input("username", &draft.user).on_input(Msg::UserChanged).padding(6),
    ]
    .align_items(iced::Alignment::Center)
    .spacing(8);
    let pass_row = row![
        label("Password"),
        text_input("password", &draft.pass)
            .secure(true)
            .on_input(Msg::PassChanged)
            .padding(6),
    ]
    .align_items(iced::Alignment::Center)
    .spacing(8);

    let actions = row![
        Space::with_width(Length::Fill),
        button(text("Cancel")).on_press(Msg::Cancel),
        button(text("Add")).on_press(Msg::Submit),
    ]
    .spacing(ROW_SPACING);

    let mut body = column![heading, subtitle, name_row, vendor_row, url_row, user_row, pass_row]
        .spacing(SECTION_SPACING);

    if let Some(err) = &draft.error {
        body = body.push(text(format!("⚠ {err}")).size(13));
    }

    body = body.push(actions);

    iced::widget::container(body).padding(PAGE_PADDING).into()
}
