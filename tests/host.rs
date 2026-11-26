//! End to end: `brady-ctl` driving real hosts.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Host {
    dir: PathBuf,
    config: PathBuf,
}

impl Host {
    fn new(name: &str, config: &str) -> Host {
        let dir = std::env::temp_dir().join(format!("bb-host-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("brady.toml");
        std::fs::write(&path, config).unwrap();
        Host { dir, config: path }
    }

    fn ctl(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_brady-ctl"));
        let mut it = args.iter();
        // The verb (and a run's task) come before the config.
        let verb = *it.next().unwrap();
        cmd.arg(verb);
        if matches!(verb, "run" | "start" | "stop" | "restart") {
            cmd.arg(it.next().unwrap());
        }
        cmd.arg(&self.config).args(it).output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.ctl(args);
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(out.status.success(), "brady-ctl {args:?} failed: {text}");
        text
    }

    fn status(&self) -> Value {
        serde_json::from_str(&self.ok(&["status"])).unwrap()
    }

    fn app(&self, name: &str) -> Value {
        let s = self.status();
        s["apps"].as_array().unwrap().iter().find(|a| a["name"] == name).cloned().unwrap_or(Value::Null)
    }

    fn names(&self) -> Vec<String> {
        self.status()["apps"].as_array().unwrap().iter().map(|a| a["name"].as_str().unwrap().to_string()).collect()
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.ctl(&["down", "--timeout", "30"]);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A copy of ping.exe with a name no other process has.
fn unique_exe(dir: &Path, name: &str) -> PathBuf {
    let system = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let exe = dir.join(format!("{name}-{}.exe", std::process::id()));
    std::fs::copy(format!(r"{system}\System32\PING.EXE"), &exe).unwrap();
    exe
}

const SLEEP: &str = "ping -n 300 127.0.0.1";

#[test]
fn a_skipped_task_stays_runnable() {
    let host = Host::new(
        "held",
        &format!(
            "title = 'held task test'\n\
             [profiles]\nquick = {{ skip = ['build'] }}\n\
             [[task]]\nname = 'build'\ncommand = 'cmd /c echo built> built.txt'\ncwd = '.'\nconsole = false\n\
             [[app]]\nname = 'app'\ncommand = '{SLEEP}'\n"
        ),
    );
    let out = host.ok(&["up", "--profile", "quick", "--timeout", "60"]);
    assert!(out.contains("ready: build, app"), "{out}");
    let build = host.app("build");
    assert_eq!(build["state"], "skipped", "{build}");
    assert!(build["reason"].as_str().unwrap().contains("brady-ctl run build"), "{build}");
    assert!(!host.dir.join("built.txt").exists());

    let out = host.ok(&["run", "build", "--timeout", "60"]);
    assert!(out.starts_with("build: done"), "{out}");
    assert!(host.dir.join("built.txt").exists());
    assert_eq!(host.app("build")["state"], "done");
}

#[test]
fn running_if_finds_an_outside_process_but_not_the_hosts_own() {
    let dir = std::env::temp_dir().join(format!("bb-host-exe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let outside = unique_exe(&dir, "bb-outside");
    let own = unique_exe(&dir, "bb-own");
    let stem = |p: &Path| p.file_stem().unwrap().to_string_lossy().into_owned();
    let mut external = Command::new(&outside).args(["-n", "300", "127.0.0.1"]).spawn().unwrap();
    let host = Host::new(
        "running-if",
        &format!(
            "title = 'running_if test'\n\
             [[task]]\nname = 'install'\ncommand = 'cmd /c echo installed> installed.txt'\ncwd = '.'\nconsole = false\n\
             skip_if_running = ['worker']\n\
             [[task]]\nname = 'setup'\ncommand = 'cmd /c echo set up> setup.txt'\ncwd = '.'\nconsole = false\n\
             skip_if_running = ['second']\n\
             [[app]]\nname = 'worker'\ncommand = '\"{outside}\" -n 300 127.0.0.1'\nconsole = false\n\
             running_if = {{ process = '{outside_name}' }}\n\
             [[app]]\nname = 'first'\ncommand = '\"{own}\" -n 300 127.0.0.1'\nconsole = false\n\
             [[app]]\nname = 'second'\ncommand = '\"{own}\" -n 300 127.0.0.1'\nconsole = false\n\
             depends_on = ['first']\nrunning_if = {{ process = '{own_path}' }}\n",
            outside = outside.display(),
            outside_name = stem(&outside),
            own = own.display(),
            own_path = own.display(),
        ),
    );
    let out = host.ok(&["up", "--timeout", "60"]);
    let worker = host.app("worker");
    let second = host.app("second");
    let install = host.app("install");
    let setup = host.app("setup");
    let _ = external.kill();
    assert!(out.contains("ready:"), "{out}");
    assert_eq!(worker["state"], "external", "{worker}");
    assert!(worker["reason"].as_str().unwrap().contains("is running, so it wasn't launched"), "{worker}");
    // `first` runs the same exe in this host; that doesn't count as running elsewhere.
    assert_eq!(second["state"], "running", "{second}");
    assert_eq!(install["state"], "skipped", "{install}");
    assert!(install["reason"].as_str().unwrap().contains("worker is already running"), "{install}");
    assert!(!host.dir.join("installed.txt").exists());
    // No `second` was running when setup ran.
    assert_eq!(setup["state"], "done", "{setup}");
    drop(host);
    let _ = external.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn up_changes_a_running_hosts_selection() {
    let host = Host::new(
        "select",
        &format!(
            "title = 'selection test'\n\
             [[task]]\nname = 'build'\ncommand = 'cmd /c exit 0'\nconsole = false\n\
             [[app]]\nname = 'a'\ncommand = '{SLEEP}'\nconsole = false\n\
             [[app]]\nname = 'b'\ncommand = '{SLEEP}'\nconsole = false\n\
             [[app]]\nname = 'c'\ncommand = '{SLEEP}'\nconsole = false\n"
        ),
    );
    host.ok(&["up", "--skip", "b,c,build", "--timeout", "60"]);
    assert_eq!(host.names(), ["build", "a"]);
    let a_pid = host.app("a")["pid"].clone();

    // Adds b; a keeps running.
    let out = host.ok(&["up", "--skip", "c", "--timeout", "60"]);
    assert!(out.contains("started: b"), "{out}");
    assert!(out.contains("build was skipped when this host started; brady-ctl run build runs it"), "{out}");
    assert_eq!(host.names(), ["build", "a", "b"]);
    assert_eq!(host.app("a")["pid"], a_pid);
    assert_eq!(host.status()["skip"], serde_json::json!(["c"]));

    // Without --exact, apps the selection leaves out keep running.
    let out = host.ok(&["up", "--skip", "b", "--timeout", "60"]);
    assert!(out.contains("started: c"), "{out}");
    assert_eq!(host.names(), ["build", "a", "b", "c"]);

    // --exact stops them.
    let out = host.ok(&["up", "--skip", "b", "--exact", "--timeout", "60"]);
    assert!(out.contains("stopped: b"), "{out}");
    assert_eq!(host.names(), ["build", "a", "c"]);
    assert_eq!(host.app("a")["pid"], a_pid);
    let err = host.ctl(&["start", "b"]);
    assert!(!err.status.success());

    // And a later up brings b back.
    let out = host.ok(&["up", "--timeout", "60"]);
    assert!(out.contains("started: b"), "{out}");
    assert_eq!(host.names(), ["build", "a", "b", "c"]);
    assert_eq!(host.app("b")["state"], "running");
}
