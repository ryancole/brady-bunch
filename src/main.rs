#![windows_subsystem = "windows"]

use brady_bunch::{child, config, control, ctrlc, events, layout, ready};
use child::{Child, Readiness, Status, StopBy, StopOutcome};
use control::{Cmd, Reply, Request, WM_APP_CONTROL};
use ready::Check;
use serde_json::{Value, json};
use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, SetProcessDpiAwarenessContext,
    SystemParametersInfoForDpi,
};
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, PCWSTR, Result, w};

const TIMER_POLL: usize = 1;
const POLL_MS: u32 = 150;
/// Posted by the shutdown thread: wParam = child index, lParam = encoded StopOutcome.
const WM_APP_STOPPED: u32 = WM_APP + 1;
const WM_APP_STOPPING: u32 = WM_APP + 2;
/// Posted when a stop requested with `--stop`/`--restart` has finished.
const WM_APP_MANUAL_STOPPED: u32 = WM_APP + 3;
/// Posted when the apps stopped for `run` have all stopped: rerun the task.
const WM_APP_RUN_GO: u32 = WM_APP + 4;

const BG: COLORREF = rgb(24, 24, 24);
const HEADER: COLORREF = rgb(45, 45, 48);
/// Header of the tile that has keyboard focus.
const HEADER_FOCUSED: COLORREF = rgb(38, 79, 120);
/// Header of a tile whose app crashed (until it's launched again).
const HEADER_CRASHED: COLORREF = rgb(120, 36, 36);
const PANEL: COLORREF = rgb(32, 32, 34);
const TEXT: COLORREF = rgb(230, 230, 230);
const DIM: COLORREF = rgb(150, 150, 150);
const OK: COLORREF = rgb(110, 200, 120);
const WARN: COLORREF = rgb(230, 180, 80);
const BAD: COLORREF = rgb(235, 100, 100);
const LINK: COLORREF = rgb(120, 175, 255);

const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

/// A `brady-ctl run` in progress: stopping apps before the task reruns.
struct Run {
    task: usize,
    /// Apps to start again once the task has been relaunched.
    apps: Vec<usize>,
    reply: Reply,
}

struct Ui {
    hwnd: HWND,
    title: String,
    config_path: PathBuf,
    selection: config::Selection,
    started_at: SystemTime,
    flash_on_crash: bool,
    remember_layout: bool,
    /// `brady-ctl down` clients, answered with the final status as the host exits.
    quit_replies: Vec<Reply>,
    run: Option<Run>,
    /// `event_log`, absolute.
    event_log: Option<PathBuf>,
    columns: Option<u32>,
    timeout_ms: u64,
    helper_exe: PathBuf,
    /// What every child is created with, kept for apps added by a later `up`.
    ctx: child::Context,
    children: Vec<Child>,
    /// Whether each child is part of the host's selection. An app a later `up --exact`
    /// left out stays in `children` (indices don't change), stopped and hidden.
    selected: Vec<bool>,
    /// For each child, the children it waits for.
    deps: Vec<Vec<usize>>,
    shutting_down: bool,
    pending_stops: usize,
    /// Stops requested from the command line that haven't finished.
    manual_stops: usize,
    /// Start the child again once its requested stop finishes (`--restart`).
    start_after_stop: Vec<bool>,
    /// Tile shown alone, filling the window; toggled by double-clicking a tile header.
    zoomed: Option<usize>,
    /// Tile whose window last received keyboard focus; restored when the host is reactivated.
    focused: Option<usize>,
    /// Which tiles were in the grid at the last layout.
    shown: Vec<bool>,
    /// Header links drawn in the last paint: (child, rect).
    links: Vec<(usize, RECT)>,
    dpi: u32,
    font: HFONT,
    link_font: HFONT,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

/// Runs `f` on the UI state unless it is already borrowed (a re-entrant message
/// sent while we're mid-update), in which case the caller falls back to defaults.
fn with_ui<R>(f: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    UI.with(|cell| cell.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let flag = args.get(1).map(String::as_str).unwrap_or("");
    let num = |i: usize| args.get(i).and_then(|s| s.parse().ok()).unwrap_or(0);
    if flag == ctrlc::HELPER_FLAG {
        std::process::exit(ctrlc::helper_main(num(2)));
    }
    if flag == ctrlc::READ_FLAG {
        std::process::exit(ctrlc::read_main(num(2), num(3)));
    }
    if control::VERBS.contains(&flag) || flag == "-V" {
        std::process::exit(control::client_main(&args[1..]));
    }

    if args.iter().any(|a| a == "--no-dialog") {
        NO_DIALOG.store(true, Ordering::Relaxed);
    }
    let launch = match parse_launch_args(&args[1..]) {
        Ok(a) => a,
        Err(e) => fatal(&e),
    };
    let path = config::resolve_path(launch.path);
    let cfg = match config::load(&path, &launch.sel) {
        Ok(c) => c,
        Err(e) => fatal(&e),
    };
    // A host is already running this config: show it instead of starting a second one.
    // Checked before the logs are touched, since they belong to that host.
    if !launch.new_instance
        && let Some(status) = control::running_host(&path)
    {
        // Start whatever this selection adds to it.
        let mut want: Vec<String> = cfg.entries.iter().map(|e| e.name().to_lowercase()).collect();
        want.sort();
        if control::app_names(&status) != want
            && let Err(e) = control::change_selection(&path, &launch.sel, false)
        {
            fatal(&format!("brady-bunch is already running this config (pid {}), and couldn't add apps to it: {e}", status["host_pid"]));
        }
        control::activate(&status);
        std::process::exit(0);
    }
    if let Some(log) = &cfg.event_log
        && let Err(e) = events::init(log, cfg.keep_logs)
    {
        fatal(&format!("Could not create event_log {}: {e}", log.display()));
    }
    events::log(format!("host started (pid {}) with {}", std::process::id(), path.display()));
    if let Err(e) = unsafe { run(cfg, path) } {
        fatal(&e.message());
    }
    events::log("host exiting");
}

#[derive(Debug, Default)]
struct LaunchArgs {
    path: Option<PathBuf>,
    sel: config::Selection,
    /// Start even if a host is already running this config.
    new_instance: bool,
}

/// `[config] [--profile p] [--only a,b] [--skip c,d] [--new-instance] [--no-dialog]`
fn parse_launch_args(args: &[String]) -> std::result::Result<LaunchArgs, String> {
    let mut path = None;
    let mut sel = config::Selection::default();
    let mut new_instance = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let list = |v: Option<&String>| -> std::result::Result<Vec<String>, String> {
            let v = v.ok_or_else(|| format!("{a} needs a comma-separated list of names"))?;
            Ok(v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
        };
        match a.as_str() {
            "--profile" => sel.profiles.extend(list(it.next())?),
            "--only" => sel.only.extend(list(it.next())?),
            "--skip" => sel.skip.extend(list(it.next())?),
            "--new-instance" => new_instance = true,
            "--no-dialog" => {} // Read in main, before anything can fail.
            s if s.starts_with("--") => return Err(format!("Unknown option {s}.")),
            _ if path.is_none() => path = Some(PathBuf::from(a)),
            _ => return Err(format!("Unexpected argument {a}.")),
        }
    }
    Ok(LaunchArgs { path, sel, new_instance })
}

/// `--no-dialog`: report fatal errors on stderr and in the event log instead of a
/// message box, for callers that can't see or dismiss one.
static NO_DIALOG: AtomicBool = AtomicBool::new(false);

fn fatal(msg: &str) -> ! {
    if NO_DIALOG.load(Ordering::Relaxed) {
        events::log(format!("fatal: {msg}"));
        eprintln!("brady-bunch: {msg}");
    } else {
        unsafe {
            MessageBoxW(None, &HSTRING::from(msg), w!("Brady Bunch"), MB_ICONERROR | MB_OK);
        }
    }
    std::process::exit(1)
}

unsafe fn run(cfg: config::Loaded, config_path: PathBuf) -> Result<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let instance = GetModuleHandleW(None)?;
        let class = w!("BradyBunchHost");
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: class,
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            return Err(windows::core::Error::from_thread());
        }
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            &HSTRING::from(&cfg.title),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            None,
            None,
            Some(instance.into()),
            None,
        )?;

        let dpi = GetDpiForWindow(hwnd);
        let helper_exe = std::env::current_exe().unwrap_or_default();
        let shim = helper_exe.parent().map(|d| d.join(child::SHIM_EXE)).filter(|p| p.is_file());
        let ctx = child::Context { shim, keep_logs: cfg.keep_logs };
        let children: Vec<Child> = cfg.entries.iter().enumerate().map(|(i, e)| Child::new(i, e, &ctx)).collect();
        let n = children.len();
        UI.with(|cell| {
            *cell.borrow_mut() = Some(Ui {
                hwnd,
                title: cfg.title.clone(),
                config_path: std::path::absolute(&config_path).unwrap_or(config_path.clone()),
                selection: cfg.selection.clone(),
                started_at: SystemTime::now(),
                flash_on_crash: cfg.flash_on_crash,
                remember_layout: cfg.remember_layout,
                quit_replies: Vec::new(),
                run: None,
                event_log: cfg.event_log.clone(),
                columns: cfg.columns,
                timeout_ms: cfg.shutdown_timeout_ms,
                helper_exe: helper_exe.clone(),
                ctx: ctx.clone(),
                children,
                selected: vec![true; n],
                deps: cfg.deps.clone(),
                shutting_down: false,
                pending_stops: 0,
                manual_stops: 0,
                start_after_stop: vec![false; n],
                zoomed: None,
                focused: None,
                shown: Vec::new(),
                links: Vec::new(),
                dpi,
                font: message_font(dpi, false),
                link_font: message_font(dpi, true),
            })
        });
        with_ui(Ui::gate);
        control::serve(hwnd, &config_path, helper_exe);

        // Where the window was last time, unless that monitor is gone.
        let saved = if cfg.remember_layout { layout::load(&config_path) } else { None };
        if let Some(name) = saved.as_ref().and_then(|l| l.zoomed.as_deref()) {
            with_ui(|ui| ui.zoomed = ui.children.iter().position(|c| c.name.eq_ignore_ascii_case(name)));
        }
        if !saved.as_ref().is_some_and(|l| layout::restore(hwnd, l)) {
            let _ = ShowWindow(hwnd, if cfg.maximized { SW_SHOWMAXIMIZED } else { SW_SHOWDEFAULT });
        }
        SetTimer(Some(hwnd), TIMER_POLL, POLL_MS, None);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        // Dropping the UI closes the job handles; KILL_ON_JOB_CLOSE reaps anything left.
        UI.with(|cell| {
            if let Some(ui) = cell.borrow_mut().as_mut() {
                ui.children.iter_mut().for_each(Child::cleanup);
                let last = ui.status_json();
                for r in ui.quit_replies.drain(..) {
                    let _ = r.send(Ok(last.clone()));
                }
            }
            cell.borrow_mut().take()
        });
        control::wait_for_final_replies(std::time::Duration::from_secs(2));
        Ok(())
    }
}

fn point(lp: LPARAM) -> POINT {
    POINT { x: lp.0 as i16 as i32, y: (lp.0 >> 16) as i16 as i32 }
}

fn encode_outcome(o: StopOutcome) -> isize {
    (o.by.index() as isize) | ((o.ms.min(i32::MAX as u64) as isize) << 4)
}

fn decode_outcome(lp: LPARAM) -> StopOutcome {
    StopOutcome { by: StopBy::from_index((lp.0 & 0xF) as usize), ms: (lp.0 >> 4) as u64 }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_TIMER if wp.0 == TIMER_POLL => {
                with_ui(Ui::poll);
                LRESULT(0)
            }
            WM_SIZE => {
                with_ui(Ui::layout);
                LRESULT(0)
            }
            WM_PAINT => match with_ui(Ui::paint) {
                Some(()) => LRESULT(0),
                None => DefWindowProcW(hwnd, msg, wp, lp),
            },
            // A click inside an embedded window. Embedded windows belong to other
            // processes and don't take focus when clicked, so hand it to them here.
            WM_PARENTNOTIFY
                if matches!((wp.0 & 0xFFFF) as u32, WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN) =>
            {
                if let Some(target) = with_ui(|ui| ui.focus_target_at(point(lp))).flatten() {
                    let _ = SetFocus(Some(target));
                }
                LRESULT(0)
            }
            WM_SETFOCUS => {
                // The host was activated: give focus back to the tile that last had it.
                if let Some(target) = with_ui(|ui| ui.focused_window()).flatten() {
                    let _ = SetFocus(Some(target));
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                // A click on the host itself (a header or empty tile) takes focus off the tiles.
                if with_ui(|ui| ui.clear_focus()).unwrap_or(false) {
                    let _ = SetFocus(Some(hwnd));
                }
                LRESULT(0)
            }
            WM_LBUTTONDBLCLK => {
                with_ui(|ui| ui.toggle_zoom_at(point(lp)));
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                with_ui(|ui| ui.click(point(lp)));
                LRESULT(0)
            }
            WM_SETCURSOR if (lp.0 & 0xFFFF) as u32 == HTCLIENT => {
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                let _ = ScreenToClient(hwnd, &mut pt);
                if with_ui(|ui| ui.link_at(pt).is_some() || ui.restartable_at(pt).is_some()).unwrap_or(false)
                    && let Ok(hand) = LoadCursorW(None, IDC_HAND)
                {
                    SetCursor(Some(hand));
                    return LRESULT(1);
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_ERASEBKGND => LRESULT(1), // WM_PAINT covers the whole client area.
            WM_CLOSE => {
                if with_ui(Ui::request_close).unwrap_or(true) {
                    let _ = DestroyWindow(hwnd);
                }
                LRESULT(0)
            }
            WM_APP_STOPPING => {
                with_ui(|ui| ui.on_stopping(wp.0));
                LRESULT(0)
            }
            WM_APP_STOPPED => {
                if with_ui(|ui| ui.on_stopped(wp.0, decode_outcome(lp))).unwrap_or(false) {
                    let _ = DestroyWindow(hwnd);
                }
                LRESULT(0)
            }
            WM_APP_MANUAL_STOPPED => {
                if with_ui(|ui| ui.on_manual_stopped(wp.0, decode_outcome(lp))).unwrap_or(false) {
                    let _ = DestroyWindow(hwnd);
                }
                LRESULT(0)
            }
            WM_APP_CONTROL => {
                // SAFETY: posted by control::post with a leaked Box<Request>.
                let req = Box::from_raw(lp.0 as *mut Request);
                let Request { cmd, reply } = *req;
                match cmd {
                    Cmd::Quit => {
                        // Answered as the host exits; a busy UI drops the reply, which the client sees.
                        if with_ui(|ui| ui.quit_replies.push(reply)).is_some() {
                            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                        }
                    }
                    Cmd::Run { task, stop } => {
                        with_ui(|ui| ui.start_run(&task, &stop, reply));
                    }
                    Cmd::Select { sel, exact } => {
                        let result = with_ui(|ui| ui.select(sel, exact))
                            .unwrap_or_else(|| Err("the host is busy; try again".into()));
                        let _ = reply.send(result);
                    }
                    cmd => {
                        let result =
                            with_ui(|ui| ui.control(cmd)).unwrap_or_else(|| Err("the host is busy; try again".into()));
                        let _ = reply.send(result);
                    }
                }
                LRESULT(0)
            }
            WM_APP_RUN_GO => {
                with_ui(Ui::on_run_go);
                LRESULT(0)
            }
            WM_DPICHANGED => {
                let r = &*(lp.0 as *const RECT);
                with_ui(|ui| ui.set_dpi((wp.0 & 0xFFFF) as u32));
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

impl Ui {
    fn scale(&self, px: i32) -> i32 {
        px * self.dpi as i32 / 96
    }

    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn poll(&mut self) {
        let (mut repaint, mut relayout) = (false, false);
        for c in &mut self.children {
            let (p, l) = c.poll(self.hwnd, self.shutting_down);
            repaint |= p;
            relayout |= l;
        }
        repaint |= self.gate();
        // Take every flag, not just the first.
        let mut crashed = false;
        for c in &mut self.children {
            crashed |= std::mem::take(&mut c.new_crash);
        }
        if crashed && self.flash_on_crash && !self.shutting_down {
            let info = FLASHWINFO {
                cbSize: size_of::<FLASHWINFO>() as u32,
                hwnd: self.hwnd,
                // Until the window is brought to the front.
                dwFlags: FLASHW_ALL | FLASHW_TIMERNOFG,
                uCount: 0,
                dwTimeout: 0,
            };
            unsafe {
                let _ = FlashWindowEx(&info);
            }
        }
        if relayout || self.shown_tiles() != self.shown {
            self.layout();
        }
        if repaint {
            self.invalidate();
        }
    }

    /// Launch every waiting child whose dependencies are ready. Returns true if any started.
    fn gate(&mut self) -> bool {
        if self.shutting_down {
            return false;
        }
        let mut any = false;
        // Launching an app without a `ready` check makes it ready at once, which can
        // release the apps that depend on it, so repeat until nothing changes.
        loop {
            let held = self.run.as_ref().map(|r| r.task);
            let next = (0..self.children.len()).find(|&i| {
                self.selected[i]
                    && self.children[i].status == Status::Waiting
                    && Some(i) != held
                    && self.deps[i].iter().all(|&d| self.children[d].is_ready())
            });
            let Some(i) = next else { return any };
            let ctx = self.precheck();
            self.children[i].launch(&ctx);
            any = true;
        }
    }

    /// What a launch checks against: this host's own processes and running apps.
    fn precheck(&self) -> child::Precheck {
        child::Precheck {
            own_pids: self.children.iter().flat_map(Child::job_pids).collect(),
            here: self
                .children
                .iter()
                .filter(|c| !c.cfg.task && (c.is_active() || matches!(c.status, Status::StopQueued | Status::Stopping)))
                .map(|c| c.name.clone())
                .collect(),
        }
    }

    /// The task shown while setup runs: the first one that hasn't finished.
    fn current_task(&self) -> Option<usize> {
        (0..self.children.len()).find(|&i| self.children[i].cfg.task && !self.children[i].is_done())
    }

    /// Whether each tile is in the grid: selected apps always, tasks only while running setup.
    fn shown_tiles(&self) -> Vec<bool> {
        let task = self.current_task();
        (0..self.children.len()).map(|i| self.selected[i] && (!self.children[i].cfg.task || Some(i) == task)).collect()
    }

    /// Tile rects for each child: a near-square grid; a short last row stretches to fill.
    /// Tiles that aren't shown get an empty rect.
    fn cells(&self) -> Vec<RECT> {
        let shown = self.shown_tiles();
        let visible: Vec<usize> = (0..shown.len()).filter(|&i| shown[i]).collect();
        let n = visible.len();
        let mut client = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut client);
        }
        let mut out = vec![RECT::default(); self.children.len()];
        if n == 0 {
            return out;
        }
        let cols = self
            .columns
            .map(|c| c as usize)
            .unwrap_or_else(|| (n as f64).sqrt().ceil() as usize)
            .clamp(1, n);
        let (margin, gap) = (self.scale(MARGIN), self.scale(GAP));
        if let Some(z) = self.zoomed.filter(|&z| shown.get(z) == Some(&true)) {
            out[z] = grid(client, 1, 1, margin, gap)[0];
            return out;
        }
        for (rect, &i) in grid(client, n, cols, margin, gap).into_iter().zip(&visible) {
            out[i] = rect;
        }
        out
    }

    /// True if the tile isn't drawn: another tile is zoomed, or it's a task not running.
    fn is_hidden(&self, index: usize) -> bool {
        let shown = self.shown_tiles();
        !shown[index] || self.zoomed.is_some_and(|z| z != index && shown.get(z) == Some(&true))
    }

    /// The embedded window under `pt` (host client coordinates), now marked as focused.
    fn focus_target_at(&mut self, pt: POINT) -> Option<HWND> {
        let cells = self.cells();
        let i = (0..self.children.len())
            .find(|&i| !self.is_hidden(i) && contains(&self.split(&cells[i]).1, pt))?;
        let hwnd = self.children[i].window.as_ref()?.hwnd;
        if self.focused != Some(i) {
            self.focused = Some(i);
            self.invalidate();
        }
        Some(hwnd)
    }

    fn focused_window(&self) -> Option<HWND> {
        let i = self.focused?;
        if self.is_hidden(i) {
            return None;
        }
        self.children.get(i)?.window.as_ref().map(|w| w.hwnd)
    }

    /// Forget the focused tile. Returns true if one was focused.
    fn clear_focus(&mut self) -> bool {
        let had = self.focused.take().is_some();
        if had {
            self.invalidate();
        }
        had
    }

    fn toggle_zoom_at(&mut self, pt: POINT) {
        let cells = self.cells();
        let hit = cells.iter().enumerate().position(|(i, cell)| {
            let (h, _) = self.split(cell);
            !self.is_hidden(i) && contains(&h, pt)
        });
        if let Some(i) = hit {
            self.zoomed = if self.zoomed == Some(i) { None } else { Some(i) };
            self.layout();
            if self.zoomed == Some(i)
                && let Some(w) = &self.children[i].window
            {
                // Zooming a tile implies working in it.
                self.focused = Some(i);
                let hwnd = w.hwnd;
                unsafe {
                    let _ = SetFocus(Some(hwnd));
                }
            }
        }
    }

    fn split(&self, cell: &RECT) -> (RECT, RECT) {
        let header_h = self.scale(24);
        let header = RECT { bottom: cell.top + header_h, ..*cell };
        let body = RECT { top: cell.top + header_h, ..*cell };
        (header, body)
    }

    fn layout(&mut self) {
        self.shown = self.shown_tiles();
        let cells = self.cells();
        for (i, (c, cell)) in self.children.iter().zip(&cells).enumerate() {
            if let Some(w) = &c.window {
                let (_, b) = self.split(cell);
                // Hidden tiles keep their size, so consoles don't reflow to a zero width.
                let visibility = if self.is_hidden(i) {
                    SWP_HIDEWINDOW | SWP_NOMOVE | SWP_NOSIZE
                } else {
                    SWP_SHOWWINDOW
                };
                unsafe {
                    // Async so a hung child can't freeze the host.
                    let _ = SetWindowPos(
                        w.hwnd,
                        None,
                        b.left,
                        b.top,
                        b.right - b.left,
                        b.bottom - b.top,
                        SWP_NOZORDER | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS | visibility,
                    );
                }
            }
        }
        self.invalidate();
    }

    fn paint(&mut self) {
        let cells = self.cells();
        let mut links = Vec::new();
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(self.hwnd, &mut ps);
            let mut client = RECT::default();
            let _ = GetClientRect(self.hwnd, &mut client);
            let bg = CreateSolidBrush(BG);
            let header = CreateSolidBrush(HEADER);
            let header_focused = CreateSolidBrush(HEADER_FOCUSED);
            let header_crashed = CreateSolidBrush(HEADER_CRASHED);
            let panel = CreateSolidBrush(PANEL);
            FillRect(hdc, &client, bg);
            let old_font = SelectObject(hdc, self.font.into());
            SetBkMode(hdc, TRANSPARENT);
            let pad = self.scale(8);

            for (i, cell) in cells.iter().enumerate() {
                if self.is_hidden(i) {
                    continue;
                }
                let c = &self.children[i];
                let (h, b) = self.split(cell);
                let brush = if c.crashed {
                    header_crashed
                } else if self.focused == Some(i) {
                    header_focused
                } else {
                    header
                };
                FillRect(hdc, &h, brush);
                let text_rc = RECT { left: h.left + pad, right: h.right - pad, ..h };
                let (status, color) = self.status_text(i);
                let status_w = text_width(hdc, &status).min((text_rc.right - text_rc.left) / 2);
                draw_text(hdc, &status, text_rc, color, DT_RIGHT);
                let name_rc = RECT { right: text_rc.right - status_w - pad, ..text_rc };
                draw_text(hdc, &c.name, name_rc, TEXT, DT_LEFT);
                if let Some(url) = &c.cfg.url {
                    let left = name_rc.left + text_width(hdc, &c.name) + pad;
                    let right = (left + text_width_font(hdc, url, self.link_font)).min(name_rc.right);
                    if right - left > self.scale(24) {
                        let rc = RECT { left, right, ..name_rc };
                        SelectObject(hdc, self.link_font.into());
                        draw_text(hdc, url, rc, LINK, DT_LEFT);
                        SelectObject(hdc, self.font.into());
                        links.push((i, rc));
                    }
                }
                if self.zoomed.is_some() {
                    draw_text(hdc, "double-click to restore the grid", text_rc, DIM, DT_CENTER);
                }
                if c.window.is_none() {
                    FillRect(hdc, &b, panel);
                    match self.body_hint(i) {
                        Some((hint, color)) => {
                            let mid = (b.top + b.bottom) / 2;
                            let line = self.scale(12);
                            draw_text(hdc, &c.cfg.command, RECT { top: mid - 2 * line, bottom: mid, ..b }, DIM, DT_CENTER);
                            draw_text(hdc, &hint, RECT { top: mid, bottom: mid + 2 * line, ..b }, color, DT_CENTER);
                        }
                        None => draw_text(hdc, &c.cfg.command, b, DIM, DT_CENTER),
                    }
                }
            }

            SelectObject(hdc, old_font);
            for brush in [bg, header, header_focused, header_crashed, panel] {
                let _ = DeleteObject(brush.into());
            }
            let _ = EndPaint(self.hwnd, &ps);
        }
        self.links = links;
    }

    /// The first dependency of child `i` that isn't ready, with why, e.g. `solr (not ready)`.
    fn waiting_for(&self, i: usize) -> Option<String> {
        if let Some(run) = self.run.as_ref().filter(|r| r.task == i) {
            let busy: Vec<&str> = run
                .apps
                .iter()
                .filter(|&&a| matches!(self.children[a].status, Status::StopQueued | Status::Stopping))
                .map(|&a| self.children[a].name.as_str())
                .collect();
            if !busy.is_empty() {
                return Some(format!("{} to stop", busy.join(", ")));
            }
        }
        let d = *self.deps[i].iter().find(|&&d| !self.children[d].is_ready())?;
        let dep = &self.children[d];
        let why = match &dep.status {
            Status::Exited(code) => format!(" (exited with code {code})"),
            Status::Failed(_) => " (failed to launch)".into(),
            Status::Stopped => " (stopped)".into(),
            Status::WaitingForWindow | Status::Running if dep.readiness == Readiness::TimedOut => {
                " (not ready)".into()
            }
            _ => String::new(),
        };
        Some(format!("{}{why}", dep.name))
    }

    fn status_text(&self, i: usize) -> (String, COLORREF) {
        let c = &self.children[i];
        let countdown = c.restart_in_secs().map(|s| format!(", restarting in {s}s")).unwrap_or_default();
        match &c.status {
            Status::Waiting => match self.waiting_for(i) {
                Some(w) => (format!("waiting for {w}…"), WARN),
                None => ("starting…".into(), WARN),
            },
            Status::Checking => ("checking…".into(), WARN),
            Status::External => ("already running".into(), OK),
            Status::Skipped(_) => ("skipped".into(), DIM),
            Status::WaitingForWindow | Status::Running if c.healthy == Some(false) => {
                (format!("unhealthy (pid {})", c.pid), BAD)
            }
            Status::WaitingForWindow | Status::Running => match (c.readiness, c.window.is_some()) {
                (Readiness::Pending, true) => (format!("starting… (pid {})", c.pid), WARN),
                (Readiness::TimedOut, _) => (format!("not ready (pid {})", c.pid), BAD),
                (_, true) => (format!("pid {}", c.pid), OK),
                (_, false) => ("starting…".into(), WARN),
            },
            Status::Exited(0) if c.cfg.task => ("done".into(), OK),
            Status::Exited(code) => (format!("exited ({code}){countdown}"), if *code == 0 { DIM } else { BAD }),
            Status::Failed(e) => (format!("failed: {}{countdown}", e.trim()), BAD),
            Status::Stopped => ("stopped".into(), DIM),
            Status::StopQueued => ("waiting to stop…".into(), DIM),
            Status::Stopping => ("stopping…".into(), WARN),
        }
    }

    /// Text in the body of a tile with no window.
    fn body_hint(&self, i: usize) -> Option<(String, COLORREF)> {
        if self.shutting_down {
            return None;
        }
        let c = &self.children[i];
        match &c.status {
            Status::Waiting => self.waiting_for(i).map(|w| (format!("Waiting for {w}"), WARN)),
            Status::Checking => Some(("Checking whether it's already running…".into(), WARN)),
            Status::External => {
                let what = c.external.clone().unwrap_or_default();
                Some((format!("Already running ({what}), so not launched. Click to check again."), OK))
            }
            Status::Skipped(why) => Some((format!("Skipped: {why}."), DIM)),
            Status::Stopped => Some(("Stopped. Click to start.".into(), TEXT)),
            _ if c.restart_in_secs().is_some() => Some((
                format!("Restarting in {}s… (click to restart now)", c.restart_in_secs().unwrap_or(0)),
                WARN,
            )),
            _ if c.can_restart() => Some(("Click to restart".into(), TEXT)),
            _ => None,
        }
    }

    /// Index of the tile under `pt` if clicking it would start its app.
    fn restartable_at(&self, pt: POINT) -> Option<usize> {
        if self.shutting_down {
            return None;
        }
        // Only the body: the header is for double-click zoom.
        self.cells()
            .iter()
            .position(|cell| contains(&self.split(cell).1, pt))
            .filter(|&i| !self.is_hidden(i) && self.children[i].window.is_none() && self.children[i].can_restart())
    }

    fn link_at(&self, pt: POINT) -> Option<usize> {
        self.links.iter().find(|(_, rc)| contains(rc, pt)).map(|(i, _)| *i)
    }

    fn click(&mut self, pt: POINT) {
        if let Some(i) = self.link_at(pt) {
            if let Some(url) = &self.children[i].cfg.url {
                unsafe {
                    ShellExecuteW(Some(self.hwnd), w!("open"), &HSTRING::from(url), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
                }
            }
            return;
        }
        if let Some(i) = self.restartable_at(pt) {
            self.children[i].request_start();
            self.gate();
            self.invalidate();
        }
    }

    // ---- Command-line control ----

    fn find(&self, name: &str) -> std::result::Result<usize, String> {
        let hits: Vec<usize> = (0..self.children.len())
            .filter(|&i| self.selected[i] && self.children[i].name.eq_ignore_ascii_case(name.trim()))
            .collect();
        match hits[..] {
            [i] => Ok(i),
            [] => {
                let all: Vec<&str> =
                    (0..self.children.len()).filter(|&i| self.selected[i]).map(|i| self.children[i].name.as_str()).collect();
                Err(format!("no app called \"{name}\"; the apps are: {}", all.join(", ")))
            }
            _ => Err(format!("more than one app is called \"{name}\"")),
        }
    }

    fn control(&mut self, cmd: Cmd) -> std::result::Result<Value, String> {
        let refuse = |ui: &Ui| if ui.shutting_down { Err("the host is shutting down".to_string()) } else { Ok(()) };
        let r = match cmd {
            Cmd::Status => return Ok(self.status_json()),
            Cmd::App(name) => return self.find(&name).map(|i| self.app_json(i)),
            Cmd::ConsolePids(name) => return self.find(&name).map(|i| json!(self.children[i].console_pids())),
            Cmd::Start(name) => {
                refuse(self)?;
                let i = self.find(&name)?;
                events::log(format!("{}: start requested", self.children[i].name));
                if !matches!(self.children[i].status, Status::StopQueued | Status::Stopping) {
                    self.children[i].request_start();
                }
                i
            }
            Cmd::Stop(name) => {
                refuse(self)?;
                let i = self.find(&name)?;
                events::log(format!("{}: stop requested", self.children[i].name));
                self.stop_one(i, false);
                i
            }
            Cmd::Restart(name) => {
                refuse(self)?;
                let i = self.find(&name)?;
                events::log(format!("{}: restart requested", self.children[i].name));
                self.stop_one(i, true);
                i
            }
            Cmd::Activate => {
                unsafe {
                    if IsIconic(self.hwnd).as_bool() {
                        let _ = ShowWindow(self.hwnd, SW_RESTORE);
                    }
                    let _ = SetForegroundWindow(self.hwnd);
                }
                return Ok(json!({ "host_pid": std::process::id() }));
            }
            Cmd::Quit | Cmd::Run { .. } | Cmd::Select { .. } => return Err("not handled here".into()),
        };
        self.gate();
        self.invalidate();
        Ok(self.app_json(r))
    }

    /// Stop one app on request, then start it again if `then_start`.
    fn stop_one(&mut self, i: usize, then_start: bool) {
        let c = &mut self.children[i];
        if matches!(c.status, Status::StopQueued | Status::Stopping) {
            self.start_after_stop[i] |= then_start;
            return;
        }
        match c.stop_plan(self.timeout_ms, &self.helper_exe) {
            Some(plan) => {
                c.begin_stop();
                self.start_after_stop[i] = then_start;
                self.manual_stops += 1;
                let host = self.hwnd.0 as isize;
                std::thread::spawn(move || {
                    let outcome = plan.run();
                    unsafe {
                        let _ = PostMessageW(
                            Some(HWND(host as *mut _)),
                            WM_APP_MANUAL_STOPPED,
                            WPARAM(i),
                            LPARAM(encode_outcome(outcome)),
                        );
                    }
                });
            }
            None if then_start => c.request_start(),
            None if matches!(c.status, Status::Skipped(_)) => {}
            None => c.status = Status::Stopped,
        }
    }

    /// `brady-ctl run`: stop the apps (dependents first), then rerun the task and start
    /// them again. Answered once the task has been launched.
    fn start_run(&mut self, task: &str, stop: &[String], reply: Reply) {
        let (t, apps) = match self.plan_run(task, stop) {
            Ok(p) => p,
            Err(e) => {
                let _ = reply.send(Err(e));
                return;
            }
        };
        let names: Vec<&str> = apps.iter().map(|&i| self.children[i].name.as_str()).collect();
        events::log(format!(
            "{}: run requested{}",
            self.children[t].name,
            if names.is_empty() { String::new() } else { format!("; stopping {}", names.join(", ")) }
        ));
        let mut plans = Vec::new();
        for &i in &apps {
            if let Some(p) = self.children[i].stop_plan(self.timeout_ms, &self.helper_exe) {
                plans.push((i, p));
            }
        }
        // Counted like `brady-ctl stop`, so a shutdown meanwhile waits for them.
        self.manual_stops += plans.len();
        // Back to waiting, held by `self.run` until the apps have stopped. This also
        // closes the console kept open from its last run.
        self.children[t].request_start();
        self.run = Some(Run { task: t, apps, reply });
        let host = self.hwnd.0 as isize;
        std::thread::spawn(move || {
            let post = |msg, i, lp| unsafe {
                let _ = PostMessageW(Some(HWND(host as *mut _)), msg, WPARAM(i), LPARAM(lp));
            };
            for (i, plan) in plans {
                post(WM_APP_STOPPING, i, 0);
                let outcome = plan.run();
                post(WM_APP_MANUAL_STOPPED, i, encode_outcome(outcome));
            }
            post(WM_APP_RUN_GO, 0, 0);
        });
        self.layout();
        self.invalidate();
    }

    /// The task and the apps to stop for `run`, dependents first.
    fn plan_run(&self, task: &str, stop: &[String]) -> std::result::Result<(usize, Vec<usize>), String> {
        if self.shutting_down {
            return Err("the host is shutting down".into());
        }
        if self.run.is_some() {
            return Err("another run is still stopping apps".into());
        }
        let t = self.find(task)?;
        let name = &self.children[t].name;
        if !self.children[t].cfg.task {
            return Err(format!("{name} is an app, not a task; use brady-ctl restart {name}"));
        }
        if let Some(busy) = self.children.iter().find(|c| c.cfg.task && c.is_active()) {
            return Err(format!("the task {} is still running; wait for it to finish", busy.name));
        }
        let mut apps = Vec::new();
        // The task's own `stops` may name apps a later `up --exact` left out.
        let own = self.children[t].cfg.stops.iter().filter(|n| self.find(n).is_ok());
        for n in own.chain(stop) {
            let i = self.find(n)?;
            if self.children[i].cfg.task {
                return Err(format!("{} is a task; only apps can be stopped", self.children[i].name));
            }
            if !apps.contains(&i) {
                apps.push(i);
            }
        }
        let mut order = config::in_start_order(&self.deps, &apps);
        order.reverse();
        Ok((t, order))
    }

    /// The apps for `run` have stopped: let the task run, and queue the apps behind it.
    fn on_run_go(&mut self) {
        let Some(run) = self.run.take() else { return };
        if self.shutting_down {
            let _ = run.reply.send(Err("the host is shutting down".into()));
            return;
        }
        for &i in &run.apps {
            if self.children[i].can_restart() {
                self.children[i].request_start();
            }
        }
        self.gate();
        self.layout();
        self.invalidate();
        // `run.apps` is in stop order (dependents first); report them in the order they start.
        let restarting = config::in_start_order(&self.deps, &run.apps);
        let _ = run.reply.send(Ok(json!({
            "task": self.children[run.task].name,
            "restarting": restarting.iter().map(|&i| self.children[i].name.as_str()).collect::<Vec<_>>(),
        })));
    }

    /// `brady-ctl up` with another selection: start the apps it adds, and with `exact`
    /// stop the ones it leaves out. Apps in both keep running. Tasks aren't rerun.
    fn select(&mut self, sel: config::Selection, exact: bool) -> std::result::Result<Value, String> {
        if self.shutting_down {
            return Err("the host is shutting down".into());
        }
        if self.run.is_some() {
            return Err("a brady-ctl run is stopping apps; try again once it has finished".into());
        }
        let cfg = config::load(&self.config_path, &sel)?;
        let mut added = Vec::new();
        let mut index = Vec::with_capacity(cfg.entries.len());
        for e in &cfg.entries {
            let i = match self.children.iter().position(|c| c.name.eq_ignore_ascii_case(e.name())) {
                Some(i) => {
                    if !self.selected[i] {
                        self.selected[i] = true;
                        added.push(i);
                        if self.children[i].can_restart() {
                            self.children[i].request_start();
                        }
                    }
                    i
                }
                None => {
                    let i = self.children.len();
                    let mut c = Child::new(i, e, &self.ctx);
                    if e.task {
                        // Only new if the config gained a task since the host started.
                        c.status = Status::Skipped("added to the config after the host started".into());
                    } else {
                        added.push(i);
                    }
                    self.children.push(c);
                    self.selected.push(true);
                    self.start_after_stop.push(false);
                    self.deps.push(Vec::new());
                    i
                }
            };
            index.push(i);
        }
        for (k, d) in cfg.deps.iter().enumerate() {
            self.deps[index[k]] = d.iter().map(|&j| index[j]).collect();
        }
        // Tasks this selection would run that were skipped when the host started.
        let skipped: Vec<String> = cfg
            .entries
            .iter()
            .zip(&index)
            .filter(|&(e, &i)| !e.held && matches!(self.children[i].status, Status::Skipped(_)))
            .map(|(_, &i)| self.children[i].name.clone())
            .collect();
        let mut leaving = Vec::new();
        if exact {
            let out: Vec<usize> = (0..self.children.len())
                .filter(|&i| self.selected[i] && !self.children[i].cfg.task && !index.contains(&i))
                .collect();
            let mut order = config::in_start_order(&self.deps, &out);
            order.reverse();
            let mut plans = Vec::new();
            for &i in &order {
                self.selected[i] = false;
                self.focused = self.focused.filter(|&f| f != i);
                self.zoomed = self.zoomed.filter(|&z| z != i);
                match self.children[i].stop_plan(self.timeout_ms, &self.helper_exe) {
                    Some(p) => plans.push((i, p)),
                    None => self.children[i].status = Status::Stopped,
                }
            }
            leaving = order;
            self.manual_stops += plans.len();
            let host = self.hwnd.0 as isize;
            std::thread::spawn(move || {
                let post = |msg, i, lp| unsafe {
                    let _ = PostMessageW(Some(HWND(host as *mut _)), msg, WPARAM(i), LPARAM(lp));
                };
                for (i, plan) in plans {
                    post(WM_APP_STOPPING, i, 0);
                    let outcome = plan.run();
                    post(WM_APP_MANUAL_STOPPED, i, encode_outcome(outcome));
                }
            });
        }
        self.selection = cfg.selection.clone();
        let names = |list: &[usize]| -> Vec<String> { list.iter().map(|&i| self.children[i].name.clone()).collect() };
        let (added_names, leaving_names) = (names(&config::in_start_order(&self.deps, &added)), names(&leaving));
        events::log(format!(
            "selection changed ({}){}{}",
            if self.selection.to_args().is_empty() { "everything".to_string() } else { self.selection.to_args().join(" ") },
            if added_names.is_empty() { String::new() } else { format!("; starting {}", added_names.join(", ")) },
            if leaving_names.is_empty() { String::new() } else { format!("; stopping {}", leaving_names.join(", ")) },
        ));
        self.gate();
        self.layout();
        self.invalidate();
        Ok(json!({
            "added": added_names,
            "stopping": leaving_names,
            "skipped_tasks": skipped,
            "apps": cfg.entries.iter().map(|e| e.name()).collect::<Vec<_>>(),
        }))
    }

    fn on_manual_stopped(&mut self, index: usize, outcome: StopOutcome) -> bool {
        self.manual_stops = self.manual_stops.saturating_sub(1);
        if let Some(c) = self.children.get_mut(index) {
            c.finish_stop(outcome);
        }
        self.invalidate();
        if self.shutting_down {
            return self.on_stopped(usize::MAX, outcome);
        }
        if std::mem::take(&mut self.start_after_stop[index]) {
            self.children[index].request_start();
            self.gate();
        }
        self.layout();
        false
    }

    fn status_json(&self) -> Value {
        json!({
            "version": config::VERSION,
            "host_pid": std::process::id(),
            "started_at": events::iso_utc(self.started_at),
            "now": events::iso_utc(SystemTime::now()),
            "config": self.config_path.to_string_lossy(),
            "title": self.title,
            "profiles": self.selection.profiles,
            "only": self.selection.only,
            "skip": self.selection.skip,
            "event_log": self.event_log.as_ref().map(|p| p.to_string_lossy()),
            "shutting_down": self.shutting_down,
            "apps": (0..self.children.len())
                .filter(|&i| self.selected[i] || matches!(self.children[i].status, Status::StopQueued | Status::Stopping))
                .map(|i| self.app_json(i))
                .collect::<Vec<_>>(),
        })
    }

    fn app_json(&self, i: usize) -> Value {
        let c = &self.children[i];
        let check = c.cfg.ready.as_ref().map(Check::describe);
        let checking = if c.cfg.task {
            let apps: Vec<&str> = c.cfg.skip_checks.iter().map(|(a, _)| a.as_str()).collect();
            format!("checking whether {} is already running", apps.join(" or "))
        } else {
            let what = c.cfg.running_check().map(|r| Check::describe_running(&r)).unwrap_or_default();
            format!("checking whether it's already running ({})", what.trim_end_matches(" answered").trim_end_matches(" is running"))
        };
        let (state, reason): (&str, Option<String>) = match &c.status {
            Status::Waiting => ("waiting", self.waiting_for(i).map(|w| format!("waiting for {w}"))),
            Status::Checking => ("checking", Some(checking)),
            Status::External => ("external", c.external.as_ref().map(|w| format!("{w}, so it wasn't launched"))),
            Status::Skipped(why) => ("skipped", Some(format!("{why}; brady-ctl run {} runs it", c.name))),
            Status::WaitingForWindow | Status::Running if c.healthy == Some(false) => {
                ("unhealthy", c.unhealthy_reason())
            }
            Status::WaitingForWindow | Status::Running => match c.readiness {
                Readiness::Pending => ("starting", check.map(|w| format!("waiting for {w}"))),
                Readiness::TimedOut => (
                    "not-ready",
                    check.map(|w| format!("{w} didn't pass within {} ms", c.cfg.ready_timeout_ms())),
                ),
                _ => ("running", None),
            },
            Status::Exited(0) if c.cfg.task => ("done", None),
            Status::Exited(code) => ("exited", Some(format!("exited with code {code}"))),
            Status::Failed(e) => ("failed", Some(e.clone())),
            Status::Stopped => (
                "stopped",
                c.last_stop.map(|s| format!("stopped by {} after {} ms", s.by.as_str(), s.ms)),
            ),
            Status::StopQueued | Status::Stopping => ("stopping", None),
        };
        let exit_code = match c.status {
            Status::Exited(code) => Some(code),
            _ => None,
        };
        let running = c.is_active() || matches!(c.status, Status::StopQueued | Status::Stopping);
        json!({
            "name": c.name,
            "task": c.cfg.task,
            "state": state,
            "ready": c.is_ready(),
            "reason": reason,
            "pid": running.then_some(c.pid),
            "exit_code": exit_code,
            "restarts": c.restarts,
            "restart_in_ms": c.restart_at.map(|t| t.saturating_duration_since(std::time::Instant::now()).as_millis() as u64),
            "depends_on": self.deps[i].iter().map(|&d| self.children[d].name.as_str()).collect::<Vec<_>>(),
            "command": c.cfg.command,
            "url": c.cfg.url,
            "log": c.cfg.log,
            "last_stop": c.last_stop.map(|s| json!({ "by": s.by.as_str(), "ms": s.ms })),
            "healthy": c.healthy,
            "crashed": c.crashed,
            "launched_at": c.launched_at.map(events::iso_utc),
            "ready_at": c.ready_at.map(events::iso_utc),
            "exited_at": c.exited_at.map(events::iso_utc),
            "uptime_ms": c.is_active().then(|| c.launched_at.and_then(|t| t.elapsed().ok())).flatten().map(|d| d.as_millis() as u64),
        })
    }

    // ---- Shutdown ----

    /// Start graceful shutdown. Returns true if the window can be destroyed now.
    fn request_close(&mut self) -> bool {
        if self.shutting_down {
            // Second close request: stop waiting and kill everything.
            events::log("second close request: killing everything");
            for c in &self.children {
                c.kill();
            }
            return false;
        }
        self.shutting_down = true;
        events::log("shutdown requested");
        if self.remember_layout {
            let zoomed = self.zoomed.map(|i| self.children[i].name.clone());
            if let Some(l) = layout::capture(self.hwnd, zoomed) {
                layout::save(&self.config_path, &l);
            }
        }
        unsafe {
            let _ = SetWindowTextW(self.hwnd, &HSTRING::from(format!("{} — shutting down…", self.title)));
        }
        // Stop apps one at a time, each after everything that depends on it: reverse
        // start order, so the first apps in the config stay up longest.
        let order = config::start_order(&self.deps);
        let mut plans = Vec::new();
        for &i in order.iter().rev() {
            if let Some(p) = self.children[i].stop_plan(self.timeout_ms, &self.helper_exe) {
                plans.push((i, p));
            }
        }
        // Stops requested from the command line are still running; wait for them too.
        self.pending_stops = plans.len() + self.manual_stops;
        let host = self.hwnd.0 as isize;
        std::thread::spawn(move || {
            let post = |msg, i, lp| unsafe {
                let _ = PostMessageW(Some(HWND(host as *mut _)), msg, WPARAM(i), LPARAM(lp));
            };
            for (i, plan) in plans {
                post(WM_APP_STOPPING, i, 0);
                events::log(format!("{}: stopping", plan.name));
                let outcome = plan.run();
                post(WM_APP_STOPPED, i, encode_outcome(outcome));
            }
        });
        self.invalidate();
        self.pending_stops == 0 && self.release_all()
    }

    fn on_stopping(&mut self, index: usize) {
        if let Some(c) = self.children.get_mut(index) {
            c.begin_stop();
        }
        self.invalidate();
    }

    fn on_stopped(&mut self, index: usize, outcome: StopOutcome) -> bool {
        if let Some(c) = self.children.get_mut(index) {
            c.finish_stop(outcome);
        }
        self.pending_stops = self.pending_stops.saturating_sub(1);
        self.invalidate();
        self.pending_stops == 0 && self.release_all()
    }

    /// Detach any window that outlived shutdown so destroying the host doesn't take it down.
    fn release_all(&mut self) -> bool {
        for c in &mut self.children {
            c.release_window();
        }
        true
    }

    fn set_dpi(&mut self, dpi: u32) {
        self.dpi = dpi;
        unsafe {
            let _ = DeleteObject(self.font.into());
            let _ = DeleteObject(self.link_font.into());
        }
        self.font = message_font(dpi, false);
        self.link_font = message_font(dpi, true);
    }
}

/// Space between the window edge and the tiles, in 96-DPI pixels.
const MARGIN: i32 = 10;
/// Space between neighbouring tiles, in 96-DPI pixels.
const GAP: i32 = 8;

/// Lay out `n` tiles in `cols` columns inside `client`, with `margin` around the grid
/// and `gap` between tiles. A short last row stretches to fill the width.
fn grid(client: RECT, n: usize, cols: usize, margin: i32, gap: i32) -> Vec<RECT> {
    let rows = n.div_ceil(cols) as i32;
    let width = (client.right - client.left - 2 * margin).max(0);
    let height = (client.bottom - client.top - 2 * margin).max(0);
    // Start and end of slot `i` of `count` along a span, gaps excluded.
    let span = |len: i32, i: i32, count: i32| {
        let usable = (len - gap * (count - 1)).max(0);
        (usable * i / count + gap * i, usable * (i + 1) / count + gap * i)
    };
    (0..n)
        .map(|i| {
            let (row, col) = ((i / cols) as i32, (i % cols) as i32);
            let in_row = cols.min(n - row as usize * cols) as i32;
            let (left, right) = span(width, col, in_row);
            let (top, bottom) = span(height, row, rows);
            RECT {
                left: client.left + margin + left,
                top: client.top + margin + top,
                right: client.left + margin + right,
                bottom: client.top + margin + bottom,
            }
        })
        .collect()
}

fn contains(r: &RECT, pt: POINT) -> bool {
    pt.x >= r.left && pt.x < r.right && pt.y >= r.top && pt.y < r.bottom
}

unsafe fn draw_text(hdc: HDC, text: &str, mut rc: RECT, color: COLORREF, align: DRAW_TEXT_FORMAT) {
    let mut buf: Vec<u16> = text.encode_utf16().collect();
    unsafe {
        SetTextColor(hdc, color);
        DrawTextW(hdc, &mut buf, &mut rc, align | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX);
    }
}

fn text_width(hdc: HDC, text: &str) -> i32 {
    let buf: Vec<u16> = text.encode_utf16().collect();
    let mut size = SIZE::default();
    unsafe {
        let _ = GetTextExtentPoint32W(hdc, &buf, &mut size);
    }
    size.cx
}

fn text_width_font(hdc: HDC, text: &str, font: HFONT) -> i32 {
    unsafe {
        let old = SelectObject(hdc, font.into());
        let w = text_width(hdc, text);
        SelectObject(hdc, old);
        w
    }
}

fn message_font(dpi: u32, underline: bool) -> HFONT {
    unsafe {
        let mut m = NONCLIENTMETRICSW { cbSize: size_of::<NONCLIENTMETRICSW>() as u32, ..Default::default() };
        let _ = SystemParametersInfoForDpi(
            SPI_GETNONCLIENTMETRICS.0,
            m.cbSize,
            Some(&mut m as *mut _ as *mut _),
            0,
            dpi,
        );
        m.lfMessageFont.lfUnderline = underline as u8;
        CreateFontIndirectW(&m.lfMessageFont)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT { left, top, right, bottom }
    }

    #[test]
    fn grid_has_margins_and_gaps() {
        // 2x2 in a 220x120 client: 10 margin, 8 gap -> 200x100 usable, cells 96x46.
        let cells = grid(rect(0, 0, 220, 120), 4, 2, 10, 8);
        assert_eq!(
            cells,
            [rect(10, 10, 106, 56), rect(114, 10, 210, 56), rect(10, 64, 106, 110), rect(114, 64, 210, 110)]
        );
    }

    #[test]
    fn short_last_row_stretches() {
        let cells = grid(rect(0, 0, 220, 120), 3, 2, 10, 8);
        assert_eq!(cells[2], rect(10, 64, 210, 110));
    }

    #[test]
    fn single_tile_fills_inside_margin() {
        assert_eq!(grid(rect(0, 0, 220, 120), 1, 1, 10, 8), [rect(10, 10, 210, 110)]);
    }

    #[test]
    fn stop_outcome_round_trips_through_lparam() {
        let o = StopOutcome { by: StopBy::Close, ms: 31_234 };
        assert_eq!(decode_outcome(LPARAM(encode_outcome(o))), o);
    }

    #[test]
    fn launch_args() {
        let args: Vec<String> =
            ["webdev.toml", "--skip", "worker, functions", "--only", "api"].iter().map(|s| s.to_string()).collect();
        let a = parse_launch_args(&args).unwrap();
        assert_eq!(a.path, Some(PathBuf::from("webdev.toml")));
        assert_eq!(a.sel.skip, ["worker", "functions"]);
        assert_eq!(a.sel.only, ["api"]);
        assert!(!a.new_instance);
        assert!(parse_launch_args(&["--bogus".to_string()]).is_err());
        let a = parse_launch_args(&["--new-instance".to_string(), "--no-dialog".to_string()]).unwrap();
        assert!(a.new_instance && a.path.is_none());
    }
}
