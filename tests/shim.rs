//! brady-shim runs console apps' commands. These run it directly, without conhost,
//! capturing the app's output through `BRADY_SHIM_LOG`.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bb-shim-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Run `command` through brady-shim; returns (exit code, copied output).
fn run(dir: &Path, command: &str, envs: &[(&str, String)]) -> (i32, String) {
    let log = dir.join("out.log");
    let _ = std::fs::remove_file(&log);
    let status = Command::new(env!("CARGO_BIN_EXE_brady-shim"))
        .env("BRADY_SHIM_COMMAND", command)
        .env("BRADY_SHIM_LOG", &log)
        .env("BRADY_SHIM_HOLD", "0")
        .envs(envs.iter().map(|(k, v)| (k, v)))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .unwrap();
    (status.code().unwrap(), std::fs::read_to_string(&log).unwrap_or_default())
}

/// Regression: `cmd /c ""%VAR%" args"` with VAR set to a path containing spaces. conhost
/// rebuilt the command line and dropped the inner quotes, so cmd couldn't find the exe.
#[test]
fn quoted_path_with_spaces_inside_cmd_c() {
    let dir = temp_dir("quotes");
    let spaced = dir.join("dir with space");
    std::fs::create_dir_all(&spaced).unwrap();
    let exe = spaced.join("cmd.exe");
    let system = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    std::fs::copy(format!(r"{system}\System32\cmd.exe"), &exe).unwrap();
    let (code, out) = run(
        &dir,
        r#"cmd /c ""%BB_TEST_EXE%" /c echo quoted-ok""#,
        &[("BB_TEST_EXE", exe.to_string_lossy().into_owned())],
    );
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("quoted-ok"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn passes_through_the_exit_code() {
    let dir = temp_dir("exit");
    let (code, out) = run(&dir, "cmd /c exit 7", &[]);
    assert_eq!(code, 7);
    assert!(out.contains("exited with code 7"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hides_its_own_variables_from_the_app() {
    let dir = temp_dir("env");
    let (code, out) = run(&dir, "cmd /c set BRADY_SHIM", &[]);
    assert_eq!(code, 1, "{out}");
    assert!(!out.contains("BRADY_SHIM_COMMAND="), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Run `command` through brady-shim in a console of its own (with no window), as
/// conhost would, and return that console's text once `until` shows up in the copy.
fn console_text(dir: &Path, command: &str, until: &str) -> (String, String) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::*;
    use windows::core::{PCWSTR, PWSTR};
    let log = dir.join("out.log");
    let _ = std::fs::remove_file(&log);
    let mut env: Vec<(String, String)> = std::env::vars().filter(|(k, _)| !k.starts_with("BRADY_SHIM_")).collect();
    env.push(("BRADY_SHIM_COMMAND".into(), command.into()));
    env.push(("BRADY_SHIM_LOG".into(), log.to_string_lossy().into_owned()));
    env.push(("BRADY_SHIM_HOLD".into(), "0".into()));
    let mut block: Vec<u16> = env.iter().flat_map(|(k, v)| format!("{k}={v}\0").encode_utf16().collect::<Vec<_>>()).collect();
    block.push(0);
    let mut cmd: Vec<u16> = format!("\"{}\"\0", env!("CARGO_BIN_EXE_brady-shim")).encode_utf16().collect();
    let si = STARTUPINFOW { cb: size_of::<STARTUPINFOW>() as u32, ..Default::default() };
    let mut pi = PROCESS_INFORMATION::default();
    unsafe {
        // No std handles passed: the new console's own become the shim's, as under conhost.
        CreateProcessW(
            PCWSTR::null(),
            Some(PWSTR(cmd.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
            Some(block.as_ptr() as *const _),
            PCWSTR::null(),
            &si,
            &mut pi,
        )
        .unwrap();
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let copied = loop {
        let text = std::fs::read_to_string(&log).unwrap_or_default();
        if text.contains(until) || std::time::Instant::now() > deadline {
            break text;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let screen = Command::new(env!("CARGO_BIN_EXE_brady-bunch"))
        .args(["--read-console", &pi.dwProcessId.to_string(), "20"])
        .output()
        .unwrap();
    unsafe {
        WaitForSingleObject(pi.hProcess, 10_000);
        let _ = CloseHandle(pi.hProcess);
        let _ = CloseHandle(pi.hThread);
    }
    (String::from_utf8_lossy(&screen.stdout).into_owned(), copied)
}

/// Regression: copied output was written to the console as bytes, which it read in code
/// page 437, so rspack's `➜` (UTF-8 E2 9E 9C) showed as `Γ₧£`.
#[test]
fn copied_utf8_output_shows_correctly_in_the_console_and_the_log() {
    let dir = temp_dir("utf8");
    let file = dir.join("rspack.txt");
    std::fs::write(&file, "  \u{279C}  Local:    https://localhost:8000/\r\ncaf\u{e9} done\r\n").unwrap();
    let command = format!(r#"cmd /c "type "{}" & ping -n 3 127.0.0.1 >nul""#, file.display());
    let (screen, copied) = console_text(&dir, &command, "done");
    assert!(copied.contains("  \u{279C}  Local:    https://localhost:8000/"), "{copied}");
    assert!(screen.contains("  \u{279C}  Local:    https://localhost:8000/"), "{screen}");
    assert!(screen.contains("caf\u{e9} done"), "{screen}");
    assert!(!screen.contains('\u{393}'), "{screen}"); // Γ
    let _ = std::fs::remove_dir_all(&dir);
}

/// `encoding = "oem"`: output in the OEM code page reaches the log as UTF-8.
#[test]
fn oem_output_is_converted_for_the_log() {
    let dir = temp_dir("oem");
    let file = dir.join("oem.txt");
    // OEM code page bytes, from the system's own table.
    let oem = unsafe { windows::Win32::Globalization::GetOEMCP() };
    let text = "caf\u{e9}";
    let wide: Vec<u16> = text.encode_utf16().collect();
    let mut bytes = vec![0u8; 16];
    let n = unsafe {
        windows::Win32::Globalization::WideCharToMultiByte(oem, 0, &wide, Some(&mut bytes), windows::core::PCSTR::null(), None)
    };
    std::fs::write(&file, &bytes[..n as usize]).unwrap();
    let (code, out) = run(&dir, &format!(r#"cmd /c type "{}""#, file.display()), &[("BRADY_SHIM_ENCODING", "oem".into())]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains(text), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn reports_a_command_that_cannot_start() {
    let dir = temp_dir("missing");
    let (code, out) = run(&dir, r"C:\no\such\program.exe", &[]);
    assert_ne!(code, 0);
    assert!(out.contains("could not start"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}
