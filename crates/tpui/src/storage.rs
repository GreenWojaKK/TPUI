//! Per-user folder where an app keeps its own files, apart from the data it shows.

use std::path::PathBuf;

/// `%LOCALAPPDATA%\<app>` on Windows. Elsewhere `$XDG_STATE_HOME/<app>`, or `~/.local/state/<app>`, with the app name
/// in lowercase. `None` when the environment names no such place.
pub fn app_dir(app: &str) -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("LOCALAPPDATA")
            .filter(|base| !base.is_empty())
            .map(|base| PathBuf::from(base).join(app))
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::env::var_os("XDG_STATE_HOME")
            .filter(|base| !base.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .filter(|home| !home.is_empty())
                    .map(|home| PathBuf::from(home).join(".local").join("state"))
            })
            .map(|base| base.join(app.to_lowercase()))
    }
}
