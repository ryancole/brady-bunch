//! Console-side launcher. brady-bunch runs `conhost.exe brady-shim.exe` for each
//! console app, and this program starts the real command inside that console.
//!
//! - **Exact command line.** conhost re-tokenizes and rebuilds its client's command
//!   line, which loses quotes (`cmd /c ""C:\a b\x.exe" --flag"` arrives without the
//!   inner ones). The command comes in through the environment instead, untouched.
//! - **QuickEdit off**, so a click on the tile can't start a selection that blocks
//!   the app's next console write.
//! - **Output copy** (`log`): the app's stdout/stderr go through a pipe that is echoed
//!   to the console and appended to a file with escape sequences stripped.
//! - **Keeps the console open** after the app exits, so its last output stays readable,
//!   by handing the console to `brady-shim.exe --hold <code>` and exiting with the
//!   app's exit code.
//!
//! Inputs (removed from the environment before the app starts):
//! `BRADY_SHIM_COMMAND` command line, `BRADY_SHIM_APP` resolved executable,
//! `BRADY_SHIM_LOG` output copy, `BRADY_SHIM_QUICK_EDIT=1` leave QuickEdit alone,
//! `BRADY_SHIM_HOLD=0` don't keep the console open.

use std::ffi::c_void;
use std::fs::File;
use std::io::Write;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation};
use windows::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows::Win32::System::Console::*;
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CreateProcessW, GetExitCodeProcess, INFINITE, PROCESS_CREATION_FLAGS, PROCESS_INFORMATION,
    STARTF_USESTDHANDLES, STARTUPINFOW, WaitForSingleObject,
};
use windows::Win32::UI::Input::KeyboardAndMouse::VK_RETURN;
use windows::core::{BOOL, HSTRING, PCWSTR, PWSTR};

const ENV_COMMAND: &str = "BRADY_SHIM_COMMAND";
const ENV_APP: &str = "BRADY_SHIM_APP";
const ENV_LOG: &str = "BRADY_SHIM_LOG";
const ENV_QUICK_EDIT: &str = "BRADY_SHIM_QUICK_EDIT";
const ENV_HOLD: &str = "BRADY_SHIM_HOLD";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    unsafe {
        // Ctrl+C and Ctrl+Break are for the app; this process stays to report its exit.
        let _ = SetConsoleCtrlHandler(Some(ignore_break), true);
    }
    if args.get(1).map(String::as_str) == Some("--hold") {
        let code = args.get(2).and_then(|c| c.parse().ok()).unwrap_or(0);
        std::process::exit(hold(code));
    }
    std::process::exit(run() as i32);
}

unsafe extern "system" fn ignore_break(event: u32) -> BOOL {
    BOOL::from(event == CTRL_C_EVENT || event == CTRL_BREAK_EVENT)
}

fn take_env(name: &str) -> Option<String> {
    let v = std::env::var(name).ok();
    // SAFETY: single-threaded at this point.
    unsafe { std::env::remove_var(name) };
    v.filter(|s| !s.is_empty())
}

fn run() -> u32 {
    let Some(command) = take_env(ENV_COMMAND) else {
        say("brady-shim: started without a command. It is run by brady-bunch.exe.\r\n");
        return 1;
    };
    let app = take_env(ENV_APP);
    let log_path = take_env(ENV_LOG);
    let quick_edit = take_env(ENV_QUICK_EDIT).as_deref() == Some("1");
    let keep_open = take_env(ENV_HOLD).as_deref() != Some("0");

    if !quick_edit {
        set_quick_edit(false);
    }
    let mut log = log_path.as_ref().and_then(|p| std::fs::OpenOptions::new().create(true).append(true).open(p).ok());
    if let Some(f) = &mut log {
        let _ = writeln!(f, "---- {} start: {command}", timestamp());
    }

    let code = match unsafe { launch(&command, app.as_deref(), log.as_ref()) } {
        Ok(code) => code,
        Err(e) => {
            let msg = format!("brady-bunch: could not start {command}\r\n{}\r\n", e.message());
            say(&msg);
            if let Some(f) = &mut log {
                let _ = f.write_all(msg.as_bytes());
            }
            (e.code().0 as u32) & 0xFFFF
        }
    };
    if let Some(f) = &mut log {
        let _ = writeln!(f, "---- {} exited with code {code}", timestamp());
    }
    if keep_open {
        // Hand the console to a holder so the output stays visible, then report the
        // exit code by exiting.
        let _ = std::process::Command::new(std::env::current_exe().unwrap_or_default())
            .arg("--hold")
            .arg(code.to_string())
            .spawn();
    }
    code
}

/// Run the app and wait for it. Returns its exit code.
unsafe fn launch(command: &str, app: Option<&str>, log: Option<&File>) -> windows::core::Result<u32> {
    unsafe {
        let mut si = STARTUPINFOW { cb: size_of::<STARTUPINFOW>() as u32, ..Default::default() };
        let mut pipe = None;
        if let Some(log) = log {
            let (mut read, mut write) = (HANDLE::default(), HANDLE::default());
            CreatePipe(&mut read, &mut write, None, 0)?;
            SetHandleInformation(write, HANDLE_FLAG_INHERIT.0, HANDLE_FLAG_INHERIT)?;
            let stdin = GetStdHandle(STD_INPUT_HANDLE)?;
            let _ = SetHandleInformation(stdin, HANDLE_FLAG_INHERIT.0, HANDLE_FLAG_INHERIT);
            si.dwFlags |= STARTF_USESTDHANDLES;
            si.hStdInput = stdin;
            si.hStdOutput = write;
            si.hStdError = write;
            pipe = Some((read, write, log.try_clone().ok()));
        }
        let mut cmd_w: Vec<u16> = command.encode_utf16().chain(Some(0)).collect();
        let app_h = app.map(HSTRING::from);
        let app_p = app_h.as_ref().map_or(PCWSTR::null(), |h| PCWSTR(h.as_ptr()));
        let mut pi = PROCESS_INFORMATION::default();
        let created = CreateProcessW(
            app_p,
            Some(PWSTR(cmd_w.as_mut_ptr())),
            None,
            None,
            pipe.is_some(),
            PROCESS_CREATION_FLAGS(0),
            None,
            PCWSTR::null(),
            &si,
            &mut pi,
        );
        // Close our copy of the write end so the reader sees EOF once the app's done.
        let reader = pipe.map(|(read, write, file)| {
            let _ = CloseHandle(write);
            let read = read.0 as isize;
            std::thread::spawn(move || copy_output(HANDLE(read as *mut c_void), file))
        });
        created?;
        let _ = CloseHandle(pi.hThread);
        WaitForSingleObject(pi.hProcess, INFINITE);
        let mut code = 0u32;
        let _ = GetExitCodeProcess(pi.hProcess, &mut code);
        let _ = CloseHandle(pi.hProcess);
        // Processes the app left running may still hold the pipe; don't wait for them.
        if let Some(r) = reader {
            let deadline = Instant::now() + Duration::from_millis(500);
            while !r.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        Ok(code)
    }
}

/// Echo the app's output to the console and append it, minus escape sequences, to the log.
fn copy_output(read: HANDLE, mut file: Option<File>) {
    unsafe {
        let out = GetStdHandle(STD_OUTPUT_HANDLE).unwrap_or_default();
        // Show colour sequences rather than printing them.
        let mut mode = CONSOLE_MODE::default();
        if GetConsoleMode(out, &mut mode).is_ok() {
            let _ = SetConsoleMode(out, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
        }
        let mut strip = AnsiStrip::default();
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            let mut n = 0u32;
            if ReadFile(read, Some(&mut buf), Some(&mut n), None).is_err() || n == 0 {
                break;
            }
            let chunk = &buf[..n as usize];
            let _ = WriteFile(out, Some(chunk), None, None);
            if let Some(f) = &mut file {
                let _ = f.write_all(&strip.feed(chunk));
            }
        }
        let _ = CloseHandle(read);
    }
}

/// Keep the console open after the app has exited, until Enter is pressed.
fn hold(code: u32) -> i32 {
    // The app is gone, so selecting text can't block anything.
    set_quick_edit(true);
    say(&format!("\r\n[exited with code {code}] Press Enter to run it again.\r\n"));
    unsafe {
        let Ok(input) = GetStdHandle(STD_INPUT_HANDLE) else { return 1 };
        let _ = FlushConsoleInputBuffer(input);
        let mut records = [INPUT_RECORD::default(); 16];
        loop {
            let mut n = 0u32;
            if ReadConsoleInputW(input, &mut records, &mut n).is_err() {
                return 1;
            }
            for r in &records[..n as usize] {
                if r.EventType == KEY_EVENT as u16 {
                    let k = r.Event.KeyEvent;
                    if k.bKeyDown.as_bool() && k.wVirtualKeyCode == VK_RETURN.0 {
                        return 0;
                    }
                }
            }
        }
    }
}

fn set_quick_edit(on: bool) {
    unsafe {
        let Ok(input) = GetStdHandle(STD_INPUT_HANDLE) else { return };
        let mut mode = CONSOLE_MODE::default();
        if GetConsoleMode(input, &mut mode).is_err() {
            return;
        }
        // QuickEdit changes only take effect together with ENABLE_EXTENDED_FLAGS.
        let mode = if on { mode | ENABLE_QUICK_EDIT_MODE } else { mode & !ENABLE_QUICK_EDIT_MODE };
        let _ = SetConsoleMode(input, mode | ENABLE_EXTENDED_FLAGS);
    }
}

fn say(text: &str) {
    unsafe {
        if let Ok(out) = GetStdHandle(STD_OUTPUT_HANDLE) {
            let w: Vec<u16> = text.encode_utf16().collect();
            if WriteConsoleW(out, &w, None, None).is_err() {
                let _ = WriteFile(out, Some(text.as_bytes()), None, None);
            }
        }
    }
}

fn timestamp() -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
}

/// Removes ANSI escape sequences from a byte stream fed in arbitrary chunks.
#[derive(Default)]
struct AnsiStrip {
    state: Esc,
}

#[derive(Default, Clone, Copy, PartialEq)]
enum Esc {
    #[default]
    None,
    /// After ESC.
    Start,
    /// ESC [ … final byte.
    Csi,
    /// ESC ] … BEL or ESC \.
    Osc,
    OscEnd,
}

impl AnsiStrip {
    fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(bytes.len());
        for &b in bytes {
            self.state = match (self.state, b) {
                (Esc::None, 0x1B) => Esc::Start,
                (Esc::None, _) => {
                    out.push(b);
                    Esc::None
                }
                (Esc::Start, b'[') => Esc::Csi,
                (Esc::Start, b']') => Esc::Osc,
                (Esc::Start, 0x20..=0x2F) => Esc::Start, // Intermediate bytes, e.g. ESC ( B.
                (Esc::Start, _) => Esc::None,
                (Esc::Csi, 0x40..=0x7E) => Esc::None,
                (Esc::Csi, _) => Esc::Csi,
                (Esc::Osc, 0x07) => Esc::None,
                (Esc::Osc, 0x1B) => Esc::OscEnd,
                (Esc::Osc, _) => Esc::Osc,
                (Esc::OscEnd, _) => Esc::None,
            };
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_escapes_across_chunks() {
        let mut s = AnsiStrip::default();
        let mut out = s.feed(b"\x1b[32minfo\x1b[");
        out.extend(s.feed(b"0m: ok\x1b]0;title\x07 done\x1b(B!\r\n"));
        assert_eq!(String::from_utf8(out).unwrap(), "info: ok done!\r\n");
    }
}
