//! The host window's placement and zoomed tile, remembered per config file in
//! `%LOCALAPPDATA%\brady-bunch\layouts\<config key>.json` (not next to the config,
//! which is usually checked in).

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONULL, MonitorFromRect};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowPlacement, SW_SHOWMAXIMIZED, SW_SHOWMINIMIZED, SW_SHOWNORMAL, SetWindowPlacement, WINDOWPLACEMENT,
    WPF_RESTORETOMAXIMIZED,
};

#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    /// The restored (not maximized) window rect, in workspace coordinates.
    pub rect: RECT,
    pub maximized: bool,
    /// Name of the app whose tile was zoomed.
    pub zoomed: Option<String>,
}

fn file_for(config: &Path) -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    let key = crate::control::config_key(config)?;
    Some(PathBuf::from(base).join("brady-bunch").join("layouts").join(format!("{key}.json")))
}

pub fn load(config: &Path) -> Option<Layout> {
    let text = std::fs::read_to_string(file_for(config)?).ok()?;
    from_json(&serde_json::from_str(&text).ok()?)
}

pub fn save(config: &Path, layout: &Layout) {
    let Some(file) = file_for(config) else { return };
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let v = json!({
        "config": config.to_string_lossy(),
        "rect": [layout.rect.left, layout.rect.top, layout.rect.right, layout.rect.bottom],
        "maximized": layout.maximized,
        "zoomed": layout.zoomed,
    });
    let _ = std::fs::write(file, serde_json::to_string_pretty(&v).unwrap_or_default());
}

fn from_json(v: &Value) -> Option<Layout> {
    let r: Vec<i32> = v["rect"].as_array()?.iter().map(|n| n.as_i64().map(|n| n as i32)).collect::<Option<_>>()?;
    let [left, top, right, bottom] = r[..] else { return None };
    if right - left < 100 || bottom - top < 100 {
        return None;
    }
    Some(Layout {
        rect: RECT { left, top, right, bottom },
        maximized: v["maximized"].as_bool().unwrap_or(false),
        zoomed: v["zoomed"].as_str().map(String::from),
    })
}

/// The window's current placement.
pub fn capture(hwnd: HWND, zoomed: Option<String>) -> Option<Layout> {
    let mut wp = WINDOWPLACEMENT { length: size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
    unsafe { GetWindowPlacement(hwnd, &mut wp) }.ok()?;
    let show = wp.showCmd as i32;
    let maximized = show == SW_SHOWMAXIMIZED.0
        || (show == SW_SHOWMINIMIZED.0 && wp.flags.0 & WPF_RESTORETOMAXIMIZED.0 != 0);
    Some(Layout { rect: wp.rcNormalPosition, maximized, zoomed })
}

/// Place and show the window as saved. False (and nothing shown) if the saved rect
/// isn't on any monitor now, e.g. that monitor was unplugged.
pub fn restore(hwnd: HWND, layout: &Layout) -> bool {
    if unsafe { MonitorFromRect(&layout.rect, MONITOR_DEFAULTTONULL) }.is_invalid() {
        return false;
    }
    let wp = WINDOWPLACEMENT {
        length: size_of::<WINDOWPLACEMENT>() as u32,
        showCmd: if layout.maximized { SW_SHOWMAXIMIZED.0 as u32 } else { SW_SHOWNORMAL.0 as u32 },
        rcNormalPosition: layout.rect,
        ..Default::default()
    };
    unsafe { SetWindowPlacement(hwnd, &wp) }.is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_json_round_trips_and_rejects_junk() {
        let l = Layout { rect: RECT { left: 10, top: 20, right: 1210, bottom: 820 }, maximized: true, zoomed: Some("api".into()) };
        let v = json!({ "rect": [10, 20, 1210, 820], "maximized": true, "zoomed": "api" });
        assert_eq!(from_json(&v), Some(l));
        assert_eq!(from_json(&json!({ "rect": [0, 0, 5, 5] })), None);
        assert_eq!(from_json(&json!({ "rect": "nope" })), None);
    }
}
