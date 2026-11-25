//! Console helpers. A process can only send control events to, or read the screen of,
//! the console it is attached to, and attaching changes process-wide state, so the host
//! runs a copy of itself in helper mode for each of these instead of attaching in-process.

use std::path::Path;
use std::process::Command;
use std::time::Duration;
use windows::Win32::Foundation::GENERIC_READ;
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, WriteFile,
};
use windows::Win32::System::Console::{
    AttachConsole, CONSOLE_SCREEN_BUFFER_INFO, COORD, CTRL_C_EVENT, FreeConsole, GenerateConsoleCtrlEvent,
    GetConsoleScreenBufferInfo, GetStdHandle, ReadConsoleOutputCharacterW, STD_OUTPUT_HANDLE, SetConsoleCtrlHandler,
};
use windows::core::w;

pub const HELPER_FLAG: &str = "--send-ctrl-c";
pub const READ_FLAG: &str = "--read-console";

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

/// Helper-mode entry point: write the last `lines` lines of `pid`'s console, up to the
/// cursor, to stdout. Exit codes: 0 done, 1 bad pid, 2 attach failed, 3 read failed.
pub fn read_main(pid: u32, lines: u32) -> i32 {
    if pid == 0 {
        return 1;
    }
    unsafe {
        // Attaching replaces the standard handles, so keep the one the caller is reading.
        let Ok(out) = GetStdHandle(STD_OUTPUT_HANDLE) else { return 3 };
        let _ = FreeConsole();
        if AttachConsole(pid).is_err() {
            return 2;
        }
        let _ = SetConsoleCtrlHandler(None, true);
        let text = read_screen(lines);
        let _ = FreeConsole();
        let Some(text) = text else { return 3 };
        let _ = WriteFile(out, Some(text.as_bytes()), None, None);
        0
    }
}

unsafe fn read_screen(lines: u32) -> Option<String> {
    unsafe {
        let screen = CreateFileW(
            w!("CONOUT$"),
            GENERIC_READ.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
        .ok()?;
        let screen = crate::child::Handle(screen);
        let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
        GetConsoleScreenBufferInfo(screen.0, &mut info).ok()?;
        let width = info.dwSize.X.max(1) as usize;
        // The cursor's row is usually the empty one after the last line written.
        let cursor = info.dwCursorPosition;
        let last = cursor.Y as i32 - (cursor.X == 0 && cursor.Y > 0) as i32;
        let first = (last - lines.max(1) as i32 + 1).max(0);
        let mut rows = Vec::new();
        let mut buf = vec![0u16; width];
        for y in first..=last {
            let mut read = 0u32;
            ReadConsoleOutputCharacterW(screen.0, &mut buf, COORD { X: 0, Y: y as i16 }, &mut read).ok()?;
            rows.push(String::from_utf16_lossy(&buf[..read as usize]).trim_end().to_string());
        }
        while rows.last().is_some_and(String::is_empty) {
            rows.pop();
        }
        let mut text = rows.join("\n");
        text.push('\n');
        Some(text)
    }
}

/// Read the console text of whichever of `pids` is attached to a console.
pub fn read_any(helper_exe: &Path, pids: &[u32], lines: u32) -> Option<String> {
    pids.iter().find_map(|pid| {
        let out = Command::new(helper_exe).arg(READ_FLAG).arg(pid.to_string()).arg(lines.to_string()).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    })
}
