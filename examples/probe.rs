//! Test child: prints a tick every second and records which console control event
//! it received before exiting, as `<label> <event>` lines appended to a log file.
//!
//!     probe.exe <logfile> [label]    (label defaults to the pid)
//!
//!     cargo build --example probe

use std::io::Write;
use std::sync::OnceLock;
use windows::Win32::System::Console::*;
use windows::core::BOOL;

static LOG: OnceLock<String> = OnceLock::new();
static LABEL: OnceLock<String> = OnceLock::new();

unsafe extern "system" fn handler(event: u32) -> BOOL {
    let name = match event {
        CTRL_C_EVENT => "ctrl-c",
        CTRL_BREAK_EVENT => "ctrl-break",
        CTRL_CLOSE_EVENT => "close",
        _ => "other",
    };
    if let Some(path) = LOG.get() {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let label = LABEL.get().cloned().unwrap_or_else(|| std::process::id().to_string());
            let _ = writeln!(f, "{label} {name}");
        }
    }
    println!("got {name}, cleaning up...");
    std::thread::sleep(std::time::Duration::from_millis(300));
    std::process::exit(0);
}

fn main() {
    if let Some(p) = std::env::args().nth(1) {
        let _ = LOG.set(p);
    }
    if let Some(l) = std::env::args().nth(2) {
        let _ = LABEL.set(l);
    }
    unsafe { SetConsoleCtrlHandler(Some(handler), true).unwrap() };
    for i in 0.. {
        println!("probe {} tick {i}", std::process::id());
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
