//! Iced application root. The Phase D entry point alongside the existing
//! GTK `launch::launch`. Runs the pure-Rust UI against the already-extracted
//! service layer.

use std::sync::Arc;

use iced::{executor, Application, Command, Element, Settings, Theme};

use crate::{
    domain::{
        ports::{RcloneClient, Repository},
        remote::{Remote, RemoteId},
    },
    screens::{main_page, remote_page, settings},
    theme,
};

/// Messages the root application dispatches. Screen-level messages are
/// wrapped by variants; service results fire their own.
#[derive(Debug, Clone)]
pub enum Message {
    Main(main_page::Msg),
    Remote(remote_page::Msg),
    Settings(settings::Msg),
    RemotesLoaded(Vec<Remote>),
}

pub struct CelesteApp {
    #[allow(dead_code)]
    repo: Arc<dyn Repository>,
    #[allow(dead_code)]
    rclone: Arc<dyn RcloneClient>,
    remotes: Vec<Remote>,
    selected: Option<RemoteId>,
}

pub struct Flags {
    pub repo: Arc<dyn Repository>,
    pub rclone: Arc<dyn RcloneClient>,
}

impl Application for CelesteApp {
    type Executor = executor::Default;
    type Message = Message;
    type Theme = Theme;
    type Flags = Flags;

    fn new(flags: Flags) -> (Self, Command<Message>) {
        let state = Self {
            repo: flags.repo.clone(),
            rclone: flags.rclone,
            remotes: Vec::new(),
            selected: None,
        };
        let repo = flags.repo;
        let load = Command::perform(
            async move { repo.list_remotes().await.unwrap_or_default() },
            Message::RemotesLoaded,
        );
        (state, load)
    }

    fn title(&self) -> String {
        "Celeste".to_string()
    }

    fn theme(&self) -> Theme {
        theme::celeste_theme()
    }

    fn update(&mut self, message: Message) -> Command<Message> {
        match message {
            Message::RemotesLoaded(remotes) => {
                self.remotes = remotes;
                Command::none()
            }
            Message::Main(main_page::Msg::Selected(id)) => {
                self.selected = Some(id);
                Command::none()
            }
            Message::Main(main_page::Msg::RefreshAll) => {
                // TODO: wire through a future SyncOrchestrator method.
                Command::none()
            }
            Message::Main(main_page::Msg::AddRemote) => {
                // TODO: invoke the login flow.
                Command::none()
            }
            Message::Remote(_) | Message::Settings(_) => Command::none(),
        }
    }

    fn view(&self) -> Element<'_, Message> {
        main_page::view(&self.remotes, self.selected).map(Message::Main)
    }
}

/// Launch the Iced application. Blocks until the window closes.
pub fn run(repo: Arc<dyn Repository>, rclone: Arc<dyn RcloneClient>) -> iced::Result {
    let settings = Settings::with_flags(Flags { repo, rclone });
    CelesteApp::run(settings)
}
