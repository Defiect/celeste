pub mod app;
pub mod domain;
pub mod infrastructure;
pub mod pending_fs_events;
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
        fs_watcher,
        persistence::{
            migrations::{Migrator, MigratorTrait},
            repository::SeaOrmRepository,
        },
        rclone::LibrcloneClient,
    },
};

fn main() {
    // rclone config file lives next to our SQLite DB in ~/.config/celeste.
    let config_dir = util::get_config_dir();
    std::fs::create_dir_all(&config_dir).expect("failed to create config dir");
    let mut rclone_config = config_dir.clone();
    rclone_config.push("rclone.conf");
    librclone::initialize();
    librclone::rpc(
        "config/setpath",
        json!({ "path": rclone_config }).to_string(),
    )
    .expect("failed to set rclone config path");

    let mut db_path = config_dir;
    db_path.push("data.sqlite");
    if !db_path.exists() {
        std::fs::File::create(&db_path).expect("failed to create db file");
    }
    let db = util::await_future(Database::connect(format!(
        "sqlite://{}",
        db_path.display()
    )))
    .expect("failed to connect to the database");
    util::await_future(Migrator::up(&db, None))
        .expect("failed to run database migrations");

    // fs_watcher pushes matched (remote_id, paths) into a slot the Iced
    // subscription polls.
    let on_change: Arc<dyn Fn(i32, Vec<std::path::PathBuf>) + Send + Sync> =
        Arc::new(pending_fs_events::push);
    fs_watcher::spawn_with_callback(db.clone(), on_change);

    let repo: Arc<dyn Repository> = Arc::new(SeaOrmRepository::new(db));
    let rclone: Arc<dyn RcloneClient> = Arc::new(LibrcloneClient::new());
    iced_run(repo, rclone).expect("iced app exited with error");
}
