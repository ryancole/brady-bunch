//! The host's event log (`event_log` in the config): launches, exits, restarts,
//! readiness and each shutdown step, one timestamped line each.

use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

static LOG: Mutex<Option<File>> = Mutex::new(None);

/// Start a new log at `path`, replacing any previous one.
pub fn init(path: &Path) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    *LOG.lock().unwrap() = Some(File::create(path)?);
    Ok(())
}

pub fn log(line: impl AsRef<str>) {
    if let Some(f) = LOG.lock().unwrap().as_mut() {
        let _ = writeln!(f, "{} {}", timestamp(), line.as_ref());
    }
}

pub fn timestamp() -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    )
}
