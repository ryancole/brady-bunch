//! Console-side launcher. brady-bunch runs `conhost.exe brady-shim.exe` for each
//! console app, and this program starts the real command inside that console.
//!
//! - **Exact command line.** conhost re-tokenizes and rebuilds its client's command
//!   line, which loses quotes (`cmd /c ""C:\a b\x.exe" --flag"` arrives without the
//!   inner ones). The command comes in through the environment instead, untouched.
//! - **QuickEdit off**, so a click on the tile can't start a selection that blocks
//!   the app's next console write.
//! - **Output copy** (`log`): the app's stdout/stderr go through a pipe that is echoed
//!   to the console and appended to a file with escape sequences stripped. The bytes
//!   are decoded (UTF-8 by default) and written to the console as UTF-16, so the
//!   console's code page doesn't garble them, and the file always gets UTF-8.
//! - **Keeps the console open** after the app exits, so its last output stays readable,
//!   by handing the console to `brady-shim.exe --hold <code>` and exiting with the
//!   app's exit code.
//!
//! Inputs (removed from the environment before the app starts):
//! `BRADY_SHIM_COMMAND` command line, `BRADY_SHIM_APP` resolved executable,
//! `BRADY_SHIM_LOG` output copy, `BRADY_SHIM_ENCODING` what the copied output is in
//! (`utf-8`, `oem` or `ansi`), `BRADY_SHIM_QUICK_EDIT=1` leave QuickEdit alone,
//! `BRADY_SHIM_HOLD=0` don't keep the console open.

use std::ffi::c_void;
use std::fs::File;
use std::io::Write;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation};
use windows::Win32::Globalization::{
    CP_UTF8, GetACP, GetOEMCP, IsDBCSLeadByteEx, MULTI_BYTE_TO_WIDE_CHAR_FLAGS, MultiByteToWideChar,
};
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
const ENV_ENCODING: &str = "BRADY_SHIM_ENCODING";

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
    let code_page = match take_env(ENV_ENCODING).as_deref() {
        Some("oem") => unsafe { GetOEMCP() },
        Some("ansi") => unsafe { GetACP() },
        _ => CP_UTF8,
    };

    if !quick_edit {
        set_quick_edit(false);
    }
    let mut log = log_path.as_ref().and_then(|p| std::fs::OpenOptions::new().create(true).append(true).open(p).ok());
    if let Some(f) = &mut log {
        let _ = writeln!(f, "---- {} start: {command}", timestamp());
        // Programs that write in the console's code page (.NET, cmd) then write UTF-8,
        // which is what the copy decodes.
        if code_page == CP_UTF8 {
            unsafe {
                let _ = SetConsoleOutputCP(CP_UTF8);
            }
        }
    }

    let code = match unsafe { launch(&command, app.as_deref(), log.as_ref(), code_page) } {
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
unsafe fn launch(command: &str, app: Option<&str>, log: Option<&File>, code_page: u32) -> windows::core::Result<u32> {
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
            std::thread::spawn(move || copy_output(HANDLE(read as *mut c_void), file, code_page))
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

/// Echo the app's output to the console and append it, minus escape sequences, to the
/// log. The output is decoded from `code_page` and written to the console as UTF-16:
/// written as bytes, the console would read them in its own code page (437, say). The
/// log gets UTF-8.
fn copy_output(read: HANDLE, mut file: Option<File>, code_page: u32) {
    unsafe {
        let out = GetStdHandle(STD_OUTPUT_HANDLE).unwrap_or_default();
        // Show colour sequences rather than printing them.
        let mut mode = CONSOLE_MODE::default();
        let is_console = GetConsoleMode(out, &mut mode).is_ok();
        if is_console {
            let _ = SetConsoleMode(out, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
        }
        let mut decoder = Decoder::new(code_page);
        let mut strip = AnsiStrip::default();
        let mut write = |text: &str| {
            if text.is_empty() {
                return;
            }
            let wide: Vec<u16> = text.encode_utf16().collect();
            if !is_console || WriteConsoleW(out, &wide, None, None).is_err() {
                let _ = WriteFile(out, Some(text.as_bytes()), None, None);
            }
            if let Some(f) = &mut file {
                let _ = f.write_all(&strip.feed(text.as_bytes()));
            }
        };
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            let mut n = 0u32;
            if ReadFile(read, Some(&mut buf), Some(&mut n), None).is_err() || n == 0 {
                break;
            }
            write(&decoder.feed(&buf[..n as usize]));
        }
        write(&decoder.finish());
        let _ = CloseHandle(read);
    }
}

/// Decodes a byte stream fed in arbitrary chunks, holding back a character split
/// between chunks. In UTF-8 mode, bytes that aren't valid UTF-8 are read in the ANSI
/// code page instead: programs that ignore the console's code page usually write in it.
struct Decoder {
    code_page: u32,
    pending: Vec<u8>,
}

impl Decoder {
    fn new(code_page: u32) -> Decoder {
        Decoder { code_page, pending: Vec::new() }
    }

    fn feed(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        let buf = std::mem::take(&mut self.pending);
        if self.code_page != CP_UTF8 {
            // Hold back a trailing DBCS lead byte; its second byte is in the next chunk.
            let mut i = 0;
            while i < buf.len() {
                let lead = unsafe { IsDBCSLeadByteEx(self.code_page, buf[i]) }.is_ok();
                i += if lead { 2 } else { 1 };
            }
            let end = if i > buf.len() { buf.len() - 1 } else { buf.len() };
            self.pending = buf[end..].to_vec();
            return decode_code_page(self.code_page, &buf[..end]);
        }
        let mut out = String::with_capacity(buf.len());
        let mut rest = &buf[..];
        loop {
            match std::str::from_utf8(rest) {
                Ok(s) => {
                    out.push_str(s);
                    break;
                }
                Err(e) => {
                    let (good, bad) = rest.split_at(e.valid_up_to());
                    out.push_str(std::str::from_utf8(good).unwrap_or_default());
                    match e.error_len() {
                        // Cut off mid-character: wait for the rest.
                        None => {
                            self.pending = bad.to_vec();
                            break;
                        }
                        Some(n) => {
                            out.push_str(&decode_code_page(unsafe { GetACP() }, &bad[..n]));
                            rest = &bad[n..];
                        }
                    }
                }
            }
        }
        out
    }

    /// Whatever was held back, at the end of the stream.
    fn finish(&mut self) -> String {
        let rest = std::mem::take(&mut self.pending);
        let cp = if self.code_page == CP_UTF8 { unsafe { GetACP() } } else { self.code_page };
        decode_code_page(cp, &rest)
    }
}

fn decode_code_page(code_page: u32, bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    unsafe {
        let flags = MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0);
        let n = MultiByteToWideChar(code_page, flags, bytes, None);
        if n <= 0 {
            return String::from_utf8_lossy(bytes).into_owned();
        }
        let mut wide = vec![0u16; n as usize];
        let n = MultiByteToWideChar(code_page, flags, bytes, Some(&mut wide));
        String::from_utf16_lossy(&wide[..n.max(0) as usize])
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
    brady_bunch::events::timestamp()
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

    #[test]
    fn decodes_utf8_split_across_chunks() {
        // "➜" (E2 9E 9C) cut after its first byte and after its second; "é" (C3 A9) too.
        let mut d = Decoder::new(CP_UTF8);
        let mut out = d.feed(b"  \xE2");
        out += &d.feed(b"\x9E");
        out += &d.feed(b"\x9C  Local \xC3\xA9t\xC3");
        out += &d.feed(b"\xA9\r\n");
        out += &d.finish();
        assert_eq!(out, "  \u{279C}  Local \u{e9}t\u{e9}\r\n");
    }

    #[test]
    fn decodes_invalid_utf8_and_other_code_pages() {
        // A byte that can't be UTF-8 is read in the ANSI code page rather than dropped.
        let mut d = Decoder::new(CP_UTF8);
        let out = d.feed(b"a\xFFb");
        assert_eq!(out.chars().count(), 3, "{out}");
        assert!(out.starts_with('a') && out.ends_with('b'), "{out}");
        // Code page 437: 0x82 is é, 0xC4 a box-drawing line.
        let mut d = Decoder::new(437);
        assert_eq!(d.feed(b"caf\x82 \xC4"), "caf\u{e9} \u{2500}");
    }
}
