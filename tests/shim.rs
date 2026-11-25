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

#[test]
fn reports_a_command_that_cannot_start() {
    let dir = temp_dir("missing");
    let (code, out) = run(&dir, r"C:\no\such\program.exe", &[]);
    assert_ne!(code, 0);
    assert!(out.contains("could not start"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}
