use std::path::PathBuf;

use futures::future::Future;

/// Block the current thread on a future. Safe from any thread;
/// `block_on` has no main-context requirements.
pub fn await_future<F: Future>(future: F) -> F::Output {
    futures::executor::block_on(future)
}

/// Replace the user's home-directory prefix with `~` for friendlier UI
/// paths. Falls back to the raw path if `$HOME` isn't set.
pub fn fmt_home(dir: &str) -> String {
    let Some(home) = std::env::var_os("HOME") else {
        return dir.to_string();
    };
    let home = home.into_string().unwrap_or_default();
    match dir.strip_prefix(&home) {
        Some(rest) => "~".to_string() + rest,
        None => dir.to_string(),
    }
}

/// `${XDG_CONFIG_HOME:-$HOME/.config}/celeste`.
pub fn get_config_dir() -> PathBuf {
    let mut base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(path) if !path.is_empty() => PathBuf::from(path),
        _ => {
            let mut home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
            home.push(".config");
            home
        }
    };
    base.push("celeste");
    base
}

/// Trim at most one leading and one trailing slash.
pub fn strip_slashes(string: &str) -> String {
    let stripped_prefix = string.strip_prefix('/').unwrap_or(string);
    stripped_prefix
        .strip_suffix('/')
        .unwrap_or(stripped_prefix)
        .to_string()
}
