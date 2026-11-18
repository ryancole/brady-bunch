#![windows_subsystem = "windows"]

mod child;
mod config;
mod ctrlc;

use child::{Child, Status};
use std::cell::RefCell;
use std::path::PathBuf;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, SetProcessDpiAwarenessContext,
    SystemParametersInfoForDpi,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, Result, w};

const TIMER_POLL: usize = 1;
const POLL_MS: u32 = 150;
/// Posted by shutdown threads: wParam = child index.
const WM_APP_STOPPED: u32 = WM_APP + 1;

const BG: COLORREF = rgb(24, 24, 24);
const HEADER: COLORREF = rgb(45, 45, 48);
const PANEL: COLORREF = rgb(32, 32, 34);
const TEXT: COLORREF = rgb(230, 230, 230);
const DIM: COLORREF = rgb(150, 150, 150);
const OK: COLORREF = rgb(110, 200, 120);
const WARN: COLORREF = rgb(230, 180, 80);
const BAD: COLORREF = rgb(235, 100, 100);

const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

struct Ui {
    hwnd: HWND,
    title: String,
    columns: Option<u32>,
    timeout_ms: u64,
    helper_exe: PathBuf,
    children: Vec<Child>,
    shutting_down: bool,
    pending_stops: usize,
    dpi: u32,
    font: HFONT,
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
    if args.get(1).map(String::as_str) == Some(ctrlc::HELPER_FLAG) {
        let pid = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
        std::process::exit(ctrlc::helper_main(pid));
    }

    let path = config::resolve_path(args.get(1).map(PathBuf::from));
    let cfg = match config::load(&path) {
        Ok(c) => c,
        Err(e) => fatal(&e),
    };
    if let Err(e) = unsafe { run(cfg) } {
        fatal(&e.message());
    }
}

fn fatal(msg: &str) -> ! {
    unsafe {
        MessageBoxW(None, &HSTRING::from(msg), w!("Brady Bunch"), MB_ICONERROR | MB_OK);
    }
    std::process::exit(1)
}

unsafe fn run(cfg: config::Config) -> Result<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let instance = GetModuleHandleW(None)?;
        let class = w!("BradyBunchHost");
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
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
        let children = cfg.apps.iter().map(Child::spawn).collect();
        UI.with(|cell| {
            *cell.borrow_mut() = Some(Ui {
                hwnd,
                title: cfg.title.clone(),
                columns: cfg.columns,
                timeout_ms: cfg.shutdown_timeout_ms,
                helper_exe: std::env::current_exe().unwrap_or_default(),
                children,
                shutting_down: false,
                pending_stops: 0,
                dpi,
                font: message_font(dpi),
            })
        });

        let _ = ShowWindow(hwnd, if cfg.maximized { SW_SHOWMAXIMIZED } else { SW_SHOWDEFAULT });
        SetTimer(Some(hwnd), TIMER_POLL, POLL_MS, None);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        // Dropping the UI closes the job handles; KILL_ON_JOB_CLOSE reaps anything left.
        UI.with(|cell| cell.borrow_mut().take());
        Ok(())
    }
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
            WM_ERASEBKGND => LRESULT(1), // WM_PAINT covers the whole client area.
            WM_CLOSE => {
                if with_ui(Ui::request_close).unwrap_or(true) {
                    let _ = DestroyWindow(hwnd);
                }
                LRESULT(0)
            }
            WM_APP_STOPPED => {
                if with_ui(|ui| ui.on_stopped(wp.0)).unwrap_or(false) {
                    let _ = DestroyWindow(hwnd);
                }
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

    fn poll(&mut self) {
        let (mut repaint, mut relayout) = (false, false);
        for c in &mut self.children {
            let (p, l) = c.poll(self.hwnd, self.shutting_down);
            repaint |= p;
            relayout |= l;
        }
        if relayout {
            self.layout();
        }
        if repaint {
            unsafe {
                let _ = InvalidateRect(Some(self.hwnd), None, false);
            }
        }
    }

    /// Tile rects for each child: a near-square grid; a short last row stretches to fill.
    fn cells(&self) -> Vec<RECT> {
        let n = self.children.len();
        let mut client = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut client);
        }
        if n == 0 {
            return Vec::new();
        }
        let cols = self
            .columns
            .map(|c| c as usize)
            .unwrap_or_else(|| (n as f64).sqrt().ceil() as usize)
            .clamp(1, n);
        let rows = n.div_ceil(cols);
        let gap = self.scale(4);
        let (w, h) = (client.right - gap, client.bottom - gap);
        (0..n)
            .map(|i| {
                let (row, col) = (i / cols, i % cols);
                let in_row = cols.min(n - row * cols) as i32;
                let (row, col, rows) = (row as i32, col as i32, rows as i32);
                RECT {
                    left: gap + w * col / in_row,
                    top: gap + h * row / rows,
                    right: w * (col + 1) / in_row,
                    bottom: h * (row + 1) / rows,
                }
            })
            .collect()
    }

    fn split(&self, cell: &RECT) -> (RECT, RECT) {
        let header_h = self.scale(24);
        let header = RECT { bottom: cell.top + header_h, ..*cell };
        let body = RECT { top: cell.top + header_h, ..*cell };
        (header, body)
    }

    fn layout(&mut self) {
        let cells = self.cells();
        for (c, cell) in self.children.iter().zip(&cells) {
            if let Some(w) = &c.window {
                let (_, b) = self.split(cell);
                unsafe {
                    // Async so a hung child can't freeze the host.
                    let _ = SetWindowPos(
                        w.hwnd,
                        None,
                        b.left,
                        b.top,
                        b.right - b.left,
                        b.bottom - b.top,
                        SWP_NOZORDER | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS,
                    );
                }
            }
        }
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn paint(&mut self) {
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(self.hwnd, &mut ps);
            let mut client = RECT::default();
            let _ = GetClientRect(self.hwnd, &mut client);
            let bg = CreateSolidBrush(BG);
            let header = CreateSolidBrush(HEADER);
            let panel = CreateSolidBrush(PANEL);
            FillRect(hdc, &client, bg);
            let old_font = SelectObject(hdc, self.font.into());
            SetBkMode(hdc, TRANSPARENT);
            let pad = self.scale(8);

            for (c, cell) in self.children.iter().zip(self.cells()) {
                let (h, b) = self.split(&cell);
                FillRect(hdc, &h, header);
                let text_rc = RECT { left: h.left + pad, right: h.right - pad, ..h };
                let (status, color) = status_text(c);
                draw_text(hdc, &status, text_rc, color, DT_RIGHT);
                let name_rc = RECT { right: text_rc.right - self.scale(140), ..text_rc };
                draw_text(hdc, &c.name, name_rc, TEXT, DT_LEFT);
                if c.window.is_none() {
                    FillRect(hdc, &b, panel);
                    draw_text(hdc, &c.cfg.command, b, DIM, DT_CENTER);
                }
            }

            SelectObject(hdc, old_font);
            for brush in [bg, header, panel] {
                let _ = DeleteObject(brush.into());
            }
            let _ = EndPaint(self.hwnd, &ps);
        }
    }

    /// Start graceful shutdown. Returns true if the window can be destroyed now.
    fn request_close(&mut self) -> bool {
        if self.shutting_down {
            // Second close request: stop waiting and kill everything.
            for c in &self.children {
                c.kill();
            }
            return false;
        }
        self.shutting_down = true;
        unsafe {
            let _ = SetWindowTextW(self.hwnd, &HSTRING::from(format!("{} — shutting down…", self.title)));
        }
        let host = self.hwnd.0 as isize;
        for (i, c) in self.children.iter_mut().enumerate() {
            if let Some(plan) = c.stop_plan(self.timeout_ms, &self.helper_exe) {
                self.pending_stops += 1;
                std::thread::spawn(move || {
                    plan.run();
                    unsafe {
                        let _ = PostMessageW(
                            Some(HWND(host as *mut _)),
                            WM_APP_STOPPED,
                            WPARAM(i),
                            LPARAM(0),
                        );
                    }
                });
            }
        }
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
        self.pending_stops == 0 && self.release_all()
    }

    fn on_stopped(&mut self, index: usize) -> bool {
        if let Some(c) = self.children.get_mut(index) {
            c.finish_stop();
        }
        self.pending_stops = self.pending_stops.saturating_sub(1);
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
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
        }
        self.font = message_font(dpi);
    }
}

fn status_text(c: &Child) -> (String, COLORREF) {
    match &c.status {
        Status::WaitingForWindow => ("starting…".into(), WARN),
        Status::Running => (format!("pid {}", c.pid), OK),
        Status::Exited(code) => (format!("exited ({code})"), if *code == 0 { DIM } else { BAD }),
        Status::Failed(e) => (format!("failed: {}", e.trim()), BAD),
        Status::Stopping => ("stopping…".into(), WARN),
        Status::Stopped => ("stopped".into(), DIM),
    }
}

unsafe fn draw_text(hdc: HDC, text: &str, mut rc: RECT, color: COLORREF, align: DRAW_TEXT_FORMAT) {
    let mut buf: Vec<u16> = text.encode_utf16().collect();
    unsafe {
        SetTextColor(hdc, color);
        DrawTextW(hdc, &mut buf, &mut rc, align | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX);
    }
}

fn message_font(dpi: u32) -> HFONT {
    unsafe {
        let mut m = NONCLIENTMETRICSW { cbSize: size_of::<NONCLIENTMETRICSW>() as u32, ..Default::default() };
        let _ = SystemParametersInfoForDpi(
            SPI_GETNONCLIENTMETRICS.0,
            m.cbSize,
            Some(&mut m as *mut _ as *mut _),
            0,
            dpi,
        );
        CreateFontIndirectW(&m.lfMessageFont)
    }
}
