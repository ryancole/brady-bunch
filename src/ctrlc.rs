//! Ctrl+C delivery. A process can only send console control events to the console
//! it is attached to, and attaching changes process-wide state, so the host runs a
//! copy of itself in helper mode for each signal instead of attaching in-process.

use std::path::Path;
use std::process::Command;
use std::time::Duration;
use windows::Win32::System::Console::{
    AttachConsole, CTRL_C_EVENT, FreeConsole, GenerateConsoleCtrlEvent, SetConsoleCtrlHandler,
};

pub const HELPER_FLAG: &str = "--send-ctrl-c";

/// Helper-mode entry point: attach to `pid`'s console and raise Ctrl+C on it.
/// Exit codes: 0 sent, 1 bad pid, 2 attach failed, 3 send failed.
pub fn helper_main(pid: u32) -> i32 {
    if pid == 0 {
        return 1;
    }
    unsafe {
        let _ = FreeConsole();
        if AttachConsole(pid).is_err() {
            return 2;
        }
        // Don't let the event we're about to raise kill this helper.
        let _ = SetConsoleCtrlHandler(None, true);
        // Group 0 = every process attached to this console.
        let sent = GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0).is_ok();
        // Delivery is asynchronous; stay attached briefly so it lands.
        std::thread::sleep(Duration::from_millis(50));
        let _ = FreeConsole();
        if sent { 0 } else { 3 }
    }
}

/// Try each pid until one is attached to a console; returns true once Ctrl+C was sent.
pub fn send_to_any(helper_exe: &Path, pids: &[u32]) -> bool {
    pids.iter().any(|pid| {
        Command::new(helper_exe)
            .arg(HELPER_FLAG)
            .arg(pid.to_string())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}
