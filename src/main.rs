#![feature(exit_status_error)]

pub mod about;
pub mod app;
pub mod domain;
pub mod gtk_util;
pub mod infrastructure;
pub mod launch;
pub mod mpsc;
pub mod pending_fs_events;
pub mod screens;
pub mod services;
pub mod theme;
pub mod traits;
pub mod util;
pub mod widgets;

use adw::{
    gtk::{self, gdk::Display, Align, Box, CssProvider, Label, Orientation, StyleContext},
    prelude::*,
    Application, ApplicationWindow, HeaderBar,
};
use clap::{Parser, Subcommand};
use serde_json::json;
use std::{
    env,
    io::{BufRead, BufReader},
    process::{Command, Stdio},
    thread,
};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Whether to start in the background.
    #[arg(long)]
    background: bool,
}

#[derive(Subcommand)]
enum Commands {
    RunGui {
        /// Whether to start in the background.
        #[arg(long)]
        background: bool,
    },
    /// Launch the new Iced-based UI (Phase D, work in progress).
    Iced,
}

fn main() {
    // Initialize GTK.
    gtk::init().unwrap();

    // Configure Rclone.
    let mut config = util::get_config_dir();
    config.push("rclone.conf");
    librclone::initialize();
    librclone::rpc("config/setpath", json!({ "path": config }).to_string()).unwrap();

    // Load our CSS.
    let provider = CssProvider::new();
    provider.load_from_data(include_bytes!(concat!(env!("OUT_DIR"), "/style.css")));

    StyleContext::add_provider_for_display(
        &Display::default().unwrap(),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    // Get the application.
    let app = Application::builder().application_id(util::APP_ID).build();

    // Due to GTK working in Rust via Rust's FFI, panics don't appear to be able to
    // be captured (this hasn't been confirmed behavior, it's just what I've
    // observed). Panics would like to be captured when they're encountered though,
    // so we relaunch this program in a subprocess and capture any errors from
    // there.
    let cli = Cli::parse();
    if let Some(cmd) = cli.command {
        match cmd {
            Commands::RunGui { background } => {
                // Start up the application.
                app.connect_activate(move |app| {
                    if app.is_remote() {
                        app.activate();
                        return;
                    }

                    let windows = app.windows();
                    if windows.is_empty() {
                        launch::launch(app, background);
                    } else {
                        windows.iter().for_each(|window| window.show());
                    }
                });

                app.run_with_args::<&str>(&[]);
            }
            Commands::Iced => {
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
                    util,
                };
                use sea_orm::Database;
                use std::sync::Arc;

                let mut db_path = util::get_config_dir();
                std::fs::create_dir_all(&db_path).unwrap();
                db_path.push("data.sqlite");
                if !db_path.exists() {
                    std::fs::File::create(&db_path).unwrap();
                }

                let db = util::await_future(Database::connect(format!(
                    "sqlite://{}",
                    db_path.display()
                )))
                .expect("failed to connect to the database");
                util::await_future(Migrator::up(&db, None))
                    .expect("failed to run database migrations");

                // fs_watcher pushes matched remote_ids into a global the
                // Iced subscription polls.
                let on_change: Arc<dyn Fn(i32) + Send + Sync> =
                    Arc::new(|id| crate::pending_fs_events::push(id));
                fs_watcher::spawn_with_callback(db.clone(), on_change);

                let repo: Arc<dyn Repository> = Arc::new(SeaOrmRepository::new(db));
                let rclone: Arc<dyn RcloneClient> = Arc::new(LibrcloneClient::new());

                iced_run(repo, rclone).expect("iced app exited with error");
            }
        }
    } else {
        // Set `RUST_BACKTRACE` so we get a better backtrace for reporting.
        unsafe {
            env::set_var("RUST_BACKTRACE", "1");
        }

        // Run the command and get the stderr, checking for a backtrace.
        let mut args = vec!["run-gui"];
        if cli.background {
            args.push("--background");
        }

        let mut command = Command::new(env::args().next().unwrap())
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout_thread = thread::spawn(move || {
            let mut stdout = String::new();
            let mut stdout_handle = command.stdout.as_mut().unwrap();
            let stdout_reader = BufReader::new(&mut stdout_handle);

            for line in stdout_reader.lines() {
                let unwrapped_line = line.unwrap();
                println!("{unwrapped_line}");
                stdout.push_str(&unwrapped_line);
                stdout.push('\n');
            }

            stdout
        });
        let stderr_thread = thread::spawn(move || {
            let mut stderr = String::new();
            let mut stderr_handle = command.stderr.as_mut().unwrap();
            let stderr_reader = BufReader::new(&mut stderr_handle);

            for line in stderr_reader.lines() {
                let unwrapped_line = line.unwrap();
                eprintln!("{unwrapped_line}");
                stderr.push_str(&unwrapped_line);
                stderr.push('\n');
            }

            stderr
        });
        let _stdout = stdout_thread.join().unwrap();
        let stderr = stderr_thread.join().unwrap();

        let maybe_backtrace = {
            let mut backtrace = String::new();
            let mut backtrace_found = false;

            for line in stderr.lines() {
                if backtrace_found && !line.contains("note: Some details are omitted") {
                    backtrace.push_str(line);
                    backtrace.push('\n');
                } else if line.starts_with("thread 'main' panicked at") {
                    backtrace.push_str(line);
                    backtrace.push('\n');
                    backtrace_found = true;
                }
            }

            backtrace.pop(); // The extra newline at the end.

            if backtrace_found {
                Some(backtrace)
            } else {
                None
            }
        };

        // Show the backtrace in the GUI if one was found.
        if let Some(backtrace) = maybe_backtrace {
            app.connect_activate(move |app| {
                let window = ApplicationWindow::builder()
                    .application(app)
                    .title(&util::get_title!("Unknown Error"))
                    .build();
                window.add_css_class("celeste-global-padding");
                let sections = Box::builder()
                    .orientation(Orientation::Vertical)
                    .build();
                sections.append(&HeaderBar::new());
                let error_label = Label::builder()
                    .label(&tr::tr!("Unknown Error"))
                    .halign(Align::Start)
                    .css_classes(vec!["heading".to_owned()])
                    .build();
                sections.append(&error_label);

                let error_text = Label::builder()
                    .label(&tr::tr!("An unknown error has occurred while running. This is an internal issue with Celeste and should be reported.\n\nThe following backtrace may help with debugging the issue - note that it may contain information such as login tokens/keys, so avoid posting the information publicly:"))
                    .halign(Align::Start)
                    .wrap(true)
                    .xalign(0.0)
                    .yalign(0.0)
                    .build();
                sections.append(&error_text);
                sections.append(&gtk_util::codeblock(&backtrace));

                window.set_content(Some(&sections));
                window.show();
            });

            app.run_with_args::<&str>(&[]);
        }
    }
}
