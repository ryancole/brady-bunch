//! `brady-ctl up` starts a host that outlives it. Callers that read `up`'s output to
//! end-of-file must get it when `up` exits, not when the host does.

use std::fs::File;
use std::io::Read;
use std::os::windows::io::{FromRawHandle, OwnedHandle};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;
use windows::Win32::Foundation::{HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation};
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::Win32::System::Pipes::CreatePipe;

fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bb-up-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// An anonymous pipe; the write end is inheritable, the read end isn't.
fn pipe() -> (File, OwnedHandle) {
    let sa = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        bInheritHandle: true.into(),
        ..Default::default()
    };
    let (mut read, mut write) = (HANDLE::default(), HANDLE::default());
    unsafe {
        CreatePipe(&mut read, &mut write, Some(&sa), 0).unwrap();
        SetHandleInformation(read, HANDLE_FLAG_INHERIT.0, Default::default()).unwrap();
        (File::from_raw_handle(read.0), OwnedHandle::from_raw_handle(write.0))
    }
}

/// Read `f` to end-of-file on another thread; None if that takes longer than `timeout`.
fn read_to_eof(mut f: File, timeout: Duration) -> Option<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut s = String::new();
        let _ = f.read_to_string(&mut s);
        let _ = tx.send(s);
    });
    rx.recv_timeout(timeout).ok()
}

/// Regression: the host inherited every inheritable handle `up` had, including ones `up`
/// itself inherited from its caller without them being its stdout (PowerShell passes its
/// own on, for example). The caller then saw no end-of-file until the host exited.
#[test]
fn capturing_ups_output_returns_while_the_host_runs() {
    let dir = temp_dir("capture");
    let config = dir.join("brady.toml");
    std::fs::write(
        &config,
        "title = \"up capture test\"\n\n[[app]]\nname = \"sleeper\"\ncommand = 'ping -n 300 127.0.0.1'\n",
    )
    .unwrap();
    let ctl = env!("CARGO_BIN_EXE_brady-ctl");
    let (out_read, out_write) = pipe();
    // A handle `up` inherits without it being one of its standard handles.
    let (outer_read, outer_write) = pipe();
    let mut up = Command::new(ctl)
        .arg("up")
        .arg(&config)
        .args(["--timeout", "60"])
        .stdin(Stdio::null())
        .stdout(Stdio::from(out_write))
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(outer_write);
    let out = read_to_eof(out_read, Duration::from_secs(60));
    let outer = read_to_eof(outer_read, Duration::from_secs(5));
    let status = up.wait().unwrap();
    let mut err = String::new();
    let _ = up.stderr.take().unwrap().read_to_string(&mut err);
    let host = Command::new(ctl).arg("status").arg(&config).output().unwrap();
    let _ = Command::new(ctl).arg("down").arg(&config).output();
    let _ = std::fs::remove_dir_all(&dir);

    assert!(status.success(), "up failed: {err}");
    let out = out.expect("up's stdout didn't reach end-of-file");
    assert!(out.contains("ready: sleeper"), "{out}");
    assert!(outer.is_some(), "the host kept a handle up inherited from its caller open");
    assert!(host.status.success(), "the host should still be running");
}
