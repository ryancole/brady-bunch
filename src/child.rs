//! Child process lifecycle: launch into a job object, find and embed its window,
//! and shut it down gracefully.

use crate::config::{AppConfig, ExitWith, Restart, Shutdown};
use std::collections::BTreeMap;
use crate::ctrlc;
use std::ffi::c_void;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, INVALID_HANDLE_VALUE, LPARAM, WPARAM};
use windows::Win32::System::IO::{CreateIoCompletionPort, GetQueuedCompletionStatus, OVERLAPPED};
use windows::Win32::Storage::FileSystem::SearchPathW;
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_ASSOCIATE_COMPLETION_PORT, JobObjectAssociateCompletionPortInformation,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicAccountingInformation, JobObjectBasicProcessIdList,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject,
};
use windows::Win32::System::Threading::{
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, GetExitCodeProcess, OpenProcess, PROCESS_INFORMATION,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW, ResumeThread, STARTUPINFOW, TerminateProcess,
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
    /// Waiting for apps launched after this one to stop first.
    StopQueued,
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
    main: Option<Arc<MainProcess>>,
    pub pid: u32,
    pub window: Option<Embedded>,
    pub status: Status,
    started_at: Instant,
    /// When an automatic restart is due, if one is scheduled.
    pub restart_at: Option<Instant>,
    /// Delay before the next automatic restart; grows while the app keeps exiting quickly.
    backoff: Duration,
    shown_secs: Option<u64>,
}

const DEFAULT_RESTART_DELAY: Duration = Duration::from_millis(1000);
const MAX_RESTART_DELAY: Duration = Duration::from_secs(30);
/// An app that ran at least this long before exiting resets the restart backoff.
const STABLE_RUN: Duration = Duration::from_secs(10);

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
            main: None,
            pid: 0,
            window: None,
            status: Status::WaitingForWindow,
            started_at: Instant::now(),
            restart_at: None,
            backoff: Duration::ZERO,
            shown_secs: None,
        };
        child.backoff = child.base_delay();
        child.start();
        child
    }

    fn base_delay(&self) -> Duration {
        self.cfg.restart_delay_ms.map_or(DEFAULT_RESTART_DELAY, Duration::from_millis)
    }

    /// Launch (or relaunch) the app into a fresh job.
    fn start(&mut self) {
        // Dropping the old job kills anything left in it (KILL_ON_JOB_CLOSE).
        self.job = None;
        self.main = None;
        self.pid = 0;
        self.window = None;
        self.restart_at = None;
        self.started_at = Instant::now();
        self.status = Status::WaitingForWindow;
        let cfg = &self.cfg;
        match unsafe { launch(&cfg.command, cfg.cwd.as_deref(), self.console, &cfg.env) } {
            Ok(l) => {
                self.job = Some(l.job);
                self.main = Some(l.main);
                self.pid = l.pid;
            }
            Err(e) => {
                self.status = Status::Failed(e.message());
                self.schedule_restart();
            }
        }
    }

    /// Restart now at the user's request, resetting the backoff.
    pub fn restart(&mut self) {
        self.backoff = self.base_delay();
        self.start();
    }

    /// True if the tile is showing an app that is no longer running.
    pub fn can_restart(&self) -> bool {
        matches!(self.status, Status::Exited(_) | Status::Failed(_))
    }

    /// Schedule an automatic restart if the app's restart policy calls for one.
    fn schedule_restart(&mut self) {
        let wanted = match (self.cfg.restart, &self.status) {
            (Restart::Always, Status::Exited(_) | Status::Failed(_)) => true,
            (Restart::OnFailure, Status::Exited(code)) => *code != 0,
            (Restart::OnFailure, Status::Failed(_)) => true,
            _ => false,
        };
        if !wanted {
            return;
        }
        if self.started_at.elapsed() >= STABLE_RUN {
            self.backoff = self.base_delay();
        }
        self.restart_at = Some(Instant::now() + self.backoff);
        self.backoff = (self.backoff * 2).min(MAX_RESTART_DELAY.max(self.base_delay()));
    }

    /// Whole seconds until the scheduled restart, rounded up.
    pub fn restart_in_secs(&self) -> Option<u64> {
        self.restart_at
            .map(|t| t.saturating_duration_since(Instant::now()).as_millis().div_ceil(1000) as u64)
    }

    pub fn is_active(&self) -> bool {
        matches!(self.status, Status::WaitingForWindow | Status::Running)
    }

    /// Track exit and window changes. Returns (repaint needed, relayout needed).
    pub fn poll(&mut self, host: HWND, shutting_down: bool) -> (bool, bool) {
        if let Some(at) = self.restart_at {
            if shutting_down {
                self.restart_at = None;
                return (true, false);
            }
            if Instant::now() >= at {
                self.start();
                return (true, false);
            }
            // Repaint only when the countdown's displayed second changes.
            let secs = self.restart_in_secs();
            let changed = secs != self.shown_secs;
            self.shown_secs = secs;
            return (changed, false);
        }
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
        let main_code = self.main_exit_code();
        if main_code.is_some() || job_active_count(job.0) == 0 {
            if let Some(code) = main_code {
                // Kill whatever the main process left behind (build servers, etc.).
                let _ = unsafe { TerminateJobObject(job.0, code) };
            }
            self.status = Status::Exited(self.exit_code());
            self.window = None;
            if !shutting_down {
                self.schedule_restart();
            }
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
        self.main.as_ref().and_then(|m| m.exit_code()).unwrap_or(1)
    }

    /// With `exit_with = "main"`: the main process's exit code once it has exited.
    fn main_exit_code(&self) -> Option<u32> {
        if self.cfg.exit_with != ExitWith::Main {
            return None;
        }
        self.main.as_ref()?.exit_code()
    }

    /// Build the shutdown plan for a background thread, or None if nothing is running.
    pub fn stop_plan(&mut self, default_timeout_ms: u64, helper_exe: &Path) -> Option<StopPlan> {
        if !self.is_active() {
            return None;
        }
        let job = self.job.clone()?;
        self.status = Status::StopQueued;
        Some(StopPlan {
            job,
            console: self.console,
            window: self.window.as_ref().map(|w| w.hwnd.0 as isize),
            method: self.cfg.shutdown,
            timeout: Duration::from_millis(self.cfg.shutdown_timeout_ms.unwrap_or(default_timeout_ms)),
            helper_exe: helper_exe.to_path_buf(),
            main: self.main.clone().filter(|_| self.cfg.exit_with == ExitWith::Main),
        })
    }

    pub fn begin_stop(&mut self) {
        if self.status == Status::StopQueued {
            self.status = Status::Stopping;
        }
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
    /// Set with `exit_with = "main"`: the stop is done once this process exits.
    main: Option<Arc<MainProcess>>,
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
            if self.done() {
                break;
            }
            match step {
                Step::CtrlC => {
                    if !ctrlc::send_to_any(&self.helper_exe, &job_pids(self.job.0)) {
                        continue; // Not a console app after all; escalate immediately.
                    }
                }
                Step::Close => self.post_close(),
            }
            // After Ctrl+C, a batch wrapper (solr.cmd, npm.cmd...) whose program has exited
            // sits at "Terminate batch job (Y/N)?", and a plain cmd shell ignores Ctrl+C.
            // Either way, waiting longer achieves nothing, so move on to the next step.
            let shells_count_as_stopped = matches!(step, Step::CtrlC);
            if self.wait(self.timeout, shells_count_as_stopped) {
                break;
            }
        }
        // Kill anything left: everything after a timeout, or just the leftovers once the
        // main process has exited.
        if job_active_count(self.job.0) > 0 {
            let _ = unsafe { TerminateJobObject(self.job.0, 1) };
            wait_for_exit(self.job.0, Duration::from_secs(2));
        }
    }

    fn done(&self) -> bool {
        job_active_count(self.job.0) == 0 || self.main.as_ref().is_some_and(|m| m.exit_code().is_some())
    }

    /// Wait for the stop to finish. Returns true if it did; false on timeout, or early
    /// if `skip_on_shells` is set and only cmd.exe/conhost.exe are left.
    fn wait(&self, timeout: Duration, skip_on_shells: bool) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if self.done() {
                return true;
            }
            if Instant::now() >= deadline || (skip_on_shells && only_shells_left(self.job.0)) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
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

/// The process an app's exit code (and, with `exit_with = "main"`, its exit) is judged by.
pub struct MainProcess {
    /// The launched process: the app itself, or conhost.exe for console apps.
    process: Handle,
    /// Console apps: the app running inside conhost, once the job reports it. conhost
    /// always exits with 0, so the app's exit code is read from here.
    console_app: Mutex<Option<Handle>>,
}

impl MainProcess {
    /// The exit code, or None while the process is still running.
    fn exit_code(&self) -> Option<u32> {
        const STILL_ACTIVE: u32 = 259;
        let app = self.console_app.lock().unwrap();
        let p = app.as_ref().unwrap_or(&self.process);
        let mut code = 0u32;
        unsafe { GetExitCodeProcess(p.0, &mut code) }.ok()?;
        (code != STILL_ACTIVE).then_some(code)
    }
}

struct Launched {
    job: Arc<Handle>,
    main: Arc<MainProcess>,
    pid: u32,
}

/// The host's environment with `env` applied, as a CreateProcessW Unicode block.
fn environment_block(env: &BTreeMap<String, String>) -> Vec<u16> {
    let mut vars: Vec<(String, String)> = std::env::vars_os()
        .map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned()))
        .filter(|(k, _)| !env.keys().any(|e| e.eq_ignore_ascii_case(k)))
        .collect();
    vars.extend(env.iter().map(|(k, v)| (k.clone(), v.clone())));
    vars.sort_by_key(|(k, _)| k.to_uppercase());
    let mut block: Vec<u16> = Vec::new();
    for (k, v) in vars {
        block.extend(format!("{k}={v}").encode_utf16());
        block.push(0);
    }
    block.push(0);
    block
}

unsafe fn launch(
    command: &str,
    cwd: Option<&str>,
    console: bool,
    env: &BTreeMap<String, String>,
) -> Result<Launched> {
    unsafe {
        let job = Arc::new(Handle(CreateJobObjectW(None, PCWSTR::null())?));
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
        let env_block = (!env.is_empty()).then(|| environment_block(env));
        let mut flags = CREATE_SUSPENDED;
        if env_block.is_some() {
            flags |= CREATE_UNICODE_ENVIRONMENT;
        }
        CreateProcessW(
            PCWSTR::null(),
            Some(PWSTR(cmd_w.as_mut_ptr())),
            None,
            None,
            false,
            flags,
            env_block.as_ref().map(|b| b.as_ptr() as *const c_void),
            cwd_p,
            &si,
            &mut pi,
        )?;
        let main = Arc::new(MainProcess { process: Handle(pi.hProcess), console_app: Mutex::new(None) });
        let thread = Handle(pi.hThread);
        if console {
            // Must be associated before the process joins the job, so the app's
            // NEW_PROCESS message isn't missed.
            watch_console_app(&job, pi.dwProcessId, &main)?;
        }
        // Assign before the first instruction runs so every grandchild lands in the job.
        if let Err(e) = AssignProcessToJobObject(job.0, main.process.0) {
            let _ = TerminateProcess(main.process.0, 1);
            return Err(e);
        }
        ResumeThread(thread.0);
        Ok(Launched { job, main, pid: pi.dwProcessId })
    }
}

const JOB_MSG_ACTIVE_PROCESS_ZERO: u32 = 4;
const JOB_MSG_NEW_PROCESS: u32 = 6;

/// Watch the job's completion port and open a handle to the first process that joins
/// after the conhost launcher, which is the console app itself. Opening it as soon as
/// it's announced keeps its exit code readable even if it exits before the next poll.
unsafe fn watch_console_app(
    job: &Arc<Handle>,
    launcher_pid: u32,
    main: &Arc<MainProcess>,
) -> Result<()> {
    unsafe {
        let port = Handle(CreateIoCompletionPort(INVALID_HANDLE_VALUE, None, 0, 1)?);
        let assoc = JOBOBJECT_ASSOCIATE_COMPLETION_PORT {
            CompletionKey: std::ptr::null_mut(),
            CompletionPort: port.0,
        };
        SetInformationJobObject(
            job.0,
            JobObjectAssociateCompletionPortInformation,
            &assoc as *const _ as *const c_void,
            size_of::<JOBOBJECT_ASSOCIATE_COMPLETION_PORT>() as u32,
        )?;
        let job: Weak<Handle> = Arc::downgrade(job);
        let main = main.clone();
        std::thread::spawn(move || {
            let port = port; // Move the whole Handle (Send), not just its raw HANDLE field.
            loop {
                let (mut msg, mut key) = (0u32, 0usize);
                let mut ov: *mut OVERLAPPED = std::ptr::null_mut();
                if GetQueuedCompletionStatus(port.0, &mut msg, &mut key, &mut ov, 500).is_err() {
                    // Timed out: stop once the child has dropped (or replaced) the job.
                    if job.strong_count() == 0 {
                        return;
                    }
                    continue;
                }
                match msg {
                    JOB_MSG_NEW_PROCESS => {
                        let pid = ov as usize as u32;
                        let mut app = main.console_app.lock().unwrap();
                        if pid != launcher_pid
                            && app.is_none()
                            && let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
                        {
                            *app = Some(Handle(h));
                        }
                    }
                    JOB_MSG_ACTIVE_PROCESS_ZERO => return,
                    _ => {}
                }
            }
        });
        Ok(())
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

/// True if every process still in the job is cmd.exe or conhost.exe.
fn only_shells_left(job: HANDLE) -> bool {
    let pids = job_pids(job);
    !pids.is_empty()
        && pids.iter().all(|&pid| {
            process_name(pid).is_some_and(|n| n.eq_ignore_ascii_case("cmd.exe") || n.eq_ignore_ascii_case("conhost.exe"))
        })
}

/// The file name of a process's executable, e.g. `cmd.exe`.
fn process_name(pid: u32) -> Option<String> {
    unsafe {
        let h = Handle(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?);
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        QueryFullProcessImageNameW(h.0, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).ok()?;
        let path = PathBuf::from(String::from_utf16_lossy(&buf[..len as usize]));
        Some(path.file_name()?.to_string_lossy().into_owned())
    }
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
        // WS_EX_NOPARENTNOTIFY would hide the clicks the host uses to hand over focus.
        let ex_strip = WS_EX_APPWINDOW | WS_EX_DLGMODALFRAME | WS_EX_WINDOWEDGE | WS_EX_NOPARENTNOTIFY;
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
