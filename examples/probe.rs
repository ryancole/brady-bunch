//! Test child: prints a tick every second and records which console control event
//! it received (to the file given as the first argument) before exiting.
//!
//!     cargo build --example probe

use std::io::Write;
use std::sync::OnceLock;
use windows::Win32::System::Console::*;
use windows::core::BOOL;

static LOG: OnceLock<String> = OnceLock::new();

unsafe extern "system" fn handler(event: u32) -> BOOL {
    let name = match event {
        CTRL_C_EVENT => "ctrl-c",
        CTRL_BREAK_EVENT => "ctrl-break",
        CTRL_CLOSE_EVENT => "close",
        _ => "other",
    };
    if let Some(path) = LOG.get() {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "{} {name}", std::process::id());
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
    unsafe { SetConsoleCtrlHandler(Some(handler), true).unwrap() };
    for i in 0.. {
        println!("probe {} tick {i}", std::process::id());
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
