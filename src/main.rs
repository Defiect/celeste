pub mod app;
pub mod domain;
pub mod infrastructure;
pub mod screens;
pub mod services;
#[cfg(test)]
pub mod test_support;
pub mod theme;
pub mod util;
pub mod widgets;

use std::sync::Arc;

use sea_orm::Database;
use serde_json::json;

use crate::{
    app::run as iced_run,
    domain::ports::{RcloneClient, Repository},
    infrastructure::{
        persistence::{
            self,
            migrations::{Migrator, MigratorTrait},
            repository::SeaOrmRepository,
        },
        rclone::LibrcloneClient,
        stderr_capture,
    },
};

fn main() {
    // Tap stderr before librclone's Go runtime can grab it — that's the
    // only way to catch the `WARN[...] Too many requests` lines rclone's
    // backends emit when they silently retry a 429. Falls back to a no-op
    // if the platform can't hand us a pipe; sync keeps working, we just
    // lose rate-limit detection for the run.
    let _stderr = stderr_capture::install();

    // rclone config file lives next to our SQLite DB in ~/.config/celeste.
    let config_dir = util::get_config_dir();
    std::fs::create_dir_all(&config_dir).expect("failed to create config dir");
    let mut rclone_config = config_dir.clone();
    rclone_config.push("rclone.conf");
    librclone::initialize();
    // Phase 1 smoke: the rclone RPC surface and the native-Go
    // ProtonDrive surface share one Go runtime. Print the native
    // identity string once at startup to confirm linking. Doesn't hit
    // the network; later phases replace this with real Drive calls.
    eprintln!("celeste: native-go identity = {}", librclone::proton_drive_version());
    librclone::rpc(
        "config/setpath",
        json!({ "path": rclone_config }).to_string(),
    )
    .expect("failed to set rclone config path");

    let mut db_path = config_dir.clone();
    db_path.push("data.sqlite");
    if !db_path.exists() {
        std::fs::File::create(&db_path).expect("failed to create db file");
    }
    let db = util::await_future(Database::connect(format!(
        "sqlite://{}",
        db_path.display()
    )))
    .expect("failed to connect to the database");

    if util::await_future(persistence::has_legacy_migrations(&db)) {
        show_legacy_config_popup(&config_dir);
        std::process::exit(0);
    }

    util::await_future(Migrator::up(&db, None))
        .expect("failed to run database migrations");

    let repo: Arc<dyn Repository> = Arc::new(SeaOrmRepository::new(db));
    let rclone: Arc<dyn RcloneClient> = Arc::new(LibrcloneClient::new());
    iced_run(repo, rclone).expect("iced app exited with error");
}

fn show_legacy_config_popup(config_dir: &std::path::Path) {
    use iced::{
        widget::{button, column, text},
        window, Application, Command, Element, Length, Settings, Theme,
    };

    struct LegacyPopup {
        config_dir: String,
    }

    #[derive(Debug, Clone)]
    enum Msg {
        Ack,
    }

    impl Application for LegacyPopup {
        type Executor = iced::executor::Default;
        type Message = Msg;
        type Theme = Theme;
        type Flags = String;

        fn new(config_dir: String) -> (Self, Command<Msg>) {
            (Self { config_dir }, Command::none())
        }

        fn title(&self) -> String {
            "Celeste — outdated configuration".to_owned()
        }

        fn update(&mut self, _msg: Msg) -> Command<Msg> {
            window::close(window::Id::MAIN)
        }

        fn view(&self) -> Element<'_, Msg> {
            column![
                text("Outdated Celeste configuration detected").size(20),
                text(format!(
                    "The sync algorithm was rewritten and the database schema is no longer compatible.\n\nDelete the following directory and restart Celeste:\n\n  {}",
                    self.config_dir,
                ))
                .size(14),
                button(text("Close Celeste")).on_press(Msg::Ack),
            ]
            .spacing(16)
            .padding(24)
            .max_width(560)
            .width(Length::Fill)
            .into()
        }
    }

    let _ = LegacyPopup::run(Settings::with_flags(config_dir.display().to_string()));
}
