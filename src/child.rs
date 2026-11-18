//! Child process lifecycle: launch into a job object, find and embed its window,
//! and shut it down gracefully.

use crate::config::{AppConfig, Shutdown};
use crate::ctrlc;
use std::ffi::c_void;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::Storage::FileSystem::SearchPathW;
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicAccountingInformation, JobObjectBasicProcessIdList,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject,
};
use windows::Win32::System::Threading::{
    CREATE_SUSPENDED, CreateProcessW, GetExitCodeProcess, PROCESS_INFORMATION, ResumeThread,
    STARTUPINFOW, TerminateProcess,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, HSTRING, PCWSTR, PWSTR, Result, w};

/// Owned kernel handle, closed on drop.
pub struct Handle(pub HANDLE);
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    WaitingForWindow,
    Running,
    Exited(u32),
    Failed(String),
    Stopping,
    Stopped,
}

/// A foreign window reparented into the host, with the styles needed to restore it.
pub struct Embedded {
    pub hwnd: HWND,
    style: u32,
    ex_style: u32,
}

pub struct Child {
    pub name: String,
    pub cfg: AppConfig,
    pub console: bool,
    /// One job per app: tracks every process it spawns (including launcher hand-offs)
    /// and kills them all if the host dies without a clean shutdown.
    job: Option<Arc<Handle>>,
    process: Option<Handle>,
    pub pid: u32,
    pub window: Option<Embedded>,
    pub status: Status,
}

impl Child {
    pub fn spawn(cfg: &AppConfig) -> Child {
        let exe = first_token(&cfg.command);
        let name = cfg.name.clone().unwrap_or_else(|| {
            Path::new(&exe)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| exe.clone())
        });
        let console = cfg.console.unwrap_or_else(|| is_console_exe(&exe));
        let mut child = Child {
            name,
            cfg: cfg.clone(),
            console,
            job: None,
            process: None,
            pid: 0,
            window: None,
            status: Status::WaitingForWindow,
        };
        match unsafe { launch(&cfg.command, cfg.cwd.as_deref(), console) } {
            Ok((job, process, pid)) => {
                child.job = Some(Arc::new(job));
                child.process = Some(process);
                child.pid = pid;
            }
            Err(e) => child.status = Status::Failed(e.message()),
        }
        child
    }

    pub fn is_active(&self) -> bool {
        matches!(self.status, Status::WaitingForWindow | Status::Running)
    }

    /// Track exit and window changes. Returns (repaint needed, relayout needed).
    pub fn poll(&mut self, host: HWND, shutting_down: bool) -> (bool, bool) {
        let Some(job) = self.job.clone() else {
            return (false, false);
        };
        let mut changed = false;
        let mut relayout = false;

        if let Some(w) = &self.window {
            if !unsafe { IsWindow(Some(w.hwnd)) }.as_bool() {
                self.window = None;
                changed = true;
            }
        }
        if !self.is_active() {
            return (changed, false);
        }
        if job_active_count(job.0) == 0 {
            self.status = Status::Exited(self.exit_code());
            self.window = None;
            return (true, false);
        }
        if self.window.is_none() && !shutting_down {
            if let Some(hwnd) = top_level_windows(&job_pids(job.0)).into_iter().next() {
                self.window = Some(unsafe { embed(hwnd, host) });
                relayout = true;
            }
        }
        let status = if self.window.is_some() { Status::Running } else { Status::WaitingForWindow };
        if status != self.status {
            self.status = status;
            changed = true;
        }
        (changed || relayout, relayout)
    }

    fn exit_code(&self) -> u32 {
        let mut code = 0u32;
        if let Some(p) = &self.process {
            let _ = unsafe { GetExitCodeProcess(p.0, &mut code) };
        }
        code
    }

    /// Build the shutdown plan for a background thread, or None if nothing is running.
    pub fn stop_plan(&mut self, default_timeout_ms: u64, helper_exe: &Path) -> Option<StopPlan> {
        if !self.is_active() {
            return None;
        }
        let job = self.job.clone()?;
        self.status = Status::Stopping;
        Some(StopPlan {
            job,
            console: self.console,
            window: self.window.as_ref().map(|w| w.hwnd.0 as isize),
            method: self.cfg.shutdown,
            timeout: Duration::from_millis(self.cfg.shutdown_timeout_ms.unwrap_or(default_timeout_ms)),
            helper_exe: helper_exe.to_path_buf(),
        })
    }

    pub fn finish_stop(&mut self) {
        let alive = self.job.as_ref().is_some_and(|j| job_active_count(j.0) > 0);
        self.status = if alive { Status::Stopping } else { Status::Stopped };
    }

    pub fn kill(&self) {
        if let Some(job) = &self.job {
            let _ = unsafe { TerminateJobObject(job.0, 1) };
        }
    }

    /// Hand a still-living window back to the desktop so it isn't destroyed with the host.
    pub fn release_window(&mut self) {
        if let Some(w) = self.window.take() {
            unsafe { detach(&w) };
        }
    }
}

pub struct StopPlan {
    job: Arc<Handle>,
    console: bool,
    window: Option<isize>,
    method: Shutdown,
    timeout: Duration,
    helper_exe: PathBuf,
}

#[derive(Clone, Copy)]
enum Step {
    CtrlC,
    Close,
}

impl StopPlan {
    /// Escalate through the configured steps, then terminate. Blocks; run off the UI thread.
    pub fn run(self) {
        let steps: &[Step] = match (self.method, self.console) {
            (Shutdown::Auto, true) => &[Step::CtrlC, Step::Close],
            (Shutdown::Auto, false) | (Shutdown::Close, _) => &[Step::Close],
            (Shutdown::CtrlC, _) => &[Step::CtrlC],
            (Shutdown::Kill, _) => &[],
        };
        for step in steps {
            if job_active_count(self.job.0) == 0 {
                return;
            }
            match step {
                Step::CtrlC => {
                    if !ctrlc::send_to_any(&self.helper_exe, &job_pids(self.job.0)) {
                        continue; // Not a console app after all; escalate immediately.
                    }
                }
                Step::Close => self.post_close(),
            }
            if wait_for_exit(self.job.0, self.timeout) {
                return;
            }
        }
        let _ = unsafe { TerminateJobObject(self.job.0, 1) };
        wait_for_exit(self.job.0, Duration::from_secs(2));
    }

    fn post_close(&self) {
        let mut targets = top_level_windows(&job_pids(self.job.0));
        if let Some(h) = self.window {
            targets.push(HWND(h as *mut c_void));
        }
        for hwnd in targets {
            let _ = unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) };
        }
    }
}

unsafe fn launch(command: &str, cwd: Option<&str>, console: bool) -> Result<(Handle, Handle, u32)> {
    unsafe {
        let job = Handle(CreateJobObjectW(None, PCWSTR::null())?);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            &limits as *const _ as *const c_void,
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )?;

        // Console apps are hosted by an explicit conhost.exe. Otherwise, when Windows
        // Terminal is the default terminal, the console would open as a WT tab that
        // can't be embedded.
        let cmdline = if console {
            format!("\"{}\" {}", conhost_path(), command)
        } else {
            command.to_string()
        };
        let mut cmd_w: Vec<u16> = cmdline.encode_utf16().chain(Some(0)).collect();
        let cwd_h = cwd.map(HSTRING::from);
        let cwd_p = cwd_h.as_ref().map_or(PCWSTR::null(), |h| PCWSTR(h.as_ptr()));
        let si = STARTUPINFOW { cb: size_of::<STARTUPINFOW>() as u32, ..Default::default() };
        let mut pi = PROCESS_INFORMATION::default();
        CreateProcessW(
            PCWSTR::null(),
            Some(PWSTR(cmd_w.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_SUSPENDED,
            None,
            cwd_p,
            &si,
            &mut pi,
        )?;
        let process = Handle(pi.hProcess);
        let thread = Handle(pi.hThread);
        // Assign before the first instruction runs so every grandchild lands in the job.
        if let Err(e) = AssignProcessToJobObject(job.0, process.0) {
            let _ = TerminateProcess(process.0, 1);
            return Err(e);
        }
        ResumeThread(thread.0);
        Ok((job, process, pi.dwProcessId))
    }
}

fn conhost_path() -> String {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    format!(r"{root}\System32\conhost.exe")
}

fn first_token(command: &str) -> String {
    let s = command.trim_start();
    if let Some(rest) = s.strip_prefix('"') {
        rest.split('"').next().unwrap_or("").to_string()
    } else {
        s.split_whitespace().next().unwrap_or("").to_string()
    }
}

fn is_console_exe(exe: &str) -> bool {
    let lower = exe.to_ascii_lowercase();
    if lower.ends_with(".bat") || lower.ends_with(".cmd") {
        return true;
    }
    resolve_exe(exe).and_then(|p| pe_subsystem(&p)) == Some(3) // IMAGE_SUBSYSTEM_WINDOWS_CUI
}

fn resolve_exe(exe: &str) -> Option<PathBuf> {
    let p = Path::new(exe);
    if p.is_file() {
        return Some(p.to_path_buf());
    }
    let mut buf = vec![0u16; 1024];
    let n = unsafe {
        SearchPathW(PCWSTR::null(), &HSTRING::from(exe), w!(".exe"), Some(&mut buf), None)
    } as usize;
    (n > 0 && n < buf.len()).then(|| PathBuf::from(String::from_utf16_lossy(&buf[..n])))
}

fn pe_subsystem(path: &Path) -> Option<u16> {
    let mut head = vec![0u8; 4096];
    let n = std::fs::File::open(path).ok()?.read(&mut head).ok()?;
    let head = &head[..n];
    let u32_at = |o: usize| head.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    let pe = u32_at(0x3C)? as usize;
    if head.get(pe..pe + 4)? != b"PE\0\0" {
        return None;
    }
    // Subsystem: PE signature (4) + COFF header (20) + 68 into the optional header.
    let o = pe + 24 + 68;
    head.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]))
}

fn job_active_count(job: HANDLE) -> u32 {
    let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
    unsafe {
        QueryInformationJobObject(
            Some(job),
            JobObjectBasicAccountingInformation,
            &mut info as *mut _ as *mut c_void,
            size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
            None,
        )
    }
    .map_or(0, |_| info.ActiveProcesses)
}

fn job_pids(job: HANDLE) -> Vec<u32> {
    #[repr(C)]
    struct PidList {
        assigned: u32,
        listed: u32,
        ids: [usize; 256],
    }
    let mut list = PidList { assigned: 0, listed: 0, ids: [0; 256] };
    let ok = unsafe {
        QueryInformationJobObject(
            Some(job),
            JobObjectBasicProcessIdList,
            &mut list as *mut _ as *mut c_void,
            size_of::<PidList>() as u32,
            None,
        )
    };
    if ok.is_err() {
        return Vec::new();
    }
    list.ids[..(list.listed as usize).min(256)].iter().map(|&p| p as u32).collect()
}

fn wait_for_exit(job: HANDLE, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if job_active_count(job) == 0 {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Visible, unowned, non-tool top-level windows belonging to any of `pids`, in z-order.
fn top_level_windows(pids: &[u32]) -> Vec<HWND> {
    struct Search<'a> {
        pids: &'a [u32],
        found: Vec<HWND>,
    }
    unsafe extern "system" fn visit(hwnd: HWND, lp: LPARAM) -> BOOL {
        unsafe {
            let s = &mut *(lp.0 as *mut Search);
            let mut pid = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            let owned = GetWindow(hwnd, GW_OWNER).is_ok_and(|o| !o.is_invalid());
            let tool = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0;
            if s.pids.contains(&pid) && IsWindowVisible(hwnd).as_bool() && !owned && !tool {
                s.found.push(hwnd);
            }
            BOOL(1)
        }
    }
    if pids.is_empty() {
        return Vec::new();
    }
    let mut s = Search { pids, found: Vec::new() };
    let _ = unsafe { EnumWindows(Some(visit), LPARAM(&mut s as *mut _ as isize)) };
    s.found
}

unsafe fn embed(hwnd: HWND, host: HWND) -> Embedded {
    unsafe {
        if IsIconic(hwnd).as_bool() || IsZoomed(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
        let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let strip = WS_POPUP | WS_CAPTION | WS_THICKFRAME | WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX;
        let ex_strip = WS_EX_APPWINDOW | WS_EX_DLGMODALFRAME | WS_EX_WINDOWEDGE;
        // Style must be WS_CHILD before SetParent.
        SetWindowLongPtrW(hwnd, GWL_STYLE, ((style & !strip.0) | WS_CHILD.0) as i32 as isize);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, (ex_style & !ex_strip.0) as i32 as isize);
        let _ = SetParent(hwnd, Some(host));
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
        Embedded { hwnd, style, ex_style }
    }
}

unsafe fn detach(w: &Embedded) {
    unsafe {
        if !IsWindow(Some(w.hwnd)).as_bool() {
            return;
        }
        let _ = SetParent(w.hwnd, None);
        SetWindowLongPtrW(w.hwnd, GWL_STYLE, w.style as i32 as isize);
        SetWindowLongPtrW(w.hwnd, GWL_EXSTYLE, w.ex_style as i32 as isize);
        let _ = SetWindowPos(
            w.hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED | SWP_SHOWWINDOW,
        );
    }
}
