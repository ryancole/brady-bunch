use serde::Deserialize;
use std::collections::BTreeMap;
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

/// Whether a child is relaunched after it exits on its own.
#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Restart {
    #[default]
    Never,
    /// Relaunch after a non-zero exit code or a failed launch.
    OnFailure,
    /// Relaunch after any exit.
    Always,
}

/// What counts as the app having exited.
#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ExitWith {
    /// Every process the app started has exited.
    #[default]
    All,
    /// The launched process has exited (for console apps, the app inside conhost).
    /// Anything it left running, such as build servers, is killed.
    Main,
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
    #[serde(default)]
    pub restart: Restart,
    /// Wait before relaunching; doubles (up to 30s) while the app keeps exiting quickly.
    pub restart_delay_ms: Option<u64>,
    /// Extra environment variables, added to (or overriding) the host's environment.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub exit_with: ExitWith,
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
    let mut cfg: Config =
        toml::from_str(&text).map_err(|e| format!("Invalid config {}:\n\n{e}", path.display()))?;
    if cfg.apps.is_empty() {
        return Err(format!("{} has no [[app]] entries.", path.display()));
    }
    if let Some(dir) = std::path::absolute(path).ok().as_deref().and_then(Path::parent) {
        resolve_cwds(&mut cfg, dir);
    }
    Ok(cfg)
}

/// Make relative `cwd`s relative to the config file's folder rather than the host's
/// working directory, so a config checked into a repo works wherever it's launched from.
fn resolve_cwds(cfg: &mut Config, config_dir: &Path) {
    for app in &mut cfg.apps {
        if let Some(cwd) = &app.cwd
            && Path::new(cwd).is_relative()
        {
            app.cwd = Some(config_dir.join(cwd).to_string_lossy().into_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cwds(toml_text: &str, dir: &str) -> Vec<Option<String>> {
        let mut cfg: Config = toml::from_str(toml_text).unwrap();
        resolve_cwds(&mut cfg, Path::new(dir));
        cfg.apps.into_iter().map(|a| a.cwd).collect()
    }

    #[test]
    fn relative_cwd_resolves_against_config_dir() {
        let text = r#"
            [[app]]
            command = "a"
            cwd = 'src\Api'
            [[app]]
            command = "b"
            cwd = '..\other'
            [[app]]
            command = "c"
            cwd = 'D:\abs'
            [[app]]
            command = "d"
        "#;
        assert_eq!(
            cwds(text, r"C:\repo"),
            [
                Some(r"C:\repo\src\Api".to_string()),
                Some(r"C:\repo\..\other".to_string()),
                Some(r"D:\abs".to_string()),
                None,
            ]
        );
    }
}
