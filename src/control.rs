//! Control from the command line: `--status`, `--start`, `--stop`, `--restart` and
//! `--tail` talk to a running host over a named pipe.
//!
//! Each host listens on `\\.\pipe\brady-bunch.pid.<host pid>` and, unless another host
//! already has it, on `\\.\pipe\brady-bunch.config.<hash of the config path>`. A request
//! is one line, `verb<TAB>app<TAB>lines`; the reply is a JSON object, `{"ok": true,
//! "result": ...}` or `{"ok": false, "error": "..."}`, and the host then closes the pipe.

use serde_json::{Value, json};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{ERROR_PIPE_CONNECTED, HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, FlushFileBuffers, PIPE_ACCESS_DUPLEX};
use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_OUTPUT_HANDLE};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
use windows::core::HSTRING;

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
}

pub struct Request {
    pub cmd: Cmd,
    pub reply: mpsc::Sender<Result<Value, String>>,
}

pub const VERBS: [&str; 5] = ["--status", "--start", "--stop", "--restart", "--tail"];

pub const USAGE: &str = "usage: brady-ctl status [config|pid]
       brady-ctl start|stop|restart <app> [config|pid]
       brady-ctl tail <app> [config|pid] [--lines N]

The host is found by its config file (default: brady.toml in the working directory,
else next to the exe) or by its process id.";

fn pid_pipe(pid: u32) -> String {
    format!(r"\\.\pipe\brady-bunch.pid.{pid}")
}

fn config_pipe(config: &Path) -> Option<String> {
    let canonical = std::fs::canonicalize(config).ok()?;
    let key = canonical.to_string_lossy().to_lowercase();
    // FNV-1a: stable across builds, unlike std's hasher.
    let hash = key.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3));
    Some(format!(r"\\.\pipe\brady-bunch.config.{hash:016x}"))
}

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
    let reply = match handle(line.trim_end_matches(['\r', '\n']), hwnd, helper) {
        Ok(v) => json!({ "ok": true, "result": v }),
        Err(e) => json!({ "ok": false, "error": e }),
    };
    let _ = pipe.write_all(reply.to_string().as_bytes());
    // Let the client read everything before the handle closes. Closing (rather than
    // DisconnectNamedPipe) gives the client a clean end-of-file.
    let _ = unsafe { FlushFileBuffers(HANDLE(pipe.as_raw_handle())) };
}

fn handle(line: &str, hwnd: isize, helper: &Path) -> Result<Value, String> {
    let mut parts = line.split('\t');
    let verb = parts.next().unwrap_or("");
    let app = parts.next().unwrap_or("").to_string();
    match verb {
        "status" => ask(hwnd, Cmd::Status),
        "start" => ask(hwnd, Cmd::Start(app)),
        "stop" => {
            ask(hwnd, Cmd::Stop(app.clone()))?;
            settled(hwnd, &app)
        }
        "restart" => {
            ask(hwnd, Cmd::Restart(app.clone()))?;
            settled(hwnd, &app)
        }
        "tail" => {
            let lines = parts.next().and_then(|n| n.parse().ok()).unwrap_or(40);
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

/// Hand a command to the UI thread and wait for its answer.
fn ask(hwnd: isize, cmd: Cmd) -> Result<Value, String> {
    let (tx, rx) = mpsc::channel();
    let req = Box::into_raw(Box::new(Request { cmd, reply: tx }));
    let posted = unsafe { PostMessageW(Some(HWND(hwnd as *mut _)), WM_APP_CONTROL, WPARAM(0), LPARAM(req as isize)) };
    if posted.is_err() {
        // SAFETY: the message wasn't posted, so nothing else owns the request.
        drop(unsafe { Box::from_raw(req) });
        return Err("the host is closing".into());
    }
    rx.recv_timeout(Duration::from_secs(10)).map_err(|_| "the host didn't answer".to_string())?
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

// ---- Client side ----

/// Run a control command from the command line. Exit codes: 0 done, 1 usage error,
/// 2 no host found, 3 the host refused the request.
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
            eprintln!("brady-bunch: {msg}");
            code
        }
    }
}

fn client(args: &[String]) -> Result<String, (i32, String)> {
    let usage = |m: &str| (1, m.to_string());
    // `--status` (brady-bunch.exe) or `status` (brady-ctl.exe).
    let verb = args.first().map(|a| a.trim_start_matches("--")).unwrap_or("");
    if !VERBS.iter().any(|v| v.trim_start_matches("--") == verb) {
        return Err(usage(&format!("unknown command {verb}

{USAGE}")));
    }
    let mut rest: Vec<&str> = args[1..].iter().map(String::as_str).collect();
    let mut lines = 40u32;
    if let Some(i) = rest.iter().position(|a| *a == "--lines") {
        lines = rest.get(i + 1).and_then(|n| n.parse().ok()).ok_or_else(|| usage("--lines needs a number"))?;
        rest.drain(i..=i + 1);
    }
    let (app, target) = match verb {
        "status" => (None, rest.first().copied()),
        _ => {
            let app = rest.first().copied().ok_or_else(|| usage(&format!("{verb} needs an app name")))?;
            (Some(app), rest.get(1).copied())
        }
    };
    let pipe = match target {
        Some(t) if !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()) => pid_pipe(t.parse().unwrap_or(0)),
        t => {
            let path = crate::config::resolve_path(t.map(PathBuf::from));
            config_pipe(&path).ok_or_else(|| (2, format!("config {} not found", path.display())))?
        }
    };
    let request = format!("{verb}\t{}\t{lines}\n", app.unwrap_or(""));
    let reply = send(&pipe, &request).map_err(|e| (2, e))?;
    let reply: Value = serde_json::from_str(&reply).map_err(|e| (3, format!("bad reply from host: {e}")))?;
    if reply["ok"] != true {
        return Err((3, reply["error"].as_str().unwrap_or("request failed").to_string()));
    }
    Ok(match &reply["result"] {
        Value::String(s) => s.clone(),
        v => serde_json::to_string_pretty(v).unwrap_or_default(),
    })
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
        let _ = std::fs::remove_file(&file);
    }
}
