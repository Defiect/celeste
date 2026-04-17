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
    PolicySaved,
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
                // TODO: hook into SyncOrchestrator once it drives the sync.
                Command::none()
            }
            Message::Main(main_page::Msg::AddRemote) => {
                // TODO: invoke the login flow.
                Command::none()
            }
            Message::Remote(remote_page::Msg::Back) => {
                self.selected = None;
                Command::none()
            }
            Message::Remote(remote_page::Msg::RefreshNow(_id)) => {
                // TODO: push into REFRESH_REQUESTS-equivalent once the Iced
                // side owns the orchestrator.
                Command::none()
            }
            Message::Remote(remote_page::Msg::Settings(sub))
            | Message::Settings(sub) => {
                let Some(id) = self.selected else {
                    return Command::none();
                };
                let Some(remote) = self.remotes.iter_mut().find(|r| r.id == id) else {
                    return Command::none();
                };
                let new_policy = settings::policy_from(&sub, &remote.policy);
                remote.policy = new_policy.clone();
                let repo = self.repo.clone();
                Command::perform(
                    async move {
                        let _ = repo.set_policy(id, new_policy).await;
                    },
                    |_| Message::PolicySaved,
                )
            }
            Message::PolicySaved => Command::none(),
        }
    }

    fn view(&self) -> Element<'_, Message> {
        match self
            .selected
            .and_then(|id| self.remotes.iter().find(|r| r.id == id))
        {
            Some(remote) => remote_page::view(remote).map(Message::Remote),
            None => main_page::view(&self.remotes, self.selected).map(Message::Main),
        }
    }
}

/// Launch the Iced application. Blocks until the window closes.
pub fn run(repo: Arc<dyn Repository>, rclone: Arc<dyn RcloneClient>) -> iced::Result {
    let settings = Settings::with_flags(Flags { repo, rclone });
    CelesteApp::run(settings)
}
