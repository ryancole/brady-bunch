//! Control from the command line. `brady-ctl.exe` (or `brady-bunch.exe --status` etc.)
//! talks to a running host over a named pipe, and `up` starts one.
//!
//! Each host listens on `\\.\pipe\brady-bunch.pid.<host pid>` and, unless another host
//! already has it, on `\\.\pipe\brady-bunch.config.<hash of the config path>`. A request
//! is one line, `verb<TAB>arg<TAB>arg`; the reply is a JSON object, `{"ok": true,
//! "result": ...}` or `{"ok": false, "error": "..."}`, and the host then closes the pipe.

use crate::config::{self, Selection, VERSION};
use serde_json::{Value, json};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_PIPE_CONNECTED, HANDLE, HANDLE_FLAG_INHERIT, HANDLE_FLAGS, HWND, LPARAM, SetHandleInformation,
    WAIT_OBJECT_0, WPARAM,
};
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, FlushFileBuffers, PIPE_ACCESS_DUPLEX};
use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_OUTPUT_HANDLE};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreatePipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::Win32::System::Threading::{
    CREATE_BREAKAWAY_FROM_JOB, CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
    GetExitCodeProcess, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST, OpenProcess,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, UpdateProcThreadAttribute, WaitForSingleObject,
};
use windows::Win32::UI::WindowsAndMessaging::{AllowSetForegroundWindow, PostMessageW, WM_APP};
use windows::core::{HSTRING, PCWSTR, PWSTR};

/// Posted to the host window: lParam is a `Box<Request>`.
pub const WM_APP_CONTROL: u32 = WM_APP + 10;

/// What the pipe thread asks the UI thread for.
pub enum Cmd {
    Status,
    /// One app's entry from the status.
    App(String),
    Start(String),
    Stop(String),
    Restart(String),
    /// Pids attached to an app's console.
    ConsolePids(String),
    /// Bring the host window to the front (a second launch of the same config).
    Activate,
    /// Shut down. Answered with the final status just before the host exits.
    Quit,
    /// Stop these apps (and the task's `stops`), rerun the task, then start the apps
    /// again. Answered once the task has been launched.
    Run { task: String, stop: Vec<String> },
    /// `up` with another selection: start the apps it adds; with `exact`, stop the
    /// ones it leaves out.
    Select { sel: Selection, exact: bool },
}

pub type Reply = mpsc::Sender<Result<Value, String>>;

pub struct Request {
    pub cmd: Cmd,
    pub reply: Reply,
}

/// `brady-bunch.exe` flags that are control commands rather than a launch.
pub const VERBS: [&str; 12] = [
    "--status", "--start", "--stop", "--restart", "--tail", "--up", "--down", "--wait", "--run", "--check", "--version",
    "--events",
];

pub const USAGE: &str = "usage: brady-ctl check [config] [--profile p] [--only a,b] [--skip a,b]
       brady-ctl up [config] [--profile p] [--only a,b] [--skip a,b] [--exact] [--timeout secs]
       brady-ctl down [config|pid] [--timeout secs]
       brady-ctl status [config|pid]
       brady-ctl wait <app> [config|pid] [--timeout secs]
       brady-ctl start|stop|restart <app> [config|pid]
       brady-ctl run <task> [config|pid] [--stop a,b] [--timeout secs]
       brady-ctl tail <app> [config|pid] [--lines N]
       brady-ctl events [config|pid] [--lines N] [--app name]
       brady-ctl --version

The host is found by its config file (default: brady.toml in the working directory,
else next to the exe) or by its process id.

Exit codes: 0 done, 1 usage or config error (check: the config has problems),
2 no host running, 3 the host refused,
4 something failed or timed out (up, down, wait, run).";

/// How long `up`, `down`, `wait` and `run` wait by default.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

fn pid_pipe(pid: u32) -> String {
    format!(r"\\.\pipe\brady-bunch.pid.{pid}")
}

fn config_pipe(config: &Path) -> Option<String> {
    Some(format!(r"\\.\pipe\brady-bunch.config.{}", config_key(config)?))
}

/// A stable key for a config file: a hash of its full path, ignoring case and spelling.
pub fn config_key(config: &Path) -> Option<String> {
    let canonical = std::fs::canonicalize(config::full_path(config)).ok()?;
    let key = canonical.to_string_lossy().to_lowercase();
    // FNV-1a: stable across builds, unlike std's hasher.
    let hash = key.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3));
    Some(format!("{hash:016x}"))
}

// ---- Host side ----

/// Clients waiting for a reply the host sends as it exits (`quit`).
static FINAL_REPLIES: AtomicUsize = AtomicUsize::new(0);

/// Start listening for control requests. Requests are handed to `hwnd`'s thread.
pub fn serve(hwnd: HWND, config: &Path, helper: PathBuf) {
    let hwnd = hwnd.0 as isize;
    let names = [Some(pid_pipe(std::process::id())), config_pipe(config)];
    for (i, name) in names.into_iter().enumerate() {
        let Some(name) = name else { continue };
        let helper = helper.clone();
        // The config pipe belongs to the first host started with that config.
        let exclusive = i == 1;
        std::thread::spawn(move || listen(&name, hwnd, exclusive, &helper));
    }
}

/// Give clients answered on the way out (`quit`) time to read their reply.
pub fn wait_for_final_replies(timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while FINAL_REPLIES.load(Ordering::Acquire) > 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn listen(name: &str, hwnd: isize, exclusive: bool, helper: &Path) {
    let mut first = true;
    loop {
        let mut mode = PIPE_ACCESS_DUPLEX;
        if first && exclusive {
            mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
        }
        let h = unsafe {
            CreateNamedPipeW(
                &HSTRING::from(name),
                mode,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                64 * 1024,
                64 * 1024,
                0,
                None,
            )
        };
        if h.is_invalid() {
            if first {
                crate::events::log(format!("control: can't listen on {name} (another host may have it)"));
                return;
            }
            std::thread::sleep(Duration::from_millis(500));
            continue;
        }
        first = false;
        let connected = unsafe { ConnectNamedPipe(h, None) };
        if let Err(e) = connected
            && e.code() != ERROR_PIPE_CONNECTED.to_hresult()
        {
            drop(unsafe { File::from_raw_handle(h.0) });
            continue;
        }
        let pipe = unsafe { File::from_raw_handle(h.0) };
        let helper = helper.to_path_buf();
        std::thread::spawn(move || serve_client(pipe, hwnd, &helper));
    }
}

fn serve_client(pipe: File, hwnd: isize, helper: &Path) {
    let mut line = String::new();
    let mut reader = BufReader::new(pipe.take(4096));
    let _ = reader.read_line(&mut line);
    let mut pipe = reader.into_inner().into_inner();
    let line = line.trim_end_matches(['\r', '\n']);
    let last_words = line.split('\t').next() == Some("quit");
    if last_words {
        FINAL_REPLIES.fetch_add(1, Ordering::AcqRel);
    }
    let reply = match handle(line, hwnd, helper) {
        Ok(v) => json!({ "ok": true, "result": v }),
        Err(e) => json!({ "ok": false, "error": e }),
    };
    let _ = pipe.write_all(reply.to_string().as_bytes());
    // Let the client read everything before the handle closes. Closing (rather than
    // DisconnectNamedPipe) gives the client a clean end-of-file.
    let _ = unsafe { FlushFileBuffers(HANDLE(pipe.as_raw_handle())) };
    if last_words {
        FINAL_REPLIES.fetch_sub(1, Ordering::AcqRel);
    }
}

fn handle(line: &str, hwnd: isize, helper: &Path) -> Result<Value, String> {
    let mut parts = line.split('\t');
    let verb = parts.next().unwrap_or("");
    let app = parts.next().unwrap_or("").to_string();
    let extra = parts.next().unwrap_or("");
    match verb {
        "status" => ask(hwnd, Cmd::Status),
        "activate" => ask(hwnd, Cmd::Activate),
        "start" => ask(hwnd, Cmd::Start(app)),
        "stop" => {
            ask(hwnd, Cmd::Stop(app.clone()))?;
            settled(hwnd, &app)
        }
        "restart" => {
            ask(hwnd, Cmd::Restart(app.clone()))?;
            settled(hwnd, &app)
        }
        "quit" => ask_until_done(hwnd, Cmd::Quit),
        "run" => ask_until_done(hwnd, Cmd::Run { task: app, stop: split_list(extra) }),
        "select" => {
            // select<TAB>exact<TAB>profiles<TAB>only<TAB>skip
            let sel = Selection {
                profiles: split_list(extra),
                only: split_list(parts.next().unwrap_or("")),
                skip: split_list(parts.next().unwrap_or("")),
            };
            ask(hwnd, Cmd::Select { sel, exact: app == "1" })
        }
        "tail" => {
            let lines = extra.parse().unwrap_or(40);
            let pids = ask(hwnd, Cmd::ConsolePids(app.clone()))?;
            let pids: Vec<u32> = serde_json::from_value(pids).unwrap_or_default();
            if pids.is_empty() {
                return Err(format!("{app} has no console open (not running, or not a console app)"));
            }
            crate::ctrlc::read_any(helper, &pids, lines)
                .map(Value::String)
                .ok_or_else(|| format!("couldn't read {app}'s console"))
        }
        _ => Err(format!("unknown request: {verb}")),
    }
}

/// Hand a command to the UI thread; returns the channel its answer arrives on.
fn post(hwnd: isize, cmd: Cmd) -> Result<mpsc::Receiver<Result<Value, String>>, String> {
    let (tx, rx) = mpsc::channel();
    let req = Box::into_raw(Box::new(Request { cmd, reply: tx }));
    let posted = unsafe { PostMessageW(Some(HWND(hwnd as *mut _)), WM_APP_CONTROL, WPARAM(0), LPARAM(req as isize)) };
    if posted.is_err() {
        // SAFETY: the message wasn't posted, so nothing else owns the request.
        drop(unsafe { Box::from_raw(req) });
        return Err("the host is closing".into());
    }
    Ok(rx)
}

/// Hand a command to the UI thread and wait for its answer.
fn ask(hwnd: isize, cmd: Cmd) -> Result<Value, String> {
    post(hwnd, cmd)?.recv_timeout(Duration::from_secs(10)).map_err(|_| "the host didn't answer".to_string())?
}

/// For commands the host answers when they're done, however long that takes. The
/// client sets its own timeout.
fn ask_until_done(hwnd: isize, cmd: Cmd) -> Result<Value, String> {
    post(hwnd, cmd)?.recv().map_err(|_| "the host exited without answering".to_string())?
}

/// Wait until the app is no longer stopping, then return its status.
fn settled(hwnd: isize, app: &str) -> Result<Value, String> {
    loop {
        let v = ask(hwnd, Cmd::App(app.to_string()))?;
        if v["state"] != "stopping" {
            return Ok(v);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn split_list(s: &str) -> Vec<String> {
    s.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}

// ---- Client side ----

type Failure = (i32, String);

/// Run a control command from the command line. See `USAGE` for the exit codes.
pub fn client_main(args: &[String]) -> i32 {
    attach_output();
    match client(args) {
        Ok(text) => {
            print!("{text}");
            if !text.ends_with('\n') {
                println!();
            }
            0
        }
        Err((code, msg)) => {
            eprintln!("brady-ctl: {msg}");
            code
        }
    }
}

#[derive(Default)]
struct Opts {
    positional: Vec<String>,
    exact: bool,
    lines: Option<u32>,
    timeout: Option<Duration>,
    sel: Selection,
    stop: Vec<String>,
    app: Option<String>,
}

fn usage(m: impl Into<String>) -> Failure {
    (1, m.into())
}

/// Options each verb takes, besides its positional arguments.
fn allowed(verb: &str) -> &'static [&'static str] {
    match verb {
        "tail" => &["--lines"],
        "events" => &["--lines", "--app"],
        "up" => &["--profile", "--only", "--skip", "--exact", "--timeout"],
        "check" => &["--profile", "--only", "--skip"],
        "down" | "wait" => &["--timeout"],
        "run" => &["--stop", "--timeout"],
        _ => &[],
    }
}

fn parse_opts(verb: &str, args: &[String]) -> Result<Opts, Failure> {
    let mut o = Opts::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if !a.starts_with("--") {
            o.positional.push(a.clone());
            continue;
        }
        if !allowed(verb).contains(&a.as_str()) {
            return Err(usage(format!("{verb} doesn't take {a}\n\n{USAGE}")));
        }
        if a == "--exact" {
            o.exact = true;
            continue;
        }
        let value = it.next().ok_or_else(|| usage(format!("{a} needs a value")))?;
        let number = || value.parse::<u64>().map_err(|_| usage(format!("{a} needs a number")));
        match a.as_str() {
            "--lines" => o.lines = Some(number()? as u32),
            "--timeout" => o.timeout = Some(Duration::from_secs(number()?)),
            "--profile" => o.sel.profiles.extend(split_list(value)),
            "--only" => o.sel.only.extend(split_list(value)),
            "--skip" => o.sel.skip.extend(split_list(value)),
            "--stop" => o.stop.extend(split_list(value)),
            "--app" => o.app = Some(value.clone()),
            _ => unreachable!(),
        }
    }
    Ok(o)
}

fn client(args: &[String]) -> Result<String, Failure> {
    // `--status` (brady-bunch.exe) or `status` (brady-ctl.exe).
    let verb = args.first().map(|a| a.trim_start_matches("--")).unwrap_or("");
    if verb == "version" || verb == "V" {
        return Ok(format!("brady-bunch {VERSION}"));
    }
    if !VERBS.iter().any(|v| v.trim_start_matches("--") == verb) {
        return Err(usage(format!("unknown command {verb}\n\n{USAGE}")));
    }
    let o = parse_opts(verb, &args[1..])?;
    let pos = |i: usize| o.positional.get(i).map(String::as_str);
    let max_positional = match verb {
        "status" | "up" | "down" | "check" | "events" => 1,
        _ => 2,
    };
    if o.positional.len() > max_positional {
        return Err(usage(format!("too many arguments for {verb}\n\n{USAGE}")));
    }
    let needs_name = |what: &str| -> Result<&str, Failure> {
        pos(0).ok_or_else(|| usage(format!("{verb} needs {what} name")))
    };
    let timeout = o.timeout.unwrap_or(DEFAULT_TIMEOUT);
    match verb {
        "status" => Ok(pretty(&request(&target_pipe(pos(0))?, &["status"])?)),
        "up" => up(pos(0), &o.sel, o.exact, timeout),
        "check" => check(pos(0), &o.sel),
        "down" => down(&target_pipe(pos(0))?, timeout),
        "wait" => {
            let app = needs_name("an app")?;
            wait_ready(&target_pipe(pos(1))?, Some(&[app.to_string()]), timeout)
        }
        "run" => {
            let task = needs_name("a task")?;
            let pipe = target_pipe(pos(1))?;
            let start = Instant::now();
            let started = request(&pipe, &["run", task, &o.stop.join(",")])?;
            let task = started["task"].as_str().unwrap_or(task).to_string();
            let apps: Vec<String> =
                started["restarting"].as_array().into_iter().flatten().filter_map(|v| v.as_str()).map(String::from).collect();
            wait_run(&pipe, &task, &apps, start, timeout)
        }
        "events" => events(pos(0), o.lines.unwrap_or(40), o.app.as_deref()),
        "tail" => {
            let app = needs_name("an app")?;
            let lines = o.lines.unwrap_or(40).to_string();
            Ok(text(request(&target_pipe(pos(1))?, &["tail", app, &lines])?))
        }
        _ => {
            let app = needs_name("an app")?;
            Ok(pretty(&request(&target_pipe(pos(1))?, &[verb, app])?))
        }
    }
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

fn text(v: Value) -> String {
    match v {
        Value::String(s) => s,
        v => pretty(&v),
    }
}

/// The pipe for a pid, or for a config file (default `brady.toml`).
fn target_pipe(target: Option<&str>) -> Result<String, Failure> {
    match target {
        Some(t) if !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()) => Ok(pid_pipe(t.parse().unwrap_or(0))),
        t => {
            let path = config::resolve_path(t.map(PathBuf::from));
            config_pipe(&path).ok_or_else(|| (2, format!("config {} not found", path.display())))
        }
    }
}

/// Send one request and unwrap the host's reply.
fn request(pipe: &str, fields: &[&str]) -> Result<Value, Failure> {
    let reply = send(pipe, &format!("{}\n", fields.join("\t"))).map_err(|e| (2, e))?;
    let reply: Value = serde_json::from_str(&reply).map_err(|e| (3, format!("bad reply from host: {e}")))?;
    if reply["ok"] != true {
        return Err((3, reply["error"].as_str().unwrap_or("request failed").to_string()));
    }
    Ok(reply["result"].clone())
}

/// Start a host for the config unless one is running, then wait for every app. A host
/// already running gets this selection: the apps it adds are started, and with `exact`
/// the ones it leaves out are stopped.
fn up(config_arg: Option<&str>, sel: &Selection, exact: bool, timeout: Duration) -> Result<String, Failure> {
    let deadline = Instant::now() + timeout;
    let path = config::resolve_path(config_arg.map(PathBuf::from));
    let path = std::path::absolute(&path).unwrap_or(path);
    // Check the config here, where errors can be printed, rather than in the host.
    let loaded = config::load(&path, sel).map_err(|e| (1, e))?;
    let pipe = config_pipe(&path).ok_or_else(|| (1, format!("config {} not found", path.display())))?;
    let wanted: Vec<String> = loaded.entries.iter().map(|e| e.name().to_string()).collect();
    let mut notes = Vec::new();
    match request(&pipe, &["status"]) {
        Ok(status) => {
            let mut want: Vec<String> = wanted.iter().map(|n| n.to_lowercase()).collect();
            want.sort();
            let same = app_names(&status) == want && selection_of(&status) == loaded.selection;
            if !same || exact {
                let changed = select(&pipe, sel, exact)?;
                let list = |k: &str| -> Vec<String> {
                    changed[k].as_array().into_iter().flatten().filter_map(|v| v.as_str()).map(String::from).collect()
                };
                let (added, stopping) = (list("added"), list("stopping"));
                if !added.is_empty() {
                    notes.push(format!("started: {}", added.join(", ")));
                }
                for t in list("skipped_tasks") {
                    notes.push(format!("{t} was skipped when this host started; brady-ctl run {t} runs it"));
                }
                if !stopping.is_empty() {
                    wait_gone(&pipe, &stopping, deadline)?;
                    notes.push(format!("stopped: {}", stopping.join(", ")));
                }
            }
        }
        Err((2, _)) => start_host(&path, &loaded.selection, &pipe, deadline)?,
        Err(e) => return Err(e),
    }
    let ready = wait_ready(&pipe, Some(&wanted), deadline.saturating_duration_since(Instant::now()))?;
    notes.push(ready);
    Ok(notes.join("\n"))
}

/// The selection a status reports.
fn selection_of(status: &Value) -> Selection {
    let list = |k: &str| -> Vec<String> {
        status[k].as_array().into_iter().flatten().filter_map(|v| v.as_str()).map(String::from).collect()
    };
    Selection { profiles: list("profiles"), only: list("only"), skip: list("skip") }
}

/// Ask the host to change its selection; the reply says what it started and stops.
fn select(pipe: &str, sel: &Selection, exact: bool) -> Result<Value, Failure> {
    let (p, o, k) = (sel.profiles.join(","), sel.only.join(","), sel.skip.join(","));
    request(pipe, &["select", if exact { "1" } else { "0" }, &p, &o, &k]).map_err(|(code, e)| match code {
        3 if e.starts_with("unknown request") => (
            3,
            "the running host is an older brady-bunch that can't change its apps; \
             run brady-ctl down first"
                .to_string(),
        ),
        _ => (code, e),
    })
}

/// A second launch of the config with another selection (`brady-bunch.exe --skip ...`):
/// start the apps it adds to the running host.
pub fn change_selection(config: &Path, sel: &Selection, exact: bool) -> Result<Value, String> {
    let pipe = config_pipe(config).ok_or_else(|| format!("config {} not found", config.display()))?;
    select(&pipe, sel, exact).map_err(|(_, e)| e)
}

/// Wait until none of `names` is listed in the status any more (stopped and dropped
/// from the selection).
fn wait_gone(pipe: &str, names: &[String], deadline: Instant) -> Result<(), Failure> {
    loop {
        let status = request(pipe, &["status"])?;
        let left: Vec<&str> =
            apps(&status).map(name_of).filter(|n| names.iter().any(|m| m.eq_ignore_ascii_case(n))).collect();
        if left.is_empty() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err((4, format!("timed out waiting for {} to stop", left.join(", "))));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Launch brady-bunch.exe from this exe's folder and wait for it to answer.
fn start_host(path: &Path, sel: &Selection, pipe: &str, deadline: Instant) -> Result<(), Failure> {
    let exe = std::env::current_exe().map_err(|e| (1, e.to_string()))?.with_file_name("brady-bunch.exe");
    let mut args = vec![path.to_string_lossy().into_owned()];
    args.extend(sel.to_args());
    // Errors go to stderr (read below) rather than a dialog nobody can see.
    args.push("--no-dialog".into());
    let mut host =
        HostProcess::spawn(&exe, &args).map_err(|e| (1, format!("couldn't start {}: {}", exe.display(), e.message())))?;
    loop {
        match request(pipe, &["status"]) {
            Ok(_) => return Ok(()),
            Err((2, _)) => {}
            Err(e) => return Err(e),
        }
        if let Some(code) = host.exit_code() {
            let mut err = String::new();
            let _ = host.stderr.read_to_string(&mut err);
            let err = err.trim();
            return Err((
                4,
                format!("brady-bunch exited with code {code} before it started{}{err}", if err.is_empty() { "" } else { ": " }),
            ));
        }
        if Instant::now() >= deadline {
            return Err((4, "timed out waiting for brady-bunch to start".into()));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A host started by `up`.
struct HostProcess {
    process: OwnedHandle,
    /// The host's stderr, where it says why it couldn't start (`--no-dialog`).
    stderr: File,
}

impl HostProcess {
    /// Start `exe` with stdin and stdout on NUL, stderr on a pipe back to us, and no
    /// other handles. The host outlives this process: if it inherited anything else of
    /// ours (our stdout, or a handle we inherited from our own caller), a caller reading
    /// our output to end-of-file would wait for the host to exit.
    fn spawn(exe: &Path, args: &[String]) -> windows::core::Result<HostProcess> {
        unsafe {
            let inherit = SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                bInheritHandle: true.into(),
                ..Default::default()
            };
            let (mut read, mut write) = (HANDLE::default(), HANDLE::default());
            CreatePipe(&mut read, &mut write, Some(&inherit), 0)?;
            let stderr = File::from_raw_handle(read.0);
            let write = OwnedHandle::from_raw_handle(write.0);
            SetHandleInformation(HANDLE(stderr.as_raw_handle()), HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0))?;
            let nul = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("NUL")
                .map_err(|e| windows::core::Error::new(windows::core::HRESULT(e.raw_os_error().unwrap_or(1)), "NUL"))?;
            let nul_h = HANDLE(nul.as_raw_handle());
            SetHandleInformation(nul_h, HANDLE_FLAG_INHERIT.0, HANDLE_FLAG_INHERIT)?;
            let write_h = HANDLE(write.as_raw_handle());

            // Inherit exactly these, whatever else is inheritable in this process.
            let handles = [nul_h, write_h];
            let mut size = 0usize;
            let _ = InitializeProcThreadAttributeList(None, 1, None, &mut size);
            let mut buf = vec![0u8; size];
            let list = LPPROC_THREAD_ATTRIBUTE_LIST(buf.as_mut_ptr().cast());
            InitializeProcThreadAttributeList(Some(list), 1, None, &mut size)?;
            let created = (|| -> windows::core::Result<OwnedHandle> {
                UpdateProcThreadAttribute(
                    list,
                    0,
                    PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                    Some(handles.as_ptr().cast()),
                    size_of_val(&handles),
                    None,
                    None,
                )?;
                let mut si = STARTUPINFOEXW::default();
                si.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
                si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
                si.StartupInfo.hStdInput = nul_h;
                si.StartupInfo.hStdOutput = nul_h;
                si.StartupInfo.hStdError = write_h;
                si.lpAttributeList = list;
                let exe_w = HSTRING::from(exe.as_os_str());
                let line: Vec<String> =
                    std::iter::once(exe.to_string_lossy().into_owned()).chain(args.iter().cloned()).map(|a| quote_arg(&a)).collect();
                let mut line: Vec<u16> = line.join(" ").encode_utf16().chain(Some(0)).collect();
                let mut pi = PROCESS_INFORMATION::default();
                // Break away from the caller's job, if it has one, so the host outlives
                // this command when the job is closed. Not every job allows it.
                let mut result = Ok(());
                for flags in [EXTENDED_STARTUPINFO_PRESENT | CREATE_BREAKAWAY_FROM_JOB, EXTENDED_STARTUPINFO_PRESENT] {
                    result = CreateProcessW(
                        PCWSTR(exe_w.as_ptr()),
                        Some(PWSTR(line.as_mut_ptr())),
                        None,
                        None,
                        true,
                        flags,
                        None,
                        None,
                        &si.StartupInfo,
                        &mut pi,
                    );
                    if result.is_ok() {
                        break;
                    }
                }
                result?;
                let _ = CloseHandle(pi.hThread);
                Ok(OwnedHandle::from_raw_handle(pi.hProcess.0))
            })();
            DeleteProcThreadAttributeList(list);
            // `write` and `nul` close here, so the host has the only write end of the pipe.
            Ok(HostProcess { process: created?, stderr })
        }
    }

    fn exit_code(&self) -> Option<u32> {
        let h = HANDLE(self.process.as_raw_handle());
        unsafe {
            if WaitForSingleObject(h, 0) != WAIT_OBJECT_0 {
                return None;
            }
            let mut code = 0;
            GetExitCodeProcess(h, &mut code).ok()?;
            Some(code)
        }
    }
}

/// Quote one argument for a Windows command line, by the rules CommandLineToArgvW reads.
fn quote_arg(a: &str) -> String {
    if !a.is_empty() && !a.contains([' ', '\t', '"']) {
        return a.to_string();
    }
    let mut out = String::from('"');
    let mut slashes = 0;
    for c in a.chars() {
        match c {
            '\\' => slashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(slashes * 2 + 1));
                out.push('"');
                slashes = 0;
            }
            c => {
                out.push_str(&"\\".repeat(slashes));
                out.push(c);
                slashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(slashes * 2));
    out.push('"');
    out
}

/// Validate a config the way a launch would, without launching anything.
fn check(config_arg: Option<&str>, sel: &Selection) -> Result<String, Failure> {
    let path = config::resolve_path(config_arg.map(PathBuf::from));
    let path = std::path::absolute(&path).unwrap_or(path);
    let loaded = match config::load(&path, sel) {
        Ok(l) => l,
        Err(e) => {
            println!("{e}");
            return Err((1, "the config is invalid".into()));
        }
    };
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_default();
    let problems: Vec<String> = loaded
        .entries
        .iter()
        .filter_map(|e| {
            crate::child::check_launch(e, &exe_dir).err().map(|err| format!("{}:{}: {}: {err}", path.display(), e.line, e.name()))
        })
        .collect();
    if !problems.is_empty() {
        println!("{}", problems.join("\n"));
        let n = problems.len();
        return Err((1, format!("{n} {} can't be launched", if n == 1 { "entry" } else { "entries" })));
    }
    let names = |task: bool| -> Vec<&str> { loaded.entries.iter().filter(|e| e.task == task).map(|e| e.name()).collect() };
    let order: Vec<&str> = config::start_order(&loaded.deps).iter().map(|&i| loaded.entries[i].name()).collect();
    let mut out = vec![format!("ok: {}", path.display())];
    if !loaded.selection.profiles.is_empty() {
        out.push(format!("profiles: {}", loaded.selection.profiles.join(", ")));
    }
    let tasks = names(true);
    if !tasks.is_empty() {
        out.push(format!("tasks: {}", tasks.join(", ")));
    }
    out.push(format!("apps: {}", names(false).join(", ")));
    out.push(format!("start order: {}", order.join(", ")));
    Ok(out.join("\n"))
}

/// The status of a host already running `config`, if there is one.
pub fn running_host(config: &Path) -> Option<Value> {
    request(&config_pipe(config)?, &["status"]).ok()
}

/// Bring a running host's window to the front. Called from the process the user just
/// started, which may hand foreground rights on to the host.
pub fn activate(status: &Value) -> bool {
    let pid = status["host_pid"].as_u64().unwrap_or(0) as u32;
    unsafe {
        let _ = AllowSetForegroundWindow(pid);
    }
    request(&pid_pipe(pid), &["activate"]).is_ok()
}

/// Lowercased app names in a status, sorted, to compare selections.
pub fn app_names(status: &Value) -> Vec<String> {
    let mut v: Vec<String> = apps(status).filter_map(|a| a["name"].as_str()).map(str::to_lowercase).collect();
    v.sort();
    v
}

fn apps(status: &Value) -> impl Iterator<Item = &Value> {
    status["apps"].as_array().into_iter().flatten()
}

/// Wait until every app in `scope` (and what it depends on), or every app, is ready.
fn wait_ready(pipe: &str, scope: Option<&[String]>, timeout: Duration) -> Result<String, Failure> {
    let start = Instant::now();
    loop {
        let status = request(pipe, &["status"]).map_err(|(code, e)| match code {
            2 => (4, "brady-bunch exited before everything was ready".to_string()),
            _ => (code, e),
        })?;
        if status["shutting_down"] == true {
            return Err((4, "the host is shutting down".into()));
        }
        let all: Vec<&Value> = apps(&status).collect();
        let wanted = in_scope(&all, scope).map_err(|e| (3, e))?;
        let mut pending = Vec::new();
        let mut failed = Vec::new();
        for a in all.iter().filter(|a| wanted.contains(&name_of(a).to_lowercase())) {
            match standing(a) {
                Standing::Ready => {}
                Standing::Failed => failed.push(*a),
                Standing::Pending => pending.push(*a),
            }
        }
        if !failed.is_empty() {
            println!("{}", pretty(&status));
            let why: Vec<String> = failed.iter().map(|a| describe(a)).collect();
            let first = name_of(failed[0]);
            return Err((4, format!("{}\n(brady-ctl tail {first} shows its console)", why.join("\n"))));
        }
        if pending.is_empty() {
            let names: Vec<&str> = all.iter().map(|a| name_of(a)).filter(|n| wanted.contains(&n.to_lowercase())).collect();
            return Ok(format!("ready: {} ({:.1}s)", names.join(", "), start.elapsed().as_secs_f64()));
        }
        if start.elapsed() >= timeout {
            println!("{}", pretty(&status));
            let why: Vec<String> = pending.iter().map(|a| describe(a)).collect();
            return Err((4, format!("timed out after {}s:\n{}", timeout.as_secs(), why.join("\n"))));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// How an app stands for a command waiting on it.
#[derive(PartialEq)]
enum Standing {
    Ready,
    Pending,
    /// Failed, exited or stopped, with no restart coming.
    Failed,
}

fn standing(a: &Value) -> Standing {
    if a["ready"] == true {
        return Standing::Ready;
    }
    let restarting = !a["restart_in_ms"].is_null();
    match a["state"].as_str().unwrap_or("") {
        "failed" | "exited" if !restarting => Standing::Failed,
        "stopped" => Standing::Failed,
        _ => Standing::Pending,
    }
}

/// `brady-ctl run`: wait for the task, then for the apps it restarts, and say what
/// happened to each.
fn wait_run(pipe: &str, task: &str, restarting: &[String], start: Instant, timeout: Duration) -> Result<String, Failure> {
    let restarted = if restarting.is_empty() { String::new() } else { format!("; restarted {}", restarting.join(", ")) };
    let mut task_done: Option<f64> = None;
    loop {
        let status = request(pipe, &["status"]).map_err(|(code, e)| match code {
            2 => (4, format!("brady-bunch exited while {task} was running")),
            _ => (code, e),
        })?;
        if status["shutting_down"] == true {
            return Err((4, "the host is shutting down".into()));
        }
        let all: Vec<&Value> = apps(&status).collect();
        let find = |n: &str| all.iter().copied().find(|a| name_of(a).eq_ignore_ascii_case(n));
        let t = find(task).ok_or_else(|| (3, format!("no task called \"{task}\"")))?;
        let secs = start.elapsed().as_secs_f64();
        if task_done.is_none() {
            match t["state"].as_str().unwrap_or("") {
                "done" => task_done = Some(secs),
                "skipped" => {
                    let why = t["reason"].as_str().unwrap_or("skipped");
                    let why = why.split("; brady-ctl run").next().unwrap_or(why);
                    return Ok(format!("{task}: skipped ({why}); nothing was restarted"));
                }
                "exited" | "failed" | "stopped" => {
                    println!("{}", pretty(&status));
                    let left = if restarting.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "\nwaiting for {task} to succeed: {}. Fix the error, then brady-ctl run {task} again.",
                            restarting.join(", ")
                        )
                    };
                    return Err((4, format!("{} ({secs:.1}s){left}\n(brady-ctl tail {task} shows its console)", describe(t))));
                }
                _ => {}
            }
        }
        if let Some(done) = task_done {
            // The restarted apps, and anything they wait for.
            let wanted = in_scope(&all, Some(restarting)).map_err(|e| (3, e))?;
            let scoped: Vec<&Value> =
                all.iter().copied().filter(|a| wanted.contains(&name_of(a).to_lowercase()) && !name_of(a).eq_ignore_ascii_case(task)).collect();
            let failed: Vec<&Value> = scoped.iter().copied().filter(|a| standing(a) == Standing::Failed).collect();
            if !failed.is_empty() {
                println!("{}", pretty(&status));
                let why: Vec<String> = failed.iter().map(|a| describe(a)).collect();
                let first = name_of(failed[0]);
                return Err((
                    4,
                    format!("{task}: done ({done:.1}s){restarted}; then:\n{}\n(brady-ctl tail {first} shows its console)", why.join("\n")),
                ));
            }
            if scoped.iter().all(|a| standing(a) == Standing::Ready) {
                let ready = if restarting.is_empty() { String::new() } else { format!("; ready ({secs:.1}s)") };
                return Ok(format!("{task}: done ({done:.1}s){restarted}{ready}"));
            }
            if start.elapsed() >= timeout {
                println!("{}", pretty(&status));
                let why: Vec<String> = scoped.iter().filter(|a| standing(a) == Standing::Pending).map(|a| describe(a)).collect();
                return Err((
                    4,
                    format!("{task}: done ({done:.1}s){restarted}; timed out after {}s:\n{}", timeout.as_secs(), why.join("\n")),
                ));
            }
        } else if start.elapsed() >= timeout {
            println!("{}", pretty(&status));
            let left = if restarting.is_empty() { String::new() } else { format!("\n{} are waiting for it", restarting.join(", ")) };
            return Err((4, format!("timed out after {}s:\n{}{left}", timeout.as_secs(), describe(t))));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// `brady-ctl events`: the end of the host's event log, read from the file, so it
/// works after the host has exited.
fn events(target: Option<&str>, lines: u32, app: Option<&str>) -> Result<String, Failure> {
    let by_pid = target.is_some_and(|t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()));
    let pipe = target_pipe(target)?;
    let path = match request(&pipe, &["status"]) {
        Ok(status) => status["event_log"].as_str().map(PathBuf::from),
        Err((2, _)) if by_pid => {
            return Err((2, "no running brady-bunch has that pid; pass the config path to read the log it left".into()));
        }
        Err((2, _)) => {
            let config = config::resolve_path(target.map(PathBuf::from));
            config::event_log_path(&config).map_err(|e| (1, e))?
        }
        Err(e) => return Err(e),
    };
    let path = path.ok_or_else(|| (1, "the config has no event_log; add event_log = 'brady-events.log' to record events".to_string()))?;
    let bytes = std::fs::read(&path).map_err(|e| (1, format!("couldn't read {}: {e}", path.display())))?;
    let text = String::from_utf8_lossy(&bytes);
    Ok(tail_events(&text, lines as usize, app))
}

/// The last `lines` lines of an event log, only `app`'s if given.
fn tail_events(text: &str, lines: usize, app: Option<&str>) -> String {
    let prefix = app.map(|a| format!("{}:", a.trim().to_lowercase()));
    let picked: Vec<&str> = text
        .lines()
        .filter(|l| prefix.as_ref().is_none_or(|p| crate::events::line_text(l).to_lowercase().starts_with(p.as_str())))
        .collect();
    let mut out = picked[picked.len().saturating_sub(lines.max(1))..].join("\n");
    out.push('\n');
    out
}

fn name_of(a: &Value) -> &str {
    a["name"].as_str().unwrap_or("")
}

/// `api: exited with code 1`
fn describe(a: &Value) -> String {
    let reason = a["reason"].as_str().map(String::from).unwrap_or_else(|| a["state"].as_str().unwrap_or("").to_string());
    format!("{}: {reason}", name_of(a))
}

/// Lowercased names of the apps in `scope` and everything they depend on.
fn in_scope(all: &[&Value], scope: Option<&[String]>) -> Result<Vec<String>, String> {
    let Some(scope) = scope else { return Ok(all.iter().map(|a| name_of(a).to_lowercase()).collect()) };
    let mut out: Vec<String> = Vec::new();
    let mut stack: Vec<String> = Vec::new();
    for s in scope {
        if !all.iter().any(|a| name_of(a).eq_ignore_ascii_case(s)) {
            let names: Vec<&str> = all.iter().map(|a| name_of(a)).collect();
            return Err(format!("no app called \"{s}\"; the apps are: {}", names.join(", ")));
        }
        stack.push(s.to_lowercase());
    }
    while let Some(n) = stack.pop() {
        if out.contains(&n) {
            continue;
        }
        if let Some(a) = all.iter().find(|a| name_of(a).eq_ignore_ascii_case(&n)) {
            stack.extend(a["depends_on"].as_array().into_iter().flatten().filter_map(|d| d.as_str()).map(str::to_lowercase));
        }
        out.push(n);
    }
    Ok(out)
}

/// Shut the host down and wait for it to exit. Exit 4 if an app had to be killed.
fn down(pipe: &str, timeout: Duration) -> Result<String, Failure> {
    let status = match request(pipe, &["status"]) {
        Ok(s) => s,
        Err((2, _)) => return Ok("no brady-bunch is running for that config".into()),
        Err(e) => return Err(e),
    };
    let pid = status["host_pid"].as_u64().unwrap_or(0) as u32;
    let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok();
    let (tx, rx) = mpsc::channel();
    let p = pipe.to_string();
    std::thread::spawn(move || {
        let _ = tx.send(request(&p, &["quit"]));
    });
    let started = Instant::now();
    let last = match rx.recv_timeout(timeout) {
        Ok(Ok(v)) => Some(v),
        // The host went away without answering, e.g. it was killed.
        Ok(Err(_)) => None,
        Err(_) => return Err((4, format!("brady-bunch (pid {pid}) is still shutting down after {}s", timeout.as_secs()))),
    };
    if let Some(h) = process {
        let left = timeout.saturating_sub(started.elapsed()).max(Duration::from_secs(5));
        unsafe {
            WaitForSingleObject(h, left.as_millis() as u32);
            let _ = CloseHandle(h);
        }
    }
    let Some(last) = last else { return Ok(format!("brady-bunch (pid {pid}) exited")) };
    let mut lines = Vec::new();
    let mut killed = Vec::new();
    for a in apps(&last) {
        if let Some(by) = a["last_stop"]["by"].as_str() {
            lines.push(format!("{}: stopped by {by} after {} ms", name_of(a), a["last_stop"]["ms"]));
            if by == "kill" {
                killed.push(name_of(a));
            }
        }
    }
    lines.push(format!("brady-bunch (pid {pid}) exited after {:.1}s", started.elapsed().as_secs_f64()));
    if killed.is_empty() {
        return Ok(lines.join("\n"));
    }
    println!("{}", lines.join("\n"));
    Err((4, format!("killed after the graceful steps timed out: {}", killed.join(", "))))
}

fn send(pipe: &str, request: &str) -> Result<String, String> {
    const ERROR_PIPE_BUSY: i32 = 231;
    const ERROR_PIPE_NOT_CONNECTED: i32 = 233;
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut f = loop {
        match std::fs::OpenOptions::new().read(true).write(true).open(pipe) {
            Ok(f) => break f,
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err("no running brady-bunch found for that config or pid".into());
            }
            Err(e) => return Err(format!("couldn't connect to the host: {e}")),
        }
    };
    f.write_all(request.as_bytes()).map_err(|e| e.to_string())?;
    let mut reply = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => reply.extend_from_slice(&buf[..n]),
            // The host closed its end: that's the end of the reply.
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_NOT_CONNECTED) => break,
            Err(e) => return Err(e.to_string()),
        }
    }
    if reply.is_empty() {
        return Err("the host closed the connection without answering".into());
    }
    String::from_utf8(reply).map_err(|e| e.to_string())
}

/// brady-bunch.exe is a GUI program, so it has no stdout unless the caller redirected
/// it. When run from a console without redirection, write to that console instead.
fn attach_output() {
    unsafe {
        let h = GetStdHandle(STD_OUTPUT_HANDLE);
        if h.is_err() || h.is_ok_and(|h| h.is_invalid() || h.0.is_null()) {
            let _ = AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_arg_round_trips_through_the_command_line_rules() {
        assert_eq!(quote_arg("plain"), "plain");
        assert_eq!(quote_arg(""), "\"\"");
        assert_eq!(quote_arg(r"C:\my configs\brady.toml"), r#""C:\my configs\brady.toml""#);
        assert_eq!(quote_arg(r"C:\dir with space\"), r#""C:\dir with space\\""#);
        assert_eq!(quote_arg(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote_arg(r#"a\"b"#), r#""a\\\"b""#);
    }

    #[test]
    fn up_takes_exact_as_a_flag() {
        let args: Vec<String> = ["cfg.toml", "--exact", "--skip", "worker"].iter().map(|s| s.to_string()).collect();
        let o = parse_opts("up", &args).unwrap();
        assert!(o.exact);
        assert_eq!(o.positional, ["cfg.toml"]);
        assert_eq!(o.sel.skip, ["worker"]);
        assert!(parse_opts("run", &["--exact".to_string()]).is_err());
    }

    #[test]
    fn config_pipe_ignores_case_and_relative_spelling() {
        let dir = std::env::temp_dir();
        let file = dir.join(format!("bb-pipe-{}.toml", std::process::id()));
        std::fs::write(&file, "").unwrap();
        let upper = PathBuf::from(file.to_string_lossy().to_uppercase());
        let dotted = dir.join(".").join(file.file_name().unwrap());
        let a = config_pipe(&file).unwrap();
        assert_eq!(config_pipe(&upper).as_ref(), Some(&a));
        assert_eq!(config_pipe(&dotted).as_ref(), Some(&a));
        assert!(config_pipe(&dir.join("missing.toml")).is_none());
        // With `..` parts, through a folder that doesn't exist.
        let parent = dir.join("no-such-dir").join("..").join(file.file_name().unwrap());
        assert_eq!(config_pipe(&parent).as_ref(), Some(&a));
        let _ = std::fs::remove_file(&file);
        // Relative to the working directory, as `brady-ctl <cmd> brady.toml` gets it.
        let name = format!("bb-pipe-rel-{}.toml", std::process::id());
        std::fs::write(&name, "").unwrap();
        let cwd = std::env::current_dir().unwrap();
        let here = cwd.file_name().unwrap().to_string_lossy().into_owned();
        let abs = config_pipe(&cwd.join(&name)).unwrap();
        for spelling in [name.clone(), format!(r".\{name}"), format!(r"..\{here}\{name}")] {
            assert_eq!(config_pipe(&config::resolve_path(Some(spelling.clone().into()))).as_ref(), Some(&abs), "{spelling}");
            assert_eq!(config_pipe(Path::new(&spelling)).as_ref(), Some(&abs), "{spelling}");
        }
        let _ = std::fs::remove_file(&name);
    }

    #[test]
    fn scope_includes_dependencies() {
        let status = json!({ "apps": [
            { "name": "build", "depends_on": [] },
            { "name": "db", "depends_on": ["build"] },
            { "name": "api", "depends_on": ["build", "db"] },
            { "name": "web", "depends_on": ["build"] },
        ]});
        let all: Vec<&Value> = apps(&status).collect();
        let mut s = in_scope(&all, Some(&["API".to_string()])).unwrap();
        s.sort();
        assert_eq!(s, ["api", "build", "db"]);
        assert_eq!(in_scope(&all, None).unwrap().len(), 4);
        assert!(in_scope(&all, Some(&["nope".to_string()])).is_err());
    }

    #[test]
    fn options_are_checked_per_verb() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let o = parse_opts("up", &args(&["x.toml", "--profile", "webdev,quick", "--skip", "a", "--timeout", "60"])).unwrap();
        assert_eq!(o.positional, ["x.toml"]);
        assert_eq!(o.sel.profiles, ["webdev", "quick"]);
        assert_eq!(o.sel.skip, ["a"]);
        assert_eq!(o.timeout, Some(Duration::from_secs(60)));
        assert!(parse_opts("status", &args(&["--timeout", "5"])).is_err());
        assert!(parse_opts("wait", &args(&["api", "--timeout"])).is_err());
        let o = parse_opts("run", &args(&["build", "--stop", "api, worker"])).unwrap();
        assert_eq!(o.stop, ["api", "worker"]);
        let o = parse_opts("events", &args(&["x.toml", "--app", "api", "--lines", "5"])).unwrap();
        assert_eq!((o.app.as_deref(), o.lines), (Some("api"), Some(5)));
        assert!(parse_opts("tail", &args(&["api", "--app", "x"])).is_err());
    }

    #[test]
    fn events_tail_filters_by_app() {
        let log = "2026-11-25 10:15:39.300-06:00 host started (pid 1) with C:\\a\\brady.toml\n\
                   2026-11-25 10:15:39.315-06:00 api: launched (pid 12)\n\
                   2026-11-25 10:15:39.320-06:00 api-worker: launched (pid 13)\n\
                   2026-11-25 10:15:40.001-06:00 API: ready after 685 ms\n\
                   2026-11-25 10:15:41.000-06:00 web: launched (pid 14)\n";
        assert_eq!(tail_events(log, 2, None).lines().count(), 2);
        assert!(tail_events(log, 2, None).ends_with("web: launched (pid 14)\n"));
        let api = tail_events(log, 40, Some("api"));
        assert_eq!(api.lines().count(), 2, "{api}");
        assert!(api.contains("api: launched") && api.contains("API: ready"), "{api}");
        assert_eq!(tail_events(log, 40, Some("nope")), "\n");
    }
}
