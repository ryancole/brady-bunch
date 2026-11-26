use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const DEFAULT_FILE: &str = "brady.toml";

/// This build's version, e.g. `0.6.0`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

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

/// What a program's copied output (`log`, `ready.log`) is written in.
#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Encoding {
    #[default]
    #[serde(rename = "utf-8")]
    Utf8,
    /// The OEM code page, e.g. 437: what the console uses by default.
    #[serde(rename = "oem")]
    Oem,
    /// The ANSI code page, e.g. 1252.
    #[serde(rename = "ansi")]
    Ansi,
}

impl Encoding {
    pub fn as_str(self) -> &'static str {
        match self {
            Encoding::Utf8 => "utf-8",
            Encoding::Oem => "oem",
            Encoding::Ansi => "ansi",
        }
    }
}

/// `tcp = 10000` (localhost) or `tcp = "host:port"`.
#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum TcpTarget {
    Port(u16),
    Addr(String),
}

/// When an app counts as ready for the apps that depend on it.
#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ReadyConfig {
    /// Something accepts TCP connections.
    pub tcp: Option<TcpTarget>,
    /// An HTTP(S) GET gets any response below 500. Certificates aren't checked.
    pub http: Option<String>,
    /// A regex matches the app's console output.
    pub log: Option<String>,
    /// How long to wait before showing the app as not ready.
    pub timeout_ms: Option<u64>,
}

pub const DEFAULT_READY_TIMEOUT_MS: u64 = 60_000;

/// Keep checking the app after it's ready: its own `tcp`/`http` check, or else the
/// `ready` check.
#[derive(Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct HealthConfig {
    pub tcp: Option<TcpTarget>,
    pub http: Option<String>,
    /// Time between checks.
    #[serde(default = "default_health_interval")]
    pub interval_ms: u64,
    /// Failures in a row before the app counts as unhealthy.
    #[serde(default = "default_health_failures")]
    pub failures: u32,
}

fn default_health_interval() -> u64 {
    10_000
}

fn default_health_failures() -> u32 {
    3
}

/// Raw config file layout. Only used to report errors with line numbers; the real
/// parse happens after `[defaults]` are merged into each app.
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
    /// File the host writes its own events to (launches, exits, readiness, shutdown).
    pub event_log: Option<String>,
    /// How many previous copies of `event_log` and each app's `log` to keep
    /// (`events.1.log`, `events.2.log`, ...) when the host starts.
    #[serde(default = "default_keep_logs")]
    pub keep_logs: u32,
    /// Flash the taskbar button when an app crashes.
    #[serde(default = "yes")]
    pub flash_on_crash: bool,
    /// Restore the window's position, size and zoomed tile from the last run.
    #[serde(default = "yes")]
    pub remember_layout: bool,
    /// Oldest brady-bunch that can run this config, e.g. `"0.6.0"`. Checked first.
    #[allow(dead_code)] // Checked from the raw table, before the typed parse.
    pub min_version: Option<String>,
    /// Named app selections for `--profile`.
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
    #[serde(default)]
    #[allow(dead_code)] // Parsed for validation; merged from the raw table.
    pub defaults: Option<Defaults>,
    #[serde(rename = "task", default)]
    pub tasks: Vec<TaskConfig>,
    #[serde(rename = "app", default)]
    pub apps: Vec<AppConfig>,
}

/// A `[profiles]` entry: `webdev = { skip = ["worker"] }`.
#[derive(Deserialize, Debug, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    #[serde(default)]
    pub only: Vec<String>,
    #[serde(default)]
    pub skip: Vec<String>,
}

/// Keys of `[defaults]`: the per-app keys that make sense to share.
#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)] // Only parsed for validation.
pub struct Defaults {
    cwd: Option<String>,
    console: Option<bool>,
    shutdown: Option<Shutdown>,
    shutdown_timeout_ms: Option<u64>,
    restart: Option<Restart>,
    restart_delay_ms: Option<u64>,
    env: Option<BTreeMap<String, String>>,
    exit_with: Option<ExitWith>,
    quick_edit: Option<bool>,
    path_prepend: Option<Vec<String>>,
    path_append: Option<Vec<String>>,
    encoding: Option<Encoding>,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    /// Label shown above the tile; defaults to the executable name. Always set after `load`.
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
    /// Folders put before / after PATH (after `env`), skipping ones that don't exist.
    /// Absolute after `load`.
    #[serde(default)]
    pub path_prepend: Vec<String>,
    #[serde(default)]
    pub path_append: Vec<String>,
    /// What the program's copied output is written in.
    #[serde(default)]
    pub encoding: Encoding,
    #[serde(default)]
    pub exit_with: ExitWith,
    /// Apps that must be ready before this one starts.
    #[serde(default)]
    pub depends_on: Vec<String>,
    pub ready: Option<ReadyConfig>,
    /// Don't launch the app if its `ready` check already passes.
    #[serde(default)]
    pub skip_if_ready: bool,
    /// Keep running the `ready` check after startup, and show the app as unhealthy
    /// when it keeps failing.
    pub health: Option<HealthConfig>,
    /// Copy the app's console output to this file.
    pub log: Option<String>,
    /// Leave the console's QuickEdit mode as the user's console defaults have it.
    #[serde(default)]
    pub quick_edit: bool,
    /// Shown as a link in the tile header.
    pub url: Option<String>,
    /// A `[[task]]`: runs to completion before the apps start.
    #[serde(skip)]
    pub task: bool,
    /// Tasks: apps that `brady-ctl run` stops while the task reruns, then starts again.
    #[serde(skip)]
    pub stops: Vec<String>,
    /// Line of the entry's `[[app]]` or `[[task]]` header in the config, for messages.
    #[serde(skip)]
    pub line: usize,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct TaskConfig {
    pub name: Option<String>,
    pub command: String,
    pub cwd: Option<String>,
    pub console: Option<bool>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub path_prepend: Vec<String>,
    #[serde(default)]
    pub path_append: Vec<String>,
    #[serde(default)]
    pub encoding: Encoding,
    #[serde(default = "task_exit_with")]
    pub exit_with: ExitWith,
    pub log: Option<String>,
    #[serde(default)]
    pub quick_edit: bool,
    #[serde(default)]
    pub shutdown: Shutdown,
    pub shutdown_timeout_ms: Option<u64>,
    /// Apps to stop while `brady-ctl run` reruns this task, e.g. ones that lock its output.
    #[serde(default)]
    pub stops: Vec<String>,
}

fn task_exit_with() -> ExitWith {
    ExitWith::Main
}

impl From<TaskConfig> for AppConfig {
    fn from(t: TaskConfig) -> AppConfig {
        AppConfig {
            name: t.name,
            command: t.command,
            cwd: t.cwd,
            console: t.console,
            env: t.env,
            path_prepend: t.path_prepend,
            path_append: t.path_append,
            encoding: t.encoding,
            exit_with: t.exit_with,
            log: t.log,
            quick_edit: t.quick_edit,
            shutdown: t.shutdown,
            shutdown_timeout_ms: t.shutdown_timeout_ms,
            task: true,
            stops: t.stops,
            ..Default::default()
        }
    }
}

impl AppConfig {
    pub fn name(&self) -> &str {
        self.name.as_deref().unwrap_or("")
    }

    pub fn ready_timeout_ms(&self) -> u64 {
        self.ready.as_ref().and_then(|r| r.timeout_ms).unwrap_or(DEFAULT_READY_TIMEOUT_MS)
    }

    /// The check `health` runs: its own `tcp`/`http`, else the `ready` check. None
    /// without `health`.
    pub fn health_check(&self) -> Option<ReadyConfig> {
        let h = self.health.as_ref()?;
        if h.tcp.is_some() || h.http.is_some() {
            return Some(ReadyConfig { tcp: h.tcp.clone(), http: h.http.clone(), log: None, timeout_ms: None });
        }
        self.ready.clone()
    }
}

fn default_keep_logs() -> u32 {
    1
}

fn yes() -> bool {
    true
}

fn default_title() -> String {
    "Brady Bunch".into()
}

fn default_timeout() -> u64 {
    5000
}

/// The loaded config: tasks first, then apps, with names, paths and `%VAR%`s resolved.
#[derive(Debug)]
pub struct Loaded {
    pub title: String,
    pub shutdown_timeout_ms: u64,
    pub columns: Option<u32>,
    pub maximized: bool,
    pub event_log: Option<PathBuf>,
    pub keep_logs: u32,
    pub flash_on_crash: bool,
    pub remember_layout: bool,
    pub entries: Vec<AppConfig>,
    /// For each entry, the indices of the entries it waits for.
    pub deps: Vec<Vec<usize>>,
    /// The selection the entries were picked with, profile names as the config spells them.
    pub selection: Selection,
}

/// `--profile`, `--only` and `--skip` from the command line.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Selection {
    pub profiles: Vec<String>,
    pub only: Vec<String>,
    pub skip: Vec<String>,
}

impl Selection {
    /// Command-line arguments that make the same selection.
    pub fn to_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        for (flag, list) in [("--profile", &self.profiles), ("--only", &self.only), ("--skip", &self.skip)] {
            if !list.is_empty() {
                args.push(flag.to_string());
                args.push(list.join(","));
            }
        }
        args
    }
}

/// Explicit path, else `brady.toml` in the working directory, else next to the exe.
/// Absolute, with `.` and `..` resolved.
pub fn resolve_path(arg: Option<PathBuf>) -> PathBuf {
    if let Some(p) = arg {
        return full_path(&p);
    }
    let local = PathBuf::from(DEFAULT_FILE);
    if local.exists() {
        return full_path(&local);
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join(DEFAULT_FILE)))
        .unwrap_or(local)
}

/// `p` made absolute against the working directory, with `.` and `..` resolved the way
/// GetFullPathName does it: by the text alone, so the path needn't exist.
pub fn full_path(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

pub fn load(path: &Path, sel: &Selection) -> Result<Loaded, String> {
    let path = &full_path(path);
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("Could not read config {}: {e}", path.display()))?;
    parse(&text, &config_dir(path), sel).map_err(|e| format!("Invalid config {}:\n\n{e}", path.display()))
}

/// The folder relative paths in the config are relative to.
fn config_dir(path: &Path) -> PathBuf {
    full_path(path).parent().map(Path::to_path_buf).unwrap_or_default()
}

/// The config's `event_log`, as an absolute path, read without checking the rest of
/// the config (for `brady-ctl events` after the host has gone).
pub fn event_log_path(path: &Path) -> Result<Option<PathBuf>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("Could not read config {}: {e}", path.display()))?;
    let table: toml::Table =
        toml::from_str(&text).map_err(|e| format!("Invalid config {}:\n\n{e}", path.display()))?;
    Ok(table
        .get("event_log")
        .and_then(toml::Value::as_str)
        .map(|p| absolute_in(&config_dir(path), &expand(p, &|k| env_var(k)))))
}

fn parse(text: &str, config_dir: &Path, sel: &Selection) -> Result<Loaded, String> {
    let mut table: toml::Table = toml::from_str(text).map_err(|e| e.to_string())?;
    // Before anything else, so a config using newer keys says to upgrade.
    check_min_version(table.get("min_version"))?;
    // First pass: for error messages with line numbers.
    let raw: Config = toml::from_str(text).map_err(|e| {
        let e = e.to_string();
        if e.contains("unknown field") {
            format!("{e}\nThis is brady-bunch {VERSION}; a key added in a newer version needs a newer brady-bunch.")
        } else {
            e
        }
    })?;
    if raw.apps.is_empty() {
        return Err("There are no [[app]] entries.".into());
    }
    // Second pass: merge [defaults] into each app, then parse for real.
    if let Some(toml::Value::Table(defaults)) = table.remove("defaults")
        && let Some(toml::Value::Array(apps)) = table.get_mut("app")
    {
        for app in apps.iter_mut().filter_map(toml::Value::as_table_mut) {
            apply_defaults(app, &defaults);
        }
    }
    let cfg: Config = toml::Value::Table(table).try_into().map_err(|e: toml::de::Error| e.to_string())?;

    let host_env = |k: &str| env_var(k);
    let mut entries: Vec<AppConfig> =
        cfg.tasks.into_iter().map(AppConfig::from).chain(cfg.apps).collect();
    let header_lines = |header: &str| -> Vec<usize> {
        text.lines()
            .enumerate()
            .filter(|(_, l)| l.trim_start().starts_with(header))
            .map(|(i, _)| i + 1)
            .collect()
    };
    let (task_lines, app_lines) = (header_lines("[[task]]"), header_lines("[[app]]"));
    let mut lines = task_lines.into_iter().chain(app_lines);
    for e in &mut entries {
        resolve_entry(e, config_dir);
        e.line = lines.next().unwrap_or(0);
    }
    let deps = dependencies(&entries)?;
    let selection = with_profiles(&cfg.profiles, sel, &entries)?;
    let (mut entries, deps) = select(entries, deps, &selection)?;
    // Apps a task stops may have been left out by the selection.
    let kept: Vec<String> = entries.iter().map(|e| e.name().to_string()).collect();
    for e in &mut entries {
        e.stops.retain(|n| kept.iter().any(|k| k.eq_ignore_ascii_case(n)));
    }
    check_cycles(&deps, &entries)?;
    let event_log = cfg.event_log.map(|p| absolute_in(config_dir, &expand(&p, &host_env)));
    Ok(Loaded {
        title: cfg.title,
        shutdown_timeout_ms: cfg.shutdown_timeout_ms,
        columns: cfg.columns,
        maximized: cfg.maximized,
        event_log,
        keep_logs: cfg.keep_logs,
        flash_on_crash: cfg.flash_on_crash,
        remember_layout: cfg.remember_layout,
        entries,
        deps,
        selection: Selection { profiles: selection.profiles, ..sel.clone() },
    })
}

/// `min_version = "0.6.0"`: refuse to run under an older brady-bunch.
fn check_min_version(v: Option<&toml::Value>) -> Result<(), String> {
    let Some(v) = v else { return Ok(()) };
    let wanted = v.as_str().and_then(parse_version).ok_or("min_version must be a version like \"0.6.0\".")?;
    let have = parse_version(VERSION).unwrap_or_default();
    if have < wanted {
        let w = v.as_str().unwrap_or_default();
        return Err(format!(
            "This config needs brady-bunch {w} or newer; this is {VERSION}. Update brady-bunch to run it:\n\n{}",
            install_command(w)
        ));
    }
    Ok(())
}

/// The PowerShell command that installs `version` where this brady-bunch is.
fn install_command(version: &str) -> String {
    const URL: &str = "https://github.com/ryancole/brady-bunch/releases/latest/download/install.ps1";
    let mut cmd = format!("& ([scriptblock]::Create((irm {URL}))) -Version {version}");
    let here = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf));
    let default = std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join(r"Programs\brady-bunch"));
    if let Some(here) = here
        && default.is_none_or(|d| !d.to_string_lossy().eq_ignore_ascii_case(&here.to_string_lossy()))
    {
        cmd.push_str(&format!(" -InstallDir '{}'", here.display()));
    }
    cmd
}

/// `1.2.3` (or `1.2`, `1`) as comparable numbers.
pub fn parse_version(s: &str) -> Option<[u32; 3]> {
    let mut out = [0; 3];
    let mut parts = s.trim().trim_start_matches('v').split('.');
    for slot in &mut out {
        match parts.next() {
            Some(p) => *slot = p.parse().ok()?,
            None => break,
        }
    }
    parts.next().is_none().then_some(out)
}

/// Fold the named profiles' `only`/`skip` into the selection. Profile names are
/// case-insensitive; the result carries them as the config spells them.
fn with_profiles(
    profiles: &BTreeMap<String, Profile>,
    sel: &Selection,
    entries: &[AppConfig],
) -> Result<Selection, String> {
    let mut out = Selection { profiles: Vec::new(), only: sel.only.clone(), skip: sel.skip.clone() };
    for name in &sel.profiles {
        let Some((key, p)) = profiles.iter().find(|(k, _)| k.eq_ignore_ascii_case(name.trim())) else {
            let known: Vec<&str> = profiles.keys().map(String::as_str).collect();
            return Err(if known.is_empty() {
                format!("There is no profile called \"{name}\"; the config has no [profiles].")
            } else {
                format!("There is no profile called \"{name}\"; the profiles are: {}.", known.join(", "))
            });
        };
        for n in p.only.iter().chain(&p.skip) {
            find(entries, n).map_err(|e| format!("Profile {key}: {e}"))?;
        }
        if !out.profiles.contains(key) {
            out.profiles.push(key.clone());
        }
        out.only.extend(p.only.iter().cloned());
        out.skip.extend(p.skip.iter().cloned());
    }
    Ok(out)
}

fn apply_defaults(app: &mut toml::Table, defaults: &toml::Table) {
    for (k, v) in defaults {
        match (app.get_mut(k), v) {
            // The app's own folders come first, then the defaults'.
            (Some(toml::Value::Array(mine)), toml::Value::Array(theirs)) if k.starts_with("path_") => {
                mine.extend(theirs.iter().cloned());
            }
            (Some(toml::Value::Table(mine)), toml::Value::Table(theirs)) if k == "env" => {
                for (ek, ev) in theirs {
                    if !mine.keys().any(|m| m.eq_ignore_ascii_case(ek)) {
                        mine.insert(ek.clone(), ev.clone());
                    }
                }
            }
            (Some(_), _) => {}
            (None, _) => {
                app.insert(k.clone(), v.clone());
            }
        }
    }
}

/// Fill in the name, expand `%VAR%`s and make paths absolute.
fn resolve_entry(e: &mut AppConfig, config_dir: &Path) {
    // `env` values see the host's environment; everything else also sees `env`.
    let host_env = |k: &str| env_var(k);
    let env: BTreeMap<String, String> =
        e.env.iter().map(|(k, v)| (k.clone(), expand(v, &host_env))).collect();
    let app_env = |k: &str| {
        env.iter().find(|(ek, _)| ek.eq_ignore_ascii_case(k)).map(|(_, v)| v.clone()).or_else(|| env_var(k))
    };
    e.command = expand(&e.command, &app_env);
    // Relative `cwd`s are relative to the config file's folder rather than the host's
    // working directory, so a config checked into a repo works wherever it's launched from.
    e.cwd = e.cwd.as_ref().map(|c| absolute_in(config_dir, &expand(c, &app_env)).to_string_lossy().into_owned());
    e.log = e.log.as_ref().map(|l| absolute_in(config_dir, &expand(l, &app_env)).to_string_lossy().into_owned());
    e.url = e.url.as_ref().map(|u| expand(u, &app_env));
    let dirs = |list: &[String]| -> Vec<String> {
        list.iter().map(|d| absolute_in(config_dir, &expand(d, &app_env)).to_string_lossy().into_owned()).collect()
    };
    e.path_prepend = dirs(&e.path_prepend);
    e.path_append = dirs(&e.path_append);
    let checks = [
        e.ready.as_mut().map(|r| (&mut r.http, &mut r.tcp)),
        e.health.as_mut().map(|h| (&mut h.http, &mut h.tcp)),
    ];
    for (http, tcp) in checks.into_iter().flatten() {
        *http = http.as_ref().map(|u| expand(u, &app_env));
        if let Some(TcpTarget::Addr(a)) = tcp {
            *tcp = Some(TcpTarget::Addr(expand(a, &app_env)));
        }
    }
    e.env = env;
    if e.name.is_none() {
        let exe = first_token(&e.command);
        e.name = Some(
            Path::new(&exe).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or(exe),
        );
    }
}

fn env_var(k: &str) -> Option<String> {
    std::env::var_os(k).map(|v| v.to_string_lossy().into_owned())
}

/// `p` relative to `dir` (if it's relative), with `.` and `..` resolved.
fn absolute_in(dir: &Path, p: &str) -> PathBuf {
    if p.is_empty() {
        return PathBuf::new();
    }
    full_path(&if Path::new(p).is_relative() { dir.join(p) } else { PathBuf::from(p) })
}

/// Replace `%NAME%` with the variable's value, like cmd.exe: names that aren't set
/// are left as they are.
pub fn expand(s: &str, lookup: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) if end > 0 => match lookup(&after[..end]) {
                Some(v) => {
                    out.push_str(&v);
                    rest = &after[end + 1..];
                }
                None => {
                    // Not a variable: keep the first '%' and rescan from the second,
                    // which may open a real one (`50% %HOME%`).
                    out.push('%');
                    rest = after;
                }
            },
            _ => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

pub fn first_token(command: &str) -> String {
    let s = command.trim_start();
    if let Some(rest) = s.strip_prefix('"') {
        rest.split('"').next().unwrap_or("").to_string()
    } else {
        s.split_whitespace().next().unwrap_or("").to_string()
    }
}

/// Index of the entry called `name` (case-insensitive).
pub fn find(entries: &[AppConfig], name: &str) -> Result<usize, String> {
    let hits: Vec<usize> =
        (0..entries.len()).filter(|&i| entries[i].name().eq_ignore_ascii_case(name.trim())).collect();
    match hits[..] {
        [i] => Ok(i),
        [] => Err(format!("There is no app called \"{name}\".")),
        _ => Err(format!("More than one app is called \"{name}\"; give them distinct names.")),
    }
}

/// `line 12: ` for messages about an entry.
fn at(e: &AppConfig) -> String {
    if e.line > 0 { format!("line {}: ", e.line) } else { String::new() }
}

/// What each entry waits for: every earlier task, plus its `depends_on`.
fn dependencies(entries: &[AppConfig]) -> Result<Vec<Vec<usize>>, String> {
    let tasks: Vec<usize> = (0..entries.len()).filter(|&i| entries[i].task).collect();
    let mut all = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        let mut deps: Vec<usize> = tasks.iter().copied().filter(|&t| t < i).collect();
        if e.task {
            // Tasks run one after another: each waits only for the one before it.
            deps = deps.last().copied().into_iter().collect();
        }
        for d in &e.depends_on {
            let j = find(entries, d).map_err(|err| format!("{}{} depends_on: {err}", at(e), e.name()))?;
            if j == i {
                return Err(format!("{}{} depends on itself.", at(e), e.name()));
            }
            if entries[j].task {
                return Err(format!("{}{} depends on the task {}; apps always wait for every task.", at(e), e.name(), d));
            }
            if !deps.contains(&j) {
                deps.push(j);
            }
        }
        if let Some(r) = &e.ready {
            check_ready(e.name(), r).map_err(|err| format!("{}{err}", at(e)))?;
        }
        for n in &e.stops {
            let j = find(entries, n).map_err(|err| format!("{}{} stops: {err}", at(e), e.name()))?;
            if entries[j].task {
                return Err(format!("{}{} stops the task {n}; only apps can be stopped.", at(e), e.name()));
            }
        }
        if e.skip_if_ready && !e.ready.as_ref().is_some_and(|r| r.tcp.is_some() || r.http.is_some()) {
            return Err(format!("{}{}: skip_if_ready needs a `ready` check with tcp or http.", at(e), e.name()));
        }
        if let Some(h) = &e.health {
            if h.tcp.is_some() && h.http.is_some() {
                return Err(format!("{}{}: `health` takes one of tcp or http, not both.", at(e), e.name()));
            }
            if h.tcp.is_some() || h.http.is_some() {
                let r = ReadyConfig { tcp: h.tcp.clone(), http: h.http.clone(), log: None, timeout_ms: None };
                check_ready(e.name(), &r).map_err(|err| format!("{}{}", at(e), err.replace("ready.", "health.")))?;
            } else if !e.ready.as_ref().is_some_and(|r| r.tcp.is_some() || r.http.is_some()) {
                return Err(format!(
                    "{}{}: health needs its own tcp or http check (`health = {{ http = \"...\" }}`), \
                     or a `ready` check with tcp or http to reuse.",
                    at(e),
                    e.name()
                ));
            }
        }
        all.push(deps);
    }
    Ok(all)
}

fn check_ready(name: &str, r: &ReadyConfig) -> Result<(), String> {
    let kinds = [r.tcp.is_some(), r.http.is_some(), r.log.is_some()].iter().filter(|&&b| b).count();
    if kinds != 1 {
        return Err(format!("{name}: `ready` needs exactly one of tcp, http or log."));
    }
    if let Some(u) = &r.http {
        let lower = u.to_ascii_lowercase();
        if !lower.starts_with("http://") && !lower.starts_with("https://") {
            return Err(format!("{name}: ready.http must start with http:// or https://."));
        }
    }
    if let Some(TcpTarget::Addr(a)) = &r.tcp
        && a.rsplit_once(':').and_then(|(_, p)| p.parse::<u16>().ok()).is_none()
    {
        return Err(format!("{name}: ready.tcp must be a port number or \"host:port\"."));
    }
    if let Some(re) = &r.log
        && let Err(e) = regex_lite::Regex::new(re)
    {
        return Err(format!("{name}: ready.log is not a valid regex: {e}"));
    }
    Ok(())
}

/// Apply `--only` and `--skip`. `--only` keeps the named apps, everything they depend
/// on, and every task. Dependencies on skipped apps are dropped.
fn select(
    entries: Vec<AppConfig>,
    deps: Vec<Vec<usize>>,
    sel: &Selection,
) -> Result<(Vec<AppConfig>, Vec<Vec<usize>>), String> {
    let mut keep = vec![sel.only.is_empty(); entries.len()];
    let mut stack = Vec::new();
    for n in &sel.only {
        let i = find(&entries, n).map_err(|e| format!("--only: {e}"))?;
        stack.push(i);
    }
    for (i, e) in entries.iter().enumerate() {
        if e.task && !sel.only.is_empty() {
            stack.push(i);
        }
    }
    while let Some(i) = stack.pop() {
        if !keep[i] {
            keep[i] = true;
            stack.extend(&deps[i]);
        }
    }
    for n in &sel.skip {
        keep[find(&entries, n).map_err(|e| format!("--skip: {e}"))?] = false;
    }
    // Renumber the kept entries' dependencies.
    let mut new_index = vec![None; entries.len()];
    let mut next = 0;
    for (i, &k) in keep.iter().enumerate() {
        if k {
            new_index[i] = Some(next);
            next += 1;
        }
    }
    if next == 0 || !entries.iter().zip(&keep).any(|(e, &k)| k && !e.task) {
        return Err("--only/--skip left no apps to run.".into());
    }
    let is_task: Vec<bool> = entries.iter().map(|e| e.task).collect();
    let tasks_kept: Vec<usize> = (0..entries.len()).filter(|&i| keep[i] && is_task[i]).collect();
    let mut out_entries = Vec::new();
    let mut out_deps = Vec::new();
    for (i, e) in entries.into_iter().enumerate() {
        if !keep[i] {
            continue;
        }
        let mut d: Vec<usize> = if e.task {
            // The task before this one, among those kept.
            tasks_kept.iter().copied().rfind(|&t| t < i).into_iter().collect()
        } else {
            let mut d: Vec<usize> = tasks_kept.iter().copied().filter(|&t| t < i).collect();
            d.extend(deps[i].iter().copied().filter(|&j| keep[j] && !is_task[j]));
            d
        };
        d = d.into_iter().filter_map(|j| new_index[j]).collect();
        d.dedup();
        out_entries.push(e);
        out_deps.push(d);
    }
    Ok((out_entries, out_deps))
}

fn check_cycles(deps: &[Vec<usize>], entries: &[AppConfig]) -> Result<(), String> {
    let placed = start_order(deps);
    if placed.len() == deps.len() {
        return Ok(());
    }
    let stuck: Vec<&str> =
        (0..deps.len()).filter(|i| !placed.contains(i)).map(|i| entries[i].name()).collect();
    Err(format!("depends_on has a cycle between: {}", stuck.join(", ")))
}

/// Entries in an order where each comes after everything it depends on, otherwise
/// in config order. Shorter than `deps` if there's a cycle.
pub fn start_order(deps: &[Vec<usize>]) -> Vec<usize> {
    let mut placed = vec![false; deps.len()];
    let mut order = Vec::new();
    while let Some(i) = (0..deps.len()).find(|&i| !placed[i] && deps[i].iter().all(|&d| placed[d])) {
        placed[i] = true;
        order.push(i);
    }
    order
}

/// `items` in start order (see `start_order`); anything not in `deps` is dropped.
pub fn in_start_order(deps: &[Vec<usize>], items: &[usize]) -> Vec<usize> {
    start_order(deps).into_iter().filter(|i| items.contains(i)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load_str(text: &str) -> Result<Loaded, String> {
        parse(text, Path::new(r"C:\repo"), &Selection::default())
    }

    fn names(l: &Loaded) -> Vec<&str> {
        l.entries.iter().map(|e| e.name()).collect()
    }

    #[test]
    fn relative_paths_resolve_against_config_dir() {
        let l = parse(
            r#"
            event_log = '..\..\.brady\events.log'
            [[app]]
            command = "a"
            cwd = 'src\Api'
            log = '.\logs\..\logs\a.log'
            path_prepend = ['..\bin']
            [[app]]
            command = "b"
            cwd = '..\other'
            [[app]]
            command = "c"
            cwd = 'D:\abs\x\..'
            [[app]]
            command = "d"
        "#,
            Path::new(r"C:\repo\etc\brady"),
            &Selection::default(),
        )
        .unwrap();
        let cwds: Vec<_> = l.entries.iter().map(|a| a.cwd.clone()).collect();
        assert_eq!(
            cwds,
            [
                Some(r"C:\repo\etc\brady\src\Api".to_string()),
                Some(r"C:\repo\etc\other".to_string()),
                Some(r"D:\abs".to_string()),
                None,
            ]
        );
        assert_eq!(l.entries[0].log.as_deref(), Some(r"C:\repo\etc\brady\logs\a.log"));
        assert_eq!(l.entries[0].path_prepend, [r"C:\repo\etc\bin"]);
        assert_eq!(l.event_log.as_deref(), Some(Path::new(r"C:\repo\.brady\events.log")));
    }

    #[test]
    fn config_paths_given_any_way_resolve_alike() {
        let cwd = std::env::current_dir().unwrap();
        let name = cwd.file_name().unwrap().to_string_lossy().into_owned();
        let relative = PathBuf::from(format!(r"..\{name}\.\brady.toml"));
        assert_eq!(resolve_path(Some(relative)), cwd.join("brady.toml"));
        assert_eq!(resolve_path(Some(PathBuf::from("brady.toml"))), cwd.join("brady.toml"));
        assert!(!resolve_path(Some(cwd.join(r"a\..\b.toml"))).to_string_lossy().contains(".."));
    }

    #[test]
    fn expand_like_cmd() {
        let vars = |k: &str| match k.to_ascii_uppercase().as_str() {
            "A" => Some("x y".to_string()),
            "HOME" => Some(r"C:\h".to_string()),
            _ => None,
        };
        assert_eq!(expand(r#"cmd /c ""%A%" --f""#, &vars), r#"cmd /c ""x y" --f""#);
        assert_eq!(expand("%NOPE% and %a%", &vars), "%NOPE% and x y");
        assert_eq!(expand("50% %HOME%", &vars), r"50% C:\h");
        assert_eq!(expand("100%", &vars), "100%");
        assert_eq!(expand("%%", &vars), "%%");
    }

    #[test]
    fn env_values_and_command_are_expanded() {
        // SAFETY: tests in this module don't otherwise touch this variable.
        unsafe { std::env::set_var("BB_TEST_DIR", r"C:\tools dir") };
        let l = load_str(
            r#"
            [[app]]
            command = '"%TOOL%" --port %PORT%'
            cwd = '%BB_TEST_DIR%\work'
            env = { TOOL = '%BB_TEST_DIR%\tool.exe', PORT = "8080", PATH = '%BB_TEST_DIR%;%PATH%' }
        "#,
        )
        .unwrap();
        let a = &l.entries[0];
        assert_eq!(a.command, r#""C:\tools dir\tool.exe" --port 8080"#);
        assert_eq!(a.cwd.as_deref(), Some(r"C:\tools dir\work"));
        assert!(a.env["PATH"].starts_with(r"C:\tools dir;") && !a.env["PATH"].contains("%PATH%"));
        assert_eq!(a.name(), "tool");
    }

    #[test]
    fn defaults_fill_missing_keys_and_merge_env() {
        let l = load_str(
            r#"
            [defaults]
            exit_with = "main"
            shutdown_timeout_ms = 30000
            env = { A = "1", B = "2" }
            [[app]]
            command = "x"
            env = { b = "mine" }
            [[app]]
            command = "y"
            exit_with = "all"
        "#,
        )
        .unwrap();
        assert_eq!(l.entries[0].exit_with, ExitWith::Main);
        assert_eq!(l.entries[0].shutdown_timeout_ms, Some(30000));
        assert_eq!(l.entries[0].env.get("A").map(String::as_str), Some("1"));
        assert_eq!(l.entries[0].env.get("b").map(String::as_str), Some("mine"));
        assert!(!l.entries[0].env.contains_key("B"));
        assert_eq!(l.entries[1].exit_with, ExitWith::All);
    }

    #[test]
    fn defaults_reject_app_specific_keys() {
        let e = load_str("[defaults]\ncommand = 'x'\n[[app]]\ncommand = 'y'\n").unwrap_err();
        assert!(e.contains("command"), "{e}");
    }

    #[test]
    fn tasks_come_first_and_gate_apps() {
        let l = load_str(
            r#"
            [[app]]
            name = "db"
            command = "db"
            [[app]]
            name = "api"
            command = "api"
            depends_on = ["db"]
            [[task]]
            name = "build"
            command = "dotnet build"
            [[task]]
            name = "npm"
            command = "cmd /c npm install"
        "#,
        )
        .unwrap();
        assert_eq!(names(&l), ["build", "npm", "db", "api"]);
        assert!(l.entries[0].task && l.entries[0].exit_with == ExitWith::Main);
        assert_eq!(l.deps, [vec![], vec![0], vec![0, 1], vec![0, 1, 2]]);
    }

    #[test]
    fn unknown_or_cyclic_dependencies_are_errors() {
        let e = load_str("[[app]]\ncommand = 'a'\ndepends_on = ['nope']\n").unwrap_err();
        assert!(e.contains("nope"), "{e}");
        let e = load_str(
            "[[app]]\nname='a'\ncommand='a'\ndepends_on=['b']\n[[app]]\nname='b'\ncommand='b'\ndepends_on=['a']\n",
        )
        .unwrap_err();
        assert!(e.contains("cycle"), "{e}");
    }

    #[test]
    fn errors_name_the_entry_line_and_health_needs_a_network_check() {
        let text = "title = \"x\"\n\n[[app]]\nname = \"a\"\ncommand = \"a\"\n\n[[app]]\nname = \"b\"\ncommand = \"b\"\ndepends_on = [\"zz\"]\n";
        let err = load_str(text).unwrap_err();
        assert!(err.starts_with("line 7: b depends_on"), "{err}");
        let health = "[[app]]\ncommand = \"a\"\nhealth = {}\n";
        assert!(load_str(health).unwrap_err().contains("health needs its own tcp or http check"));
        let ok = "[[app]]\ncommand = \"a\"\nready = { tcp = 1 }\nhealth = { interval_ms = 500 }\n";
        let h = load_str(ok).unwrap().entries[0].health.clone().unwrap();
        assert_eq!((h.interval_ms, h.failures), (500, 3));
        let l = load_str("keep_logs = 3\nflash_on_crash = false\n[[app]]\ncommand = \"a\"\n").unwrap();
        assert_eq!((l.keep_logs, l.flash_on_crash, l.remember_layout), (3, false, true));
    }

    #[test]
    fn health_can_have_its_own_check() {
        let text = "[[app]]\ncommand = 'a'\nready = { log = 'compiled successfully' }\n\
                    health = { http = 'https://localhost:%BB_HEALTH_PORT%', interval_ms = 500 }\n";
        // SAFETY: tests in this module don't otherwise touch this variable.
        unsafe { std::env::set_var("BB_HEALTH_PORT", "8000") };
        let l = load_str(text).unwrap();
        let check = l.entries[0].health_check().unwrap();
        assert_eq!(check.http.as_deref(), Some("https://localhost:8000"));
        assert!(check.log.is_none());
        // Without its own check, health reuses a tcp/http ready check.
        let l = load_str("[[app]]\ncommand = 'a'\nready = { tcp = 5 }\nhealth = {}\n").unwrap();
        assert_eq!(l.entries[0].health_check().unwrap().tcp, Some(TcpTarget::Port(5)));
        let both = "[[app]]\ncommand = 'a'\nhealth = { tcp = 1, http = 'http://x' }\n";
        assert!(load_str(both).unwrap_err().contains("not both"));
        let bad = "[[app]]\ncommand = 'a'\nhealth = { http = 'localhost' }\n";
        assert!(load_str(bad).unwrap_err().contains("health.http must start with"));
        let log = "[[app]]\ncommand = 'a'\nhealth = { log = 'x' }\n";
        assert!(load_str(log).is_err(), "health has no log check");
    }

    #[test]
    fn path_lists_resolve_and_merge_with_defaults() {
        let l = load_str(
            r#"
            [defaults]
            path_prepend = ['C:\shared']
            path_append = ['tools']
            encoding = "oem"
            [[app]]
            command = "a"
            path_prepend = ['node_modules\.bin', 'D:\abs']
            [[app]]
            command = "b"
            encoding = "utf-8"
            [[task]]
            command = "t"
            path_prepend = ['bin']
        "#,
        )
        .unwrap();
        let (t, a, b) = (&l.entries[0], &l.entries[1], &l.entries[2]);
        assert_eq!(a.path_prepend, [r"C:\repo\node_modules\.bin", r"D:\abs", r"C:\shared"]);
        assert_eq!(a.path_append, [r"C:\repo\tools"]);
        assert_eq!(b.path_prepend, [r"C:\shared"]);
        assert_eq!((a.encoding, b.encoding, t.encoding), (Encoding::Oem, Encoding::Utf8, Encoding::Utf8));
        assert_eq!(t.path_prepend, [r"C:\repo\bin"]);
        assert!(load_str("[[app]]\ncommand = 'a'\nencoding = 'latin1'\n").is_err());
    }

    #[test]
    fn event_log_path_needs_only_the_key() {
        let dir = std::env::temp_dir().join(format!("bb-evpath-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("x.toml");
        // Not a loadable config (no apps, a future version), but the path is still found.
        std::fs::write(&file, "min_version = '999.0'\nevent_log = 'logs\\ev.log'\n").unwrap();
        assert_eq!(event_log_path(&file).unwrap(), Some(dir.join(r"logs\ev.log")));
        std::fs::write(&file, "title = 'x'\n").unwrap();
        assert_eq!(event_log_path(&file).unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ready_needs_exactly_one_check() {
        let e = load_str("[[app]]\ncommand='a'\nready = { timeout_ms = 5 }\n").unwrap_err();
        assert!(e.contains("exactly one"), "{e}");
        let e = load_str("[[app]]\ncommand='a'\nready = { tcp = 1, http = 'http://x' }\n").unwrap_err();
        assert!(e.contains("exactly one"), "{e}");
        let e = load_str("[[app]]\ncommand='a'\nready = { log = '(' }\n").unwrap_err();
        assert!(e.contains("regex"), "{e}");
        let e = load_str("[[app]]\ncommand='a'\nskip_if_ready = true\n").unwrap_err();
        assert!(e.contains("skip_if_ready"), "{e}");
        let l = load_str("[[app]]\ncommand='a'\nready = { tcp = '127.0.0.1:10000', timeout_ms = 5 }\n").unwrap();
        assert_eq!(l.entries[0].ready_timeout_ms(), 5);
    }

    const STACK: &str = r#"
        [[task]]
        name = "build"
        command = "b"
        [[app]]
        name = "solr"
        command = "s"
        [[app]]
        name = "azurite"
        command = "z"
        [[app]]
        name = "api"
        command = "a"
        depends_on = ["solr", "azurite"]
        [[app]]
        name = "worker"
        command = "w"
        depends_on = ["azurite"]
        [[app]]
        name = "web"
        command = "n"
    "#;

    fn with_profiles_table(extra: &str) -> String {
        format!(
            "{STACK}\n[profiles]\nwebdev = {{ skip = [\"worker\"] }}\nquick = {{ skip = [\"build\"] }}\n{extra}"
        )
    }

    #[test]
    fn profiles_merge_with_each_other_and_the_command_line() {
        let text = with_profiles_table("");
        let sel = Selection {
            profiles: vec!["WebDev".into(), "quick".into()],
            skip: vec!["web".into()],
            ..Default::default()
        };
        let l = parse(&text, Path::new("C:\\"), &sel).unwrap();
        assert_eq!(names(&l), ["solr", "azurite", "api"]);
        assert_eq!(l.selection.profiles, ["webdev", "quick"]);
        assert_eq!(l.selection.skip, ["web"]);
        let sel = Selection { profiles: vec!["nope".into()], ..Default::default() };
        let err = parse(&text, Path::new("C:\\"), &sel).unwrap_err();
        assert!(err.contains("quick, webdev"), "{err}");
        let bad = with_profiles_table("broken = { skip = [\"missing\"] }");
        let sel = Selection { profiles: vec!["broken".into()], ..Default::default() };
        assert!(parse(&bad, Path::new("C:\\"), &sel).unwrap_err().contains("Profile broken"));
    }

    #[test]
    fn min_version_is_checked_before_unknown_keys() {
        let text = format!("min_version = \"999.0\"\nbrand_new_key = 1\n{STACK}");
        let err = load_str(&text).unwrap_err();
        assert!(err.contains("needs brady-bunch 999.0 or newer"), "{err}");
        assert!(err.contains("install.ps1))) -Version 999.0"), "{err}");
        let text = format!("min_version = \"{VERSION}\"\n{STACK}");
        assert!(load_str(&text).is_ok());
        assert!(load_str(&format!("min_version = \"soon\"\n{STACK}")).is_err());
        let err = load_str(&format!("brand_new_key = 1\n{STACK}")).unwrap_err();
        assert!(err.contains("needs a newer brady-bunch"), "{err}");
        assert!(parse_version("0.10.2").unwrap() > parse_version("0.9").unwrap());
    }

    #[test]
    fn task_stops_must_name_apps_and_follow_the_selection() {
        let text = STACK.replace("command = \"b\"", "command = \"b\"\nstops = [\"api\", \"worker\"]");
        let sel = Selection { skip: vec!["worker".into()], ..Default::default() };
        let l = parse(&text, Path::new("C:\\"), &sel).unwrap();
        assert_eq!(l.entries[0].stops, ["api"]);
        let bad = STACK.replace("command = \"b\"", "command = \"b\"\nstops = [\"nope\"]");
        assert!(load_str(&bad).unwrap_err().contains("build stops"));
        let app_stops = STACK.replace("command = \"s\"", "command = \"s\"\nstops = [\"api\"]");
        assert!(load_str(&app_stops).is_err(), "stops is only for tasks");
    }

    #[test]
    fn only_keeps_dependencies_and_tasks() {
        let sel = Selection { only: vec!["API".into()], ..Default::default() };
        let l = parse(STACK, Path::new("C:\\"), &sel).unwrap();
        assert_eq!(names(&l), ["build", "solr", "azurite", "api"]);
        assert_eq!(l.deps[3], [0, 1, 2]);
    }

    #[test]
    fn skip_drops_apps_and_dependencies_on_them() {
        let sel = Selection { skip: vec!["worker".into(), "azurite".into(), "build".into()], ..Default::default() };
        let l = parse(STACK, Path::new("C:\\"), &sel).unwrap();
        assert_eq!(names(&l), ["solr", "api", "web"]);
        assert_eq!(l.deps, [vec![], vec![0], vec![]]);
        let sel = Selection { skip: vec!["nope".into()], ..Default::default() };
        assert!(parse(STACK, Path::new("C:\\"), &sel).is_err());
    }

    #[test]
    fn example_config_loads() {
        let l = load_str(include_str!("../brady.example.toml")).unwrap();
        assert_eq!(names(&l), ["ping", "shell", "powershell", "charmap"]);
    }

    #[test]
    fn start_order_respects_dependencies() {
        // 0 depends on 2; 1 has none.
        assert_eq!(start_order(&[vec![2], vec![], vec![]]), [1, 2, 0]);
        assert_eq!(start_order(&[vec![1], vec![0]]), Vec::<usize>::new());
        // build, api, worker (after api), functions: given in stop order, back in start order.
        let deps = [vec![], vec![0], vec![0, 1], vec![0]];
        assert_eq!(in_start_order(&deps, &[3, 2, 1]), [1, 2, 3]);
        assert_eq!(in_start_order(&deps, &[2, 9]), [2]);
    }
}
