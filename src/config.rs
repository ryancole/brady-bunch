use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const DEFAULT_FILE: &str = "brady.toml";

/// How a child is asked to exit when the host shuts down.
#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Shutdown {
    /// Console apps: Ctrl+C, then close the console window. GUI apps: WM_CLOSE.
    #[default]
    Auto,
    /// Ctrl+C only.
    CtrlC,
    /// WM_CLOSE to the app's windows only.
    Close,
    /// Terminate immediately.
    Kill,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_title")]
    pub title: String,
    /// How long each shutdown step waits for the child to exit before escalating.
    #[serde(default = "default_timeout")]
    pub shutdown_timeout_ms: u64,
    /// Fixed column count; defaults to ceil(sqrt(n)).
    pub columns: Option<u32>,
    #[serde(default)]
    pub maximized: bool,
    #[serde(rename = "app", default)]
    pub apps: Vec<AppConfig>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    /// Label shown above the tile; defaults to the executable name.
    pub name: Option<String>,
    /// Full command line, e.g. `ping -t localhost`.
    pub command: String,
    pub cwd: Option<String>,
    /// Force console / GUI handling; auto-detected from the executable when omitted.
    pub console: Option<bool>,
    #[serde(default)]
    pub shutdown: Shutdown,
    pub shutdown_timeout_ms: Option<u64>,
}

fn default_title() -> String {
    "Brady Bunch".into()
}

fn default_timeout() -> u64 {
    5000
}

/// Explicit path, else `brady.toml` in the working directory, else next to the exe.
pub fn resolve_path(arg: Option<PathBuf>) -> PathBuf {
    if let Some(p) = arg {
        return p;
    }
    let local = PathBuf::from(DEFAULT_FILE);
    if local.exists() {
        return local;
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join(DEFAULT_FILE)))
        .unwrap_or(local)
}

pub fn load(path: &Path) -> Result<Config, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("Could not read config {}: {e}", path.display()))?;
    let cfg: Config =
        toml::from_str(&text).map_err(|e| format!("Invalid config {}:\n\n{e}", path.display()))?;
    if cfg.apps.is_empty() {
        return Err(format!("{} has no [[app]] entries.", path.display()));
    }
    Ok(cfg)
}
