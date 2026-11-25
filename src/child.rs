//! Child process lifecycle: launch into a job object, find and embed its window,
//! check readiness, and shut it down gracefully.

use crate::config::{AppConfig, ExitWith, Restart, Shutdown};
use crate::ctrlc;
use crate::events;
use crate::ready::{self, Check, ProbeGuard};
use std::collections::{BTreeMap, HashMap};
use std::ffi::c_void;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, INVALID_HANDLE_VALUE, LPARAM, WPARAM};
use windows::Win32::System::IO::{CreateIoCompletionPort, GetQueuedCompletionStatus, OVERLAPPED};
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
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE, QueryFullProcessImageNameW, ResumeThread,
    STARTUPINFOW, TerminateProcess,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, HSTRING, PCWSTR, PWSTR, Result};

/// File name of the console-side launcher, shipped next to brady-bunch.exe.
pub const SHIM_EXE: &str = "brady-shim.exe";

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
    /// Not launched: waiting for the apps it depends on.
    Waiting,
    /// `skip_if_ready`: checking whether it's already running elsewhere.
    Checking,
    /// `skip_if_ready`: it was already running, so it wasn't launched.
    External,
    WaitingForWindow,
    Running,
    Exited(u32),
    Failed(String),
    /// Stopped on request, or during shutdown.
    Stopped,
    /// Waiting for apps that depend on it to stop first.
    StopQueued,
    Stopping,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Readiness {
    /// No `ready` check: ready once launched.
    NoCheck,
    Pending,
    Passed,
    TimedOut,
}

/// How a stop ended: the step that did it, and how long the whole stop took.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StopOutcome {
    pub by: StopBy,
    pub ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StopBy {
    /// It had already exited.
    Exited,
    CtrlC,
    Close,
    Kill,
}

impl StopBy {
    pub fn as_str(self) -> &'static str {
        match self {
            StopBy::Exited => "exited",
            StopBy::CtrlC => "ctrl-c",
            StopBy::Close => "close",
            StopBy::Kill => "kill",
        }
    }

    pub fn from_index(i: usize) -> StopBy {
        [StopBy::Exited, StopBy::CtrlC, StopBy::Close, StopBy::Kill].get(i).copied().unwrap_or(StopBy::Kill)
    }

    pub fn index(self) -> usize {
        self as usize
    }
}

/// A foreign window reparented into the host, with the styles needed to restore it.
pub struct Embedded {
    pub hwnd: HWND,
    style: u32,
    ex_style: u32,
}

/// Paths shared by every child.
pub struct Context {
    /// brady-shim.exe, if it's next to the host.
    pub shim: Option<PathBuf>,
}

pub struct Child {
    pub name: String,
    pub cfg: AppConfig,
    pub console: bool,
    shim: Option<PathBuf>,
    /// One job per launch: tracks every process the app spawns (including launcher
    /// hand-offs) and kills them all if the host dies without a clean shutdown.
    job: Option<Arc<Job>>,
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
    /// Launches after the first.
    pub restarts: u32,
    launched_once: bool,
    pub readiness: Readiness,
    ready_deadline: Instant,
    probe: Option<ProbeGuard>,
    /// `skip_if_ready`'s one-off check.
    precheck: Option<ProbeGuard>,
    /// Where the shim copies the app's output: `log`, or a temp file for `ready.log`.
    pub capture: Option<PathBuf>,
    capture_is_temp: bool,
    /// The console was kept open after the app exited; its holder process.
    keeper: Option<Handle>,
    pub last_stop: Option<StopOutcome>,
}

const DEFAULT_RESTART_DELAY: Duration = Duration::from_millis(1000);
const MAX_RESTART_DELAY: Duration = Duration::from_secs(30);
/// An app that ran at least this long before exiting resets the restart backoff.
const STABLE_RUN: Duration = Duration::from_secs(10);

impl Child {
    /// A child that hasn't been launched yet (status `Waiting`).
    pub fn new(index: usize, cfg: &AppConfig, ctx: &Context) -> Child {
        let exe = crate::config::first_token(&cfg.command);
        let console = cfg.console.unwrap_or_else(|| {
            resolve_exe(&exe, cfg.cwd.as_deref(), &effective_path(&cfg.env)).is_some_and(|p| is_console_exe(&p))
        });
        let wants_log_ready = cfg.ready.as_ref().is_some_and(|r| r.log.is_some());
        let (capture, capture_is_temp) = match (&cfg.log, wants_log_ready) {
            (Some(p), _) => (Some(PathBuf::from(p)), false),
            (None, true) => (
                Some(std::env::temp_dir().join(format!("brady-bunch-{}-{index}.log", std::process::id()))),
                true,
            ),
            (None, false) => (None, false),
        };
        // Each host run starts the copy afresh; restarts append to it.
        if let Some(p) = &capture {
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::File::create(p);
        }
        let mut child = Child {
            name: cfg.name().to_string(),
            cfg: cfg.clone(),
            console,
            shim: ctx.shim.clone(),
            job: None,
            main: None,
            pid: 0,
            window: None,
            status: Status::Waiting,
            started_at: Instant::now(),
            restart_at: None,
            backoff: Duration::ZERO,
            shown_secs: None,
            restarts: 0,
            launched_once: false,
            readiness: Readiness::NoCheck,
            ready_deadline: Instant::now(),
            probe: None,
            precheck: None,
            capture,
            capture_is_temp,
            keeper: None,
            last_stop: None,
        };
        child.backoff = child.base_delay();
        child
    }

    fn base_delay(&self) -> Duration {
        self.cfg.restart_delay_ms.map_or(DEFAULT_RESTART_DELAY, Duration::from_millis)
    }

    /// Launch, or with `skip_if_ready`, first check whether it's already running.
    /// Called once the apps it depends on are ready.
    pub fn launch(&mut self) {
        if self.cfg.skip_if_ready
            && let Some(check) = self.cfg.ready.as_ref().and_then(|r| Check::from_config(r, None))
        {
            self.status = Status::Checking;
            self.precheck = Some(ready::spawn(check, true));
            return;
        }
        self.start();
    }

    /// Launch (or relaunch) the app into a fresh job.
    fn start(&mut self) {
        // Dropping the old job kills anything left in it (KILL_ON_JOB_CLOSE), including
        // a console kept open after the app exited.
        self.job = None;
        self.main = None;
        self.keeper = None;
        self.pid = 0;
        self.window = None;
        self.restart_at = None;
        self.probe = None;
        self.precheck = None;
        self.started_at = Instant::now();
        self.status = Status::WaitingForWindow;
        if self.launched_once {
            self.restarts += 1;
        }
        self.launched_once = true;
        let log_offset = self.capture.as_ref().map(|p| std::fs::metadata(p).map_or(0, |m| m.len()));
        match self.try_launch() {
            Ok(l) => {
                events::log(format!("{}: launched (pid {})", self.name, l.pid));
                self.job = Some(l.job);
                self.main = Some(l.main);
                self.pid = l.pid;
                self.readiness = Readiness::NoCheck;
                if let Some(r) = &self.cfg.ready {
                    let log = self.capture.clone().zip(log_offset);
                    if let Some(check) = Check::from_config(r, log) {
                        self.probe = Some(ready::spawn(check, false));
                        self.readiness = Readiness::Pending;
                        self.ready_deadline = Instant::now() + Duration::from_millis(self.cfg.ready_timeout_ms());
                    }
                }
            }
            Err(e) => {
                events::log(format!("{}: failed to launch: {e}", self.name));
                self.status = Status::Failed(e);
                self.schedule_restart();
            }
        }
    }

    fn try_launch(&self) -> std::result::Result<Launched, String> {
        let cfg = &self.cfg;
        let token = crate::config::first_token(&cfg.command);
        let exe = resolve_exe(&token, cfg.cwd.as_deref(), &effective_path(&cfg.env)).ok_or_else(|| {
            format!("{token} not found in the app's folder or on PATH (run .cmd files and shell commands through cmd /c)")
        })?;
        if let Some(cwd) = &cfg.cwd
            && !Path::new(cwd).is_dir()
        {
            return Err(format!("working directory {cwd} doesn't exist"));
        }
        // Only hand CreateProcess an application name for real executables; batch
        // files are run by the command interpreter, which it starts itself.
        let app = exe.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe") || e.eq_ignore_ascii_case("com"));
        let app = app.then_some(exe.as_path());
        let shim = self.shim.as_deref().filter(|_| self.console);
        if !self.console && self.capture.is_some() {
            return Err("log and ready.log only work for console apps".into());
        }
        if self.console && shim.is_none() && self.capture.is_some() {
            return Err(format!("{SHIM_EXE} is missing next to brady-bunch.exe; it's needed for log and ready.log"));
        }
        let launch = LaunchSpec {
            command: &cfg.command,
            app,
            cwd: cfg.cwd.as_deref(),
            console: self.console,
            env: &cfg.env,
            shim,
            capture: self.capture.as_deref(),
            quick_edit: cfg.quick_edit,
        };
        unsafe { launch.run() }.map_err(|e| e.message())
    }

    /// Restart now at the user's request: back to waiting for dependencies, resetting the backoff.
    pub fn request_start(&mut self) {
        self.backoff = self.base_delay();
        self.restart_at = None;
        self.back_to_waiting();
    }

    /// Close whatever is left of the last run and wait for dependencies again.
    fn back_to_waiting(&mut self) {
        self.precheck = None;
        if self.is_active() {
            return;
        }
        self.kill_job();
        self.status = Status::Waiting;
    }

    /// True if the tile is showing an app that is no longer running and can be started.
    pub fn can_restart(&self) -> bool {
        matches!(self.status, Status::Exited(_) | Status::Failed(_) | Status::Stopped | Status::External)
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
        events::log(format!("{}: restarting in {} ms", self.name, self.backoff.as_millis()));
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

    /// Ready for the apps that depend on it.
    pub fn is_ready(&self) -> bool {
        match &self.status {
            Status::External => true,
            Status::WaitingForWindow | Status::Running => {
                !self.cfg.task && matches!(self.readiness, Readiness::NoCheck | Readiness::Passed)
            }
            Status::Exited(0) => self.cfg.task,
            _ => false,
        }
    }

    /// A task that has finished successfully.
    pub fn is_done(&self) -> bool {
        self.cfg.task && self.status == Status::Exited(0)
    }

    /// Track exit, readiness and window changes. Returns (repaint needed, relayout needed).
    pub fn poll(&mut self, host: HWND, shutting_down: bool) -> (bool, bool) {
        if self.status == Status::Checking {
            let Some(p) = self.precheck.as_ref().map(|g| g.0.clone()) else { return (false, false) };
            if !p.finished() {
                return (false, false);
            }
            self.precheck = None;
            if p.passed() {
                let what = self.cfg.ready.as_ref().map(Check::describe).unwrap_or_default();
                events::log(format!("{}: already running ({what} answered); not launched", self.name));
                self.status = Status::External;
            } else if !shutting_down {
                self.start();
            } else {
                self.status = Status::Stopped;
            }
            return (true, false);
        }
        if let Some(at) = self.restart_at {
            if shutting_down {
                self.restart_at = None;
                return (true, false);
            }
            if Instant::now() >= at {
                self.restart_at = None;
                self.back_to_waiting();
                return (true, true);
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

        if let Some(w) = &self.window
            && !unsafe { IsWindow(Some(w.hwnd)) }.as_bool()
        {
            self.window = None;
            changed = true;
            relayout = true;
        }
        if !self.is_active() {
            // A kept-open console: Enter in it exits the holder with 0, which means run again.
            if matches!(self.status, Status::Exited(_) | Status::Failed(_)) && !shutting_down {
                // An app that exits quickly can leave before its console is embedded.
                if self.window.is_none()
                    && let Some(hwnd) = top_level_windows(&job_pids(job.handle.0)).into_iter().next()
                {
                    self.window = Some(unsafe { embed(hwnd, host) });
                    changed = true;
                    relayout = true;
                }
                if self.keeper.is_none() {
                    self.keeper = job.find_keeper(self.main.as_ref().map(|m| m.console_pid()).unwrap_or(0));
                }
                if self.keeper.as_ref().and_then(|k| exit_code(k.0)) == Some(0) {
                    events::log(format!("{}: run again from its console", self.name));
                    self.request_start();
                    return (true, true);
                }
            }
            return (changed, relayout);
        }
        if self.readiness == Readiness::Pending {
            if self.probe.as_ref().is_some_and(|p| p.0.passed()) {
                events::log(format!("{}: ready after {} ms", self.name, self.started_at.elapsed().as_millis()));
                self.readiness = Readiness::Passed;
                changed = true;
            } else if Instant::now() >= self.ready_deadline {
                events::log(format!("{}: not ready after {} ms", self.name, self.cfg.ready_timeout_ms()));
                self.readiness = Readiness::TimedOut;
                changed = true;
            }
        } else if self.readiness == Readiness::TimedOut && self.probe.as_ref().is_some_and(|p| p.0.passed()) {
            events::log(format!("{}: ready after {} ms", self.name, self.started_at.elapsed().as_millis()));
            self.readiness = Readiness::Passed;
            changed = true;
        }
        if let Some(code) = self.exited(&job) {
            self.status = Status::Exited(code);
            self.probe = None;
            events::log(format!("{}: exited with code {code}", self.name));
            if self.is_done() || !self.console {
                // A finished task's console has done its job; close it.
                self.kill_job();
                self.window = None;
            }
            if self.window.is_none() {
                relayout = true;
            }
            if !shutting_down {
                self.schedule_restart();
            }
            return (true, relayout);
        }
        if self.window.is_none()
            && !shutting_down
            && let Some(hwnd) = top_level_windows(&job_pids(job.handle.0)).into_iter().next()
        {
            self.window = Some(unsafe { embed(hwnd, host) });
            relayout = true;
        }
        let status = if self.window.is_some() { Status::Running } else { Status::WaitingForWindow };
        if status != self.status {
            self.status = status;
            changed = true;
        }
        (changed || relayout, relayout)
    }

    /// The exit code, once the app counts as exited.
    fn exited(&self, job: &Job) -> Option<u32> {
        let main = self.main.as_ref()?;
        if job.active_count() == 0 {
            return Some(main.exit_code().unwrap_or(1));
        }
        let code = main.exit_code()?;
        let gone = self.cfg.exit_with == ExitWith::Main || (self.console && job.app_pids().is_empty());
        if !gone {
            return None;
        }
        if self.cfg.exit_with == ExitWith::Main {
            // Kill whatever the main process left behind (build servers, etc.), but
            // not the console kept open to show its output.
            job.kill_apps(code);
        }
        Some(code)
    }

    /// Build the stop plan for a background thread, or None if nothing is running.
    pub fn stop_plan(&mut self, default_timeout_ms: u64, helper_exe: &Path) -> Option<StopPlan> {
        self.restart_at = None;
        self.precheck = None;
        if !self.is_active() {
            if matches!(self.status, Status::Waiting | Status::Checking) {
                self.status = Status::Stopped;
            }
            // Close a console kept open after the app exited.
            self.kill_job();
            return None;
        }
        let job = self.job.clone()?;
        self.status = Status::StopQueued;
        Some(StopPlan {
            name: self.name.clone(),
            job,
            console: self.console,
            window: self.window.as_ref().map(|w| w.hwnd.0 as isize),
            method: self.cfg.shutdown,
            timeout: Duration::from_millis(self.cfg.shutdown_timeout_ms.unwrap_or(default_timeout_ms)),
            helper_exe: helper_exe.to_path_buf(),
            main: self.main.clone(),
            exit_with: self.cfg.exit_with,
        })
    }

    pub fn begin_stop(&mut self) {
        if self.status == Status::StopQueued {
            self.status = Status::Stopping;
        }
    }

    pub fn finish_stop(&mut self, outcome: StopOutcome) {
        self.last_stop = Some(outcome);
        self.probe = None;
        let alive = self.job.as_ref().is_some_and(|j| j.active_count() > 0);
        self.status = if alive { Status::Stopping } else { Status::Stopped };
    }

    pub fn kill(&self) {
        if let Some(job) = &self.job {
            let _ = unsafe { TerminateJobObject(job.handle.0, 1) };
        }
    }

    /// Kill everything left in the job and forget it.
    fn kill_job(&mut self) {
        self.kill();
        self.job = None;
        self.keeper = None;
        self.window = None;
    }

    /// Pids of the processes attached to the app's console, for reading its text.
    pub fn console_pids(&self) -> Vec<u32> {
        let Some(job) = &self.job else { return Vec::new() };
        if !self.console {
            return Vec::new();
        }
        job_pids(job.handle.0).into_iter().filter(|&p| p != job.launcher_pid).collect()
    }

    /// Hand a still-living window back to the desktop so it isn't destroyed with the host.
    pub fn release_window(&mut self) {
        if let Some(w) = self.window.take() {
            unsafe { detach(&w) };
        }
    }

    /// Remove the temp file used for `ready.log` without `log`.
    pub fn cleanup(&mut self) {
        if self.capture_is_temp
            && let Some(p) = &self.capture
        {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// An app's job object and what's needed to tell its processes apart.
pub struct Job {
    handle: Handle,
    /// conhost.exe for console apps, which isn't part of the app.
    launcher_pid: u32,
    console: bool,
    /// Image file names by pid, so each process is looked up once.
    names: Mutex<HashMap<u32, String>>,
}

impl Job {
    fn active_count(&self) -> u32 {
        job_active_count(self.handle.0)
    }

    fn name_of(&self, pid: u32) -> String {
        let mut names = self.names.lock().unwrap();
        names.entry(pid).or_insert_with(|| process_name(pid).unwrap_or_default()).clone()
    }

    /// Processes that are part of the app: everything except conhost and brady-shim.
    fn app_pids(&self) -> Vec<u32> {
        let pids = job_pids(self.handle.0);
        if !self.console {
            return pids;
        }
        pids.into_iter()
            .filter(|&p| p != self.launcher_pid && !self.name_of(p).eq_ignore_ascii_case(SHIM_EXE))
            .collect()
    }

    fn kill_apps(&self, code: u32) {
        if !self.console {
            let _ = unsafe { TerminateJobObject(self.handle.0, code) };
            return;
        }
        for pid in self.app_pids() {
            unsafe {
                if let Ok(h) = OpenProcess(PROCESS_TERMINATE, false, pid) {
                    let h = Handle(h);
                    let _ = TerminateProcess(h.0, code);
                }
            }
        }
    }

    /// True if a cmd.exe is left and everything else still in the job is console plumbing:
    /// a batch wrapper at `Terminate batch job (Y/N)?`, or a shell that ignores Ctrl+C.
    fn only_shells_left(&self) -> bool {
        let names: Vec<String> = job_pids(self.handle.0).into_iter().map(|p| self.name_of(p)).collect();
        names.iter().any(|n| n.eq_ignore_ascii_case("cmd.exe"))
            && names.iter().all(|n| ["cmd.exe", "conhost.exe", SHIM_EXE].iter().any(|s| n.eq_ignore_ascii_case(s)))
    }

    /// The `brady-shim.exe --hold` process keeping the console open, if there is one.
    fn find_keeper(&self, shim_pid: u32) -> Option<Handle> {
        job_pids(self.handle.0)
            .into_iter()
            .filter(|&p| p != shim_pid && self.name_of(p).eq_ignore_ascii_case(SHIM_EXE))
            .find_map(|p| unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, p).ok().map(Handle) })
    }
}

pub struct StopPlan {
    pub name: String,
    job: Arc<Job>,
    console: bool,
    window: Option<isize>,
    method: Shutdown,
    timeout: Duration,
    helper_exe: PathBuf,
    main: Option<Arc<MainProcess>>,
    exit_with: ExitWith,
}

#[derive(Clone, Copy)]
enum Step {
    CtrlC,
    Close,
}

impl StopPlan {
    /// Escalate through the configured steps, then terminate. Blocks; run off the UI thread.
    pub fn run(self) -> StopOutcome {
        let start = Instant::now();
        let steps: &[Step] = match (self.method, self.console) {
            (Shutdown::Auto, true) => &[Step::CtrlC, Step::Close],
            (Shutdown::Auto, false) | (Shutdown::Close, _) => &[Step::Close],
            (Shutdown::CtrlC, _) => &[Step::CtrlC],
            (Shutdown::Kill, _) => &[],
        };
        let mut by = StopBy::Kill;
        // Set when Ctrl+C stopped the app but left its cmd.exe behind, at a batch file's
        // `Terminate batch job (Y/N)?` prompt. Closing that prompt doesn't count as
        // the step that stopped the app.
        let mut ctrlc_left_shell = false;
        if self.done() {
            by = StopBy::Exited;
        } else {
            for step in steps {
                let name = match step {
                    Step::CtrlC => "ctrl-c",
                    Step::Close => "close",
                };
                // A plain cmd shell ignores Ctrl+C; only a program that was running counts.
                let had_program = !self.job.only_shells_left();
                match step {
                    Step::CtrlC => {
                        if !ctrlc::send_to_any(&self.helper_exe, &job_pids(self.job.handle.0)) {
                            events::log(format!("{}: no console to send ctrl-c to", self.name));
                            continue; // Not a console app after all; escalate immediately.
                        }
                    }
                    Step::Close => self.post_close(),
                }
                events::log(format!("{}: sent {name}", self.name));
                // After Ctrl+C, a batch wrapper (solr.cmd, npm.cmd...) whose program has exited
                // sits at "Terminate batch job (Y/N)?", and a plain cmd shell ignores Ctrl+C.
                // Either way, waiting longer achieves nothing, so move on to the next step.
                let shells_count_as_stopped = matches!(step, Step::CtrlC);
                if self.wait(self.timeout, shells_count_as_stopped) {
                    by = match step {
                        Step::CtrlC => StopBy::CtrlC,
                        Step::Close if ctrlc_left_shell => StopBy::CtrlC,
                        Step::Close => StopBy::Close,
                    };
                    break;
                }
                if shells_count_as_stopped && had_program && self.job.only_shells_left() {
                    ctrlc_left_shell = true;
                    events::log(format!("{}: ctrl-c stopped the app; closing the cmd.exe it left", self.name));
                } else if !self.done() {
                    events::log(format!("{}: still running after {name}", self.name));
                }
            }
        }
        if ctrlc_left_shell && by == StopBy::Kill {
            by = StopBy::CtrlC; // Only the leftover prompt needed killing.
        }
        // Kill anything left: everything after a timeout, or the leftovers (and the
        // console kept open) once the app has exited.
        if self.job.active_count() > 0 {
            if by == StopBy::Kill {
                events::log(format!("{}: killing", self.name));
            }
            let _ = unsafe { TerminateJobObject(self.job.handle.0, 1) };
            wait_for_exit(self.job.handle.0, Duration::from_secs(2));
        }
        let outcome = StopOutcome { by, ms: start.elapsed().as_millis() as u64 };
        events::log(format!("{}: stopped by {} after {} ms", self.name, by.as_str(), outcome.ms));
        outcome
    }

    /// The app has stopped: by the same rules as `Child::exited`. For console apps this
    /// includes brady-shim having exited, so it gets to record the exit.
    fn done(&self) -> bool {
        if self.job.active_count() == 0 {
            return true;
        }
        let main_exited = self.main.as_ref().is_some_and(|m| m.exit_code().is_some());
        match (self.exit_with, self.console) {
            (ExitWith::Main, _) => main_exited,
            (ExitWith::All, true) => main_exited && self.job.app_pids().is_empty(),
            (ExitWith::All, false) => false,
        }
    }

    /// Wait for the stop to finish. Returns true if it did; false on timeout, or early
    /// if `skip_on_shells` is set and only cmd.exe/conhost.exe are left.
    fn wait(&self, timeout: Duration, skip_on_shells: bool) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if self.done() {
                return true;
            }
            if Instant::now() >= deadline || (skip_on_shells && self.job.only_shells_left()) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn post_close(&self) {
        let mut targets = top_level_windows(&job_pids(self.job.handle.0));
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
    /// Console apps: the first process inside conhost (brady-shim, or the app itself),
    /// once the job reports it. conhost always exits with 0, so the exit code is read
    /// from here; brady-shim exits with the app's code.
    console_app: Mutex<Option<(u32, Handle)>>,
}

impl MainProcess {
    /// The exit code, or None while the process is still running.
    fn exit_code(&self) -> Option<u32> {
        let app = self.console_app.lock().unwrap();
        exit_code(app.as_ref().map_or(&self.process, |(_, h)| h).0)
    }

    fn console_pid(&self) -> u32 {
        self.console_app.lock().unwrap().as_ref().map_or(0, |(p, _)| *p)
    }
}

fn exit_code(process: HANDLE) -> Option<u32> {
    const STILL_ACTIVE: u32 = 259;
    let mut code = 0u32;
    unsafe { GetExitCodeProcess(process, &mut code) }.ok()?;
    (code != STILL_ACTIVE).then_some(code)
}

struct Launched {
    job: Arc<Job>,
    main: Arc<MainProcess>,
    pid: u32,
}

/// PATH as the app will see it.
fn effective_path(env: &BTreeMap<String, String>) -> String {
    env.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("PATH"))
        .map(|(_, v)| v.clone())
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default()
}

/// The host's environment with `env` applied, as a CreateProcessW Unicode block.
fn environment_block(env: &BTreeMap<String, String>, extra: &[(&str, String)]) -> Vec<u16> {
    let overridden = |k: &str| env.keys().any(|e| e.eq_ignore_ascii_case(k)) || extra.iter().any(|(e, _)| e.eq_ignore_ascii_case(k));
    let mut vars: Vec<(String, String)> = std::env::vars_os()
        .map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned()))
        .filter(|(k, _)| !overridden(k))
        .collect();
    vars.extend(env.iter().map(|(k, v)| (k.clone(), v.clone())));
    vars.extend(extra.iter().map(|(k, v)| (k.to_string(), v.clone())));
    vars.sort_by_key(|(k, _)| k.to_uppercase());
    let mut block: Vec<u16> = Vec::new();
    for (k, v) in vars {
        block.extend(format!("{k}={v}").encode_utf16());
        block.push(0);
    }
    block.push(0);
    block
}

struct LaunchSpec<'a> {
    command: &'a str,
    /// The resolved executable, for real .exe files.
    app: Option<&'a Path>,
    cwd: Option<&'a str>,
    console: bool,
    env: &'a BTreeMap<String, String>,
    /// brady-shim.exe, for console apps when it's available.
    shim: Option<&'a Path>,
    capture: Option<&'a Path>,
    quick_edit: bool,
}

impl LaunchSpec<'_> {
    /// conhost's command line, and the variables that tell brady-shim what to run.
    fn console_command(&self, shim: Option<&Path>) -> (String, Vec<(&'static str, String)>) {
        let conhost = conhost_path();
        let Some(shim) = shim else {
            // Without brady-shim, conhost runs the command itself (and may lose quotes).
            return (format!("\"{conhost}\" {}", self.command), Vec::new());
        };
        let mut vars = vec![("BRADY_SHIM_COMMAND", self.command.to_string())];
        if let Some(app) = self.app {
            vars.push(("BRADY_SHIM_APP", app.to_string_lossy().into_owned()));
        }
        if let Some(c) = self.capture {
            vars.push(("BRADY_SHIM_LOG", c.to_string_lossy().into_owned()));
        }
        if self.quick_edit {
            vars.push(("BRADY_SHIM_QUICK_EDIT", "1".into()));
        }
        (format!("\"{conhost}\" \"{}\"", shim.display()), vars)
    }

    unsafe fn run(&self) -> Result<Launched> {
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
            let (cmdline, extra, app) = if self.console {
                let (c, v) = self.console_command(self.shim);
                (c, v, None)
            } else {
                (self.command.to_string(), Vec::new(), self.app)
            };
            let mut cmd_w: Vec<u16> = cmdline.encode_utf16().chain(Some(0)).collect();
            let app_h = app.map(|a| HSTRING::from(a.as_os_str()));
            let app_p = app_h.as_ref().map_or(PCWSTR::null(), |h| PCWSTR(h.as_ptr()));
            let cwd_h = self.cwd.map(HSTRING::from);
            let cwd_p = cwd_h.as_ref().map_or(PCWSTR::null(), |h| PCWSTR(h.as_ptr()));
            let si = STARTUPINFOW { cb: size_of::<STARTUPINFOW>() as u32, ..Default::default() };
            let mut pi = PROCESS_INFORMATION::default();
            let env_block = (!self.env.is_empty() || !extra.is_empty()).then(|| environment_block(self.env, &extra));
            let mut flags = CREATE_SUSPENDED;
            if env_block.is_some() {
                flags |= CREATE_UNICODE_ENVIRONMENT;
            }
            CreateProcessW(
                app_p,
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
            let job = Arc::new(Job {
                handle: job,
                launcher_pid: if self.console { pi.dwProcessId } else { 0 },
                console: self.console,
                names: Mutex::new(HashMap::new()),
            });
            if self.console {
                // Must be associated before the process joins the job, so the app's
                // NEW_PROCESS message isn't missed.
                watch_console_app(&job, pi.dwProcessId, &main)?;
            }
            // Assign before the first instruction runs so every grandchild lands in the job.
            if let Err(e) = AssignProcessToJobObject(job.handle.0, main.process.0) {
                let _ = TerminateProcess(main.process.0, 1);
                return Err(e);
            }
            ResumeThread(thread.0);
            Ok(Launched { job, main, pid: pi.dwProcessId })
        }
    }
}

const JOB_MSG_ACTIVE_PROCESS_ZERO: u32 = 4;
const JOB_MSG_NEW_PROCESS: u32 = 6;

/// Watch the job's completion port and open a handle to the first process that joins
/// after the conhost launcher, which is brady-shim (or the console app itself). Opening
/// it as soon as it's announced keeps its exit code readable even if it exits before
/// the next poll.
unsafe fn watch_console_app(job: &Arc<Job>, launcher_pid: u32, main: &Arc<MainProcess>) -> Result<()> {
    unsafe {
        let port = Handle(CreateIoCompletionPort(INVALID_HANDLE_VALUE, None, 0, 1)?);
        let assoc = JOBOBJECT_ASSOCIATE_COMPLETION_PORT {
            CompletionKey: std::ptr::null_mut(),
            CompletionPort: port.0,
        };
        SetInformationJobObject(
            job.handle.0,
            JobObjectAssociateCompletionPortInformation,
            &assoc as *const _ as *const c_void,
            size_of::<JOBOBJECT_ASSOCIATE_COMPLETION_PORT>() as u32,
        )?;
        let job: Weak<Job> = Arc::downgrade(job);
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
                            *app = Some((pid, Handle(h)));
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

/// Find the executable the way the app's command will run it: a path is relative to
/// the app's working directory; a bare name is looked for there, then on the app's PATH.
/// `.exe` is added to names without an extension.
pub fn resolve_exe(exe: &str, cwd: Option<&str>, path: &str) -> Option<PathBuf> {
    if exe.is_empty() {
        return None;
    }
    let found = |p: PathBuf| -> Option<PathBuf> {
        if p.is_file() {
            return Some(p);
        }
        if p.extension().is_none() {
            let with_exe = p.with_extension("exe");
            if with_exe.is_file() {
                return Some(with_exe);
            }
        }
        None
    };
    let p = Path::new(exe);
    let base = cwd.map(PathBuf::from).or_else(|| std::env::current_dir().ok()).unwrap_or_default();
    if exe.contains(['\\', '/', ':']) {
        return found(if p.is_absolute() { p.to_path_buf() } else { base.join(p) });
    }
    std::iter::once(base).chain(std::env::split_paths(path)).find_map(|d| found(d.join(exe)))
}

fn is_console_exe(path: &Path) -> bool {
    let lower = path.to_string_lossy().to_ascii_lowercase();
    if lower.ends_with(".bat") || lower.ends_with(".cmd") {
        return true;
    }
    pe_subsystem(path) == Some(3) // IMAGE_SUBSYSTEM_WINDOWS_CUI
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_command_line_never_contains_the_app_command() {
        // conhost rebuilds its client's command line and loses quotes, so the command
        // must travel through the environment instead.
        let env = BTreeMap::new();
        let spec = LaunchSpec {
            command: r#"cmd /c ""C:\dir with space\azurite.exe" --skipApiVersionCheck""#,
            app: Some(Path::new(r"C:\Windows\System32\cmd.exe")),
            cwd: None,
            console: true,
            env: &env,
            shim: None,
            capture: Some(Path::new(r"C:\logs\a.log")),
            quick_edit: false,
        };
        let (cmdline, vars) = spec.console_command(Some(Path::new(r"C:\bb\brady-shim.exe")));
        assert!(cmdline.ends_with(r#"conhost.exe" "C:\bb\brady-shim.exe""#), "{cmdline}");
        assert!(vars.contains(&("BRADY_SHIM_COMMAND", spec.command.to_string())));
        assert!(vars.contains(&("BRADY_SHIM_APP", r"C:\Windows\System32\cmd.exe".to_string())));
        assert!(vars.contains(&("BRADY_SHIM_LOG", r"C:\logs\a.log".to_string())));
        assert!(!vars.iter().any(|(k, _)| *k == "BRADY_SHIM_QUICK_EDIT"));
    }

    #[test]
    fn resolve_exe_uses_cwd_then_the_apps_path() {
        let dir = std::env::temp_dir().join(format!("bb-resolve-{}", std::process::id()));
        let bin = dir.join("bin dir");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("tool.exe"), b"").unwrap();
        std::fs::write(dir.join("local.cmd"), b"").unwrap();
        let path = format!(r"C:\nowhere;{}", bin.display());
        let cwd = dir.to_str();
        assert_eq!(resolve_exe("tool", cwd, &path), Some(bin.join("tool.exe")));
        assert_eq!(resolve_exe("local.cmd", cwd, ""), Some(dir.join("local.cmd")));
        assert_eq!(resolve_exe(r"bin dir\tool.exe", cwd, ""), Some(bin.join("tool.exe")));
        assert_eq!(resolve_exe("tool", cwd, ""), None);
        assert!(resolve_exe("cmd", None, &std::env::var("PATH").unwrap()).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
