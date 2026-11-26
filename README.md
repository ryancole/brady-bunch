# brady-bunch

Launches a list of programs as child processes and tiles their windows inside one host window. Apps can wait for each other to be ready, setup tasks can run first, and closing the host shuts everything down cleanly.

```
brady-bunch.exe [config.toml] [--profile name] [--only app,app] [--skip app,app] [--new-instance] [--no-dialog]
brady-ctl check|up|down|status|wait|start|stop|restart|run|tail|events ...
```

The config defaults to `brady.toml` in the working directory, or next to the exe if there isn't one there. Relative paths in it are relative to the config file's folder. See [brady.example.toml](brady.example.toml), and [docs/AGENTS.md](docs/AGENTS.md) for the full reference.

## Install

```powershell
irm https://github.com/ryancole/brady-bunch/releases/latest/download/install.ps1 | iex
```

[install.ps1](install.ps1) downloads the latest release, checks its SHA-256, installs it into `%LOCALAPPDATA%\Programs\brady-bunch` and adds that folder to your user PATH. Run it again to update; it refuses while brady-bunch is running from that folder. It takes `-Version 0.6.0`, `-InstallDir`, `-NoPath` and `-Check` when run as a script block: `& ([scriptblock]::Create((irm <url>))) -Check`. `-Config brady.toml` installs what that config's `min_version` needs, and nothing if the installed version already meets it.

Or download the zip from [Releases](https://github.com/ryancole/brady-bunch/releases) and extract it anywhere. Either way, copy `brady.example.toml` to `brady.toml` (or write your own config). Keep the three exes together:

- `brady-bunch.exe`: the host window.
- `brady-shim.exe`: runs each console app inside its console. Without it, console apps still run, but `log`, `ready.log`, QuickEdit control, kept-open consoles and exact command-line quoting don't work.
- `brady-ctl.exe`: command-line control of a running host.

No other dependencies. The zip also includes `AGENTS.md` ([docs/AGENTS.md](docs/AGENTS.md)), a usage guide for AI agents.

To publish a release, bump `version` in `Cargo.toml`, rename the `[Unreleased]` section of [CHANGELOG.md](CHANGELOG.md) to that version, commit, then push a matching tag. The release fails if the changelog has no section for the version, and that section becomes the release notes.

```
git tag v0.1.0
git push origin v0.1.0
```

## Behaviour

- **Launch:** each app runs in its own job object set to kill on close. If the host crashes or is force-killed, Windows kills every child and grandchild.
- **Console apps:** detected from the exe's PE header, or set with `console = true`. They run under an explicit `conhost.exe` so they get an embeddable console window even when Windows Terminal is the default terminal. Inside it, `brady-shim.exe` starts the command exactly as written and turns off QuickEdit, so a click on the tile can't freeze the app.
- **Exited console apps** keep their console open, showing the last output and exit code. Press Enter in it to run the app again.
- **Startup order:** `depends_on = ["db"]` holds an app until `db` is ready. `ready = { tcp = 10000 }`, `{ http = "https://localhost:8002" }` or `{ log = 'regex' }` says when an app counts as ready; without it, an app is ready as soon as it's launched. Waiting tiles show `waiting for db…`.
- **Tasks:** `[[task]]` entries run one at a time, each to completion, before any app starts. Their progress is shown in a tile that disappears once they've all succeeded. A failed task stops the startup and stays on screen.
- **Already running:** with `skip_if_ready = true`, an app whose `ready` check already passes isn't launched and shows `already running`.
- **Environment:** `%VAR%` in `command`, `cwd`, `env`, `log`, `url`, `ready` and `health` is expanded from the app's `env` and the host's environment. `path_prepend = ['C:\dir1', 'C:\dir2']` and `path_append` add folders to the app's PATH, skipping ones that don't exist.
- **Shared settings:** a `[defaults]` table sets keys for every `[[app]]`. `--only`/`--skip` pick apps from the command line; `--only` brings in what they depend on. A `[profiles]` table names selections, e.g. `webdev = { skip = ["worker"] }`, used as `--profile webdev`.
- **Version check:** `min_version = "0.7.0"` makes an older brady-bunch refuse the config with a message saying to update. `brady-ctl --version` prints the version.
- **Tiling:** the first visible top-level window of any process in the app's job is reparented into a grid cell. If that window closes, for example a splash screen, the next one is picked up. Console windows are created hidden and only shown inside their tile, so they never appear in the taskbar.
- **Keyboard:** click inside a tile to type into it; the focused tile's header turns blue.
- **One host per config:** launching a config that's already running brings its window to the front instead of starting a second host. `--new-instance` opts out.
- **Crashes:** an app that exits with a non-zero code or fails to launch gets a red tile header, and the taskbar button flashes until you look (`flash_on_crash = false` turns the flashing off).
- **Health:** `health = { interval_ms = 10000, failures = 3 }` keeps running an app's `tcp`/`http` `ready` check after startup, and shows it as `unhealthy` while it keeps failing. `health = { http = "https://localhost:8000" }` gives it a check of its own, for apps whose `ready` is a `log` regex.
- **Window layout:** the window's position, size and zoomed tile are restored on the next start of the same config (`remember_layout = false` turns this off).
- **Zoom:** double-click a tile's header to show that tile alone, filling the window. Double-click it again to return to the grid.
- **Links:** `url = "https://localhost:8002"` shows a link in the tile header that opens in the browser.
- **.NET:** set `exit_with = "main"` on `dotnet run`/`dotnet watch` apps. Otherwise the build servers they leave running keep the app counted as alive, so exits aren't detected and shutdown waits out every timeout. See the .NET section of [docs/AGENTS.md](docs/AGENTS.md).
- **Restart:** an app that exits on its own is relaunched when `restart = "on-failure"` (non-zero exit or failed launch) or `"always"` is set. The delay (`restart_delay_ms`, default 1000) doubles up to 30s while the app keeps exiting within 10s. A restart waits for the app's dependencies again.
- **Checking a config:** `brady-ctl check` validates a config without launching it, including whether each app's exe and working directory can be found, with line numbers. `--no-dialog` makes `brady-bunch.exe` report errors on stderr and in the event log instead of a message box.
- **Control:** `brady-ctl up` starts the host (or reuses a running one) and returns once everything is ready, with exit code 4 and the status JSON if something fails. `brady-ctl down` shuts it down and returns once it has exited. `status` prints every app's state as JSON; `start`, `stop`, `restart`, `wait` and `tail <app>` act on one app. `events` prints the end of the event log, also after the host has exited. `log = 'path'` copies an app's console output to a file, as UTF-8 (`encoding = "oem"` or `"ansi"` for programs that write in those code pages).
- **Rebuilds:** `brady-ctl run build` reruns a task after startup. It first stops the apps listed in the task's `stops` (or `--stop`), which would otherwise lock the files it writes, then starts them again once the task succeeds, and reports `build: done (8.4s); restarted api, worker; ready (14.8s)`.
- **Shutdown** (closing the window, or `taskkill` without `/f`): apps are stopped one at a time, each after the apps that depend on it, otherwise in reverse config order. For each app:
  - Console apps get Ctrl+C. If they're still running, their console window is closed (`CTRL_CLOSE_EVENT`). A batch wrapper left at `Terminate batch job (Y/N)?`, or a `cmd` shell, is closed right away instead of after the timeout.
  - GUI apps get `WM_CLOSE`.
  - Each step waits `shutdown_timeout_ms` before escalating. Anything left after that is terminated.
  - Closing the host a second time kills everything immediately.
  - `event_log = 'path'` records which step stopped each app and how long it took, with local times and their UTC offset. The previous run's log is kept as `name.1.log` (`keep_logs` sets how many), and so are the apps' `log` files.

## Limitations

- Store/UWP apps (the new Notepad, Calculator, Windows Terminal) don't embed.
- A GUI app's window can show on the desktop and in the taskbar for a moment before it's embedded, since brady-bunch can only pick it up once the app has shown it. Console apps don't.
- Elevated apps can't be embedded by a host that isn't elevated.
- `taskkill /f` on the host skips graceful shutdown; the job objects still kill the children.
- With `log` (or `ready.log`), the app's output goes through a pipe, so the app no longer sees a console on stdout/stderr. Some programs then drop colours and progress bars, or buffer their output.

## Testing

`cargo test` runs the unit tests and `tests/shim.rs`, which runs `brady-shim.exe` directly (including the command-line quoting regression).

`cargo build --example probe` builds a console app that logs which control event it received. Point an `[[app]]` at `target\debug\examples\probe.exe <logfile>`.
