//! Add-remote flow for every provider Celeste supports:
//!
//! - WebDAV / Nextcloud / Owncloud — raw username + password
//! - Proton Drive — username + password + optional TOTP
//! - Dropbox / Google Drive / pCloud — OAuth2 via `rclone authorize`
//!   (launches the default browser; the user confirms, rclone prints
//!   the token, we pass it to config/create)

use iced::{
    widget::{button, column, container, pick_list, row, text, text_input, Space},
    Element, Length,
};

use crate::{
    services::auth_service::{OAuthProvider, WebDavVendor},
    theme::{PAGE_PADDING, ROW_SPACING, SECTION_SPACING},
};

#[derive(Debug, Clone)]
pub enum Msg {
    NameChanged(String),
    ProviderChanged(ProviderKind),
    UrlChanged(String),
    UserChanged(String),
    PassChanged(String),
    TotpChanged(String),
    ClientIdChanged(String),
    ClientSecretChanged(String),
    Submit,
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderKind {
    WebDav,
    Nextcloud,
    Owncloud,
    ProtonDrive,
    Dropbox,
    GDrive,
    PCloud,
}

impl ProviderKind {
    pub const ALL: [ProviderKind; 7] = [
        ProviderKind::WebDav,
        ProviderKind::Nextcloud,
        ProviderKind::Owncloud,
        ProviderKind::ProtonDrive,
        ProviderKind::Dropbox,
        ProviderKind::GDrive,
        ProviderKind::PCloud,
    ];

    pub fn webdav_vendor(self) -> Option<WebDavVendor> {
        match self {
            ProviderKind::WebDav => Some(WebDavVendor::WebDav),
            ProviderKind::Nextcloud => Some(WebDavVendor::Nextcloud),
            ProviderKind::Owncloud => Some(WebDavVendor::Owncloud),
            _ => None,
        }
    }

    pub fn oauth_provider(self) -> Option<OAuthProvider> {
        match self {
            ProviderKind::Dropbox => Some(OAuthProvider::Dropbox),
            ProviderKind::GDrive => Some(OAuthProvider::GDrive),
            ProviderKind::PCloud => Some(OAuthProvider::PCloud),
            _ => None,
        }
    }

    pub fn is_webdav_family(self) -> bool {
        self.webdav_vendor().is_some()
    }

    pub fn is_oauth(self) -> bool {
        self.oauth_provider().is_some()
    }

    pub fn is_proton_drive(self) -> bool {
        matches!(self, ProviderKind::ProtonDrive)
    }
}

impl std::fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            ProviderKind::WebDav => "Generic WebDAV",
            ProviderKind::Nextcloud => "Nextcloud",
            ProviderKind::Owncloud => "Owncloud",
            ProviderKind::ProtonDrive => "Proton Drive",
            ProviderKind::Dropbox => "Dropbox",
            ProviderKind::GDrive => "Google Drive",
            ProviderKind::PCloud => "pCloud",
        };
        f.write_str(label)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Draft {
    pub name: String,
    pub provider: Option<ProviderKind>,
    // WebDAV-family:
    pub url: String,
    // WebDAV + Proton Drive:
    pub user: String,
    pub pass: String,
    // Proton Drive:
    pub totp: String,
    // OAuth:
    pub client_id: String,
    pub client_secret: String,

    pub error: Option<String>,
    /// True while `rclone authorize` is running (or the blocking
    /// WebDAV validation is in flight). Disables the Submit button
    /// and swaps the header for a "please wait" hint.
    pub busy: bool,
}

fn field_label(l: &'static str) -> Element<'static, Msg> {
    text(l).width(Length::Fixed(130.0)).size(13).into()
}

pub fn view(draft: &Draft) -> Element<'_, Msg> {
    let heading = text("Add remote").size(22);

    let name_row = row![
        field_label("Name"),
        text_input("My remote", &draft.name)
            .on_input(Msg::NameChanged)
            .padding(6),
    ]
    .align_items(iced::Alignment::Center)
    .spacing(8);

    let provider_row = row![
        field_label("Type"),
        pick_list(&ProviderKind::ALL[..], draft.provider, Msg::ProviderChanged)
            .placeholder("Pick a provider"),
    ]
    .align_items(iced::Alignment::Center)
    .spacing(8);

    let mut body = column![heading, name_row, provider_row].spacing(SECTION_SPACING);

    match draft.provider {
        Some(p) if p.is_webdav_family() => {
            body = body.push(
                row![
                    field_label("URL"),
                    text_input("https://cloud.example.org", &draft.url)
                        .on_input(Msg::UrlChanged)
                        .padding(6),
                ]
                .align_items(iced::Alignment::Center)
                .spacing(8),
            );
            body = body.push(
                row![
                    field_label("Username"),
                    text_input("username", &draft.user)
                        .on_input(Msg::UserChanged)
                        .padding(6),
                ]
                .align_items(iced::Alignment::Center)
                .spacing(8),
            );
            body = body.push(
                row![
                    field_label("Password"),
                    text_input("password", &draft.pass)
                        .secure(true)
                        .on_input(Msg::PassChanged)
                        .padding(6),
                ]
                .align_items(iced::Alignment::Center)
                .spacing(8),
            );
        }
        Some(p) if p.is_proton_drive() => {
            body = body.push(
                text(
                    "Note: if you have 2FA enabled, Proton's tokens expire \
                     periodically. When that happens rclone can't re-auth \
                     (the 2FA code is one-time-use) and you'll need to \
                     delete and re-add the remote with a fresh code."
                )
                .size(12),
            );
            body = body.push(
                row![
                    field_label("Username"),
                    text_input("username", &draft.user)
                        .on_input(Msg::UserChanged)
                        .padding(6),
                ]
                .align_items(iced::Alignment::Center)
                .spacing(8),
            );
            body = body.push(
                row![
                    field_label("Password"),
                    text_input("password", &draft.pass)
                        .secure(true)
                        .on_input(Msg::PassChanged)
                        .padding(6),
                ]
                .align_items(iced::Alignment::Center)
                .spacing(8),
            );
            body = body.push(
                row![
                    field_label("2FA code"),
                    text_input("(optional)", &draft.totp)
                        .on_input(Msg::TotpChanged)
                        .padding(6),
                ]
                .align_items(iced::Alignment::Center)
                .spacing(8),
            );
        }
        Some(p) if p.is_oauth() => {
            body = body.push(
                text("Clicking Connect opens your default browser for authorization. Client ID / Secret are optional — leave blank to use rclone's built-in defaults.")
                    .size(12),
            );
            body = body.push(
                row![
                    field_label("Client ID"),
                    text_input("(optional)", &draft.client_id)
                        .on_input(Msg::ClientIdChanged)
                        .padding(6),
                ]
                .align_items(iced::Alignment::Center)
                .spacing(8),
            );
            body = body.push(
                row![
                    field_label("Client secret"),
                    text_input("(optional)", &draft.client_secret)
                        .secure(true)
                        .on_input(Msg::ClientSecretChanged)
                        .padding(6),
                ]
                .align_items(iced::Alignment::Center)
                .spacing(8),
            );
        }
        Some(_) | None => {}
    }

    if draft.busy {
        body = body.push(
            text("Waiting for authorization — complete the flow in your browser…")
                .size(13),
        );
    }

    if let Some(err) = &draft.error {
        body = body.push(text(format!("⚠ {err}")).size(13));
    }

    let submit_label = match draft.provider {
        Some(p) if p.is_oauth() => "Connect",
        _ => "Add",
    };
    let submit_btn = {
        let b = button(text(submit_label));
        if draft.busy {
            b
        } else {
            b.on_press(Msg::Submit)
        }
    };

    let cancel_btn = {
        let b = button(text("Cancel"));
        if draft.busy {
            b
        } else {
            b.on_press(Msg::Cancel)
        }
    };

    body = body.push(
        row![Space::with_width(Length::Fill), cancel_btn, submit_btn].spacing(ROW_SPACING),
    );

    container(body).padding(PAGE_PADDING).into()
}
