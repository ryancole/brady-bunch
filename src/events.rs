//! The host's event log (`event_log` in the config): launches, exits, restarts,
//! readiness and each shutdown step, one timestamped line each.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

static LOG: Mutex<Option<File>> = Mutex::new(None);

/// Start a new log at `path`, keeping `keep` previous ones (see `rotate`).
pub fn init(path: &Path, keep: u32) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    rotate(path, keep);
    *LOG.lock().unwrap() = Some(File::create(path)?);
    Ok(())
}

pub fn log(line: impl AsRef<str>) {
    if let Some(f) = LOG.lock().unwrap().as_mut() {
        let _ = writeln!(f, "{} {}", timestamp(), line.as_ref());
    }
}

/// Move `events.log` to `events.1.log`, `events.1.log` to `events.2.log` and so on,
/// keeping `keep` old copies. Nothing happens if `path` doesn't exist.
pub fn rotate(path: &Path, keep: u32) {
    if keep == 0 || !path.exists() {
        return;
    }
    let _ = std::fs::remove_file(numbered(path, keep));
    for n in (1..keep).rev() {
        let _ = std::fs::rename(numbered(path, n), numbered(path, n + 1));
    }
    let _ = std::fs::rename(path, numbered(path, 1));
}

/// `events.log` -> `events.<n>.log`; `events` -> `events.<n>`.
pub fn numbered(path: &Path, n: u32) -> PathBuf {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let name = match path.extension() {
        Some(ext) => format!("{stem}.{n}.{}", ext.to_string_lossy()),
        None => format!("{stem}.{n}"),
    };
    path.with_file_name(name)
}

pub fn timestamp() -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    )
}

/// ISO 8601 in UTC with milliseconds: `2026-11-25T18:22:15.280Z`.
pub fn iso_utc(t: SystemTime) -> String {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs();
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60,
        d.subsec_millis()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn iso_utc_formats_known_instants() {
        assert_eq!(iso_utc(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
        let t = UNIX_EPOCH + Duration::from_millis(1_795_998_135_280);
        assert_eq!(iso_utc(t), "2026-11-30T00:22:15.280Z");
        let leap = UNIX_EPOCH + Duration::from_secs(951_782_400); // 2000-02-29
        assert_eq!(iso_utc(leap), "2000-02-29T00:00:00.000Z");
    }

    #[test]
    fn rotate_keeps_numbered_copies() {
        let dir = std::env::temp_dir().join(format!("bb-rotate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("events.log");
        for run in 1..=4 {
            rotate(&log, 2);
            std::fs::write(&log, format!("run {run}")).unwrap();
        }
        let read = |p: PathBuf| std::fs::read_to_string(p).unwrap_or_default();
        assert_eq!(read(log.clone()), "run 4");
        assert_eq!(read(dir.join("events.1.log")), "run 3");
        assert_eq!(read(dir.join("events.2.log")), "run 2");
        assert!(!dir.join("events.3.log").exists());
        assert_eq!(numbered(&dir.join("plain"), 1), dir.join("plain.1"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
