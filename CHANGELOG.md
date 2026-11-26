# Changelog

All notable changes to brady-bunch. Each release's section is used as its GitHub release notes.

## [Unreleased]

## [0.7.0] - 2026-11-25

### Fixed

- Non-ASCII output of apps with `log` or `ready.log` was garbled in the console (and in `brady-ctl tail`): brady-shim passed the bytes on to a console that read them in its OEM code page, so rspack's `➜` showed as `Γ₧£`. brady-shim now decodes the output and writes it to the console as text, and sets the console to UTF-8 so .NET and cmd write UTF-8 too. The `log` file is always UTF-8, and a `ready.log` regex can match non-ASCII text, including a character split between two reads.
- `brady-ctl run <task>` printed the task's dependencies and the restarted apps as `ready: ...`, listing apps it hadn't touched. It now says what it did: `build: done (8.4s); restarted api, worker, functions; ready (14.8s)`, and on failure, what failed and which apps are left waiting.

### Added

- Per-app `encoding = "utf-8" | "oem" | "ansi"` (default `utf-8`): what an app with copied output writes. In UTF-8 mode, bytes that aren't valid UTF-8 are read in the ANSI code page.
- `health` can name its own check: `health = { http = "https://localhost:8000" }` or `{ tcp = 8000 }`, for apps whose `ready` check is a `log` regex. Without one, it reuses the `ready` check as before.
- `path_prepend = ['dir', ...]` and `path_append` (per app, per task and in `[defaults]`): folders added to the app's PATH after `env`, skipping ones that don't exist. Relative to the config file. The exe lookup uses them too.
- `brady-ctl events [config|pid] [--lines N] [--app name]` prints the end of the event log, optionally only one app's lines. Given the config, it also works after the host has exited.
- `brady-ctl status` has the `event_log` path (each app's `log` was already there, absolute).
- `install.ps1 -Config <path>` installs what the config's `min_version` needs: nothing (without going online) if the installed version meets it, else the latest release.
- When a config's `min_version` isn't met, the error includes the command that installs that version.

### Changed

- `event_log` lines, and the `----` lines in `log` files, have the local time with its UTC offset (`2026-11-25 19:02:36.893-06:00`), so they can be matched against `brady-ctl status` times, which are UTC. The `log` lines now also have milliseconds.

## [0.6.0] - 2026-11-25

### Added

- `brady-ctl up [config]` checks the config, starts `brady-bunch.exe` for it unless one is already running, and returns once every task is done and every app is ready. If a task or app fails or exits, or `--timeout` (default 300s) passes, it exits with code 4, prints the status JSON and names the app and the reason. Takes `--profile`, `--only` and `--skip`.
- `brady-ctl down [config|pid]` shuts the host down gracefully and returns once it has exited, printing how each app was stopped. Exits with code 4 if an app had to be killed.
- `brady-ctl wait <app>` returns once that app is ready, e.g. after `restart`.
- `brady-ctl run <task>` reruns a task after startup, such as a build. The apps in the task's new `stops` key, and any given with `--stop`, are stopped first (dependents first), so they don't lock the files it writes, and start again once it succeeds. If it fails, they wait for it, and start once it is rerun successfully.
- `[profiles]` in the config, e.g. `webdev = { skip = ["worker"] }`, used as `--profile webdev`. Profiles combine with each other and with `--only`/`--skip`. `brady-ctl status` shows the active ones.
- `min_version = "0.6.0"` in the config: an older brady-bunch refuses it with a message saying to update. Unknown-key errors now also say which version is running.
- `brady-ctl --version` and `brady-bunch.exe --version`, and `version` in `brady-ctl status`.
- `brady-ctl check [config]` (and `brady-bunch.exe --check`) validates a config without launching anything, including whether each app's exe and `cwd` can be found as a launch would look them up. Problems are printed one per line with the config line number; on success it prints the tasks, apps and start order. Config errors about an entry now also give its line.
- `--no-dialog`: `brady-bunch.exe` reports a fatal error on stderr and in the event log and exits with code 1, instead of showing a message box. `brady-ctl up` passes it, and prints the error if the host exits before starting.
- Launching `brady-bunch.exe` for a config that a host is already running brings that host's window to the front and exits, instead of starting a second host. `--new-instance` starts one anyway.
- `keep_logs` (default 1): when the host starts, the previous `event_log` and app `log` files are kept as `name.1.log`, `name.2.log` and so on.
- `brady-ctl status` has timestamps (UTC, ISO 8601): the host's `started_at` and `now`, and per app `launched_at`, `ready_at`, `exited_at` and `uptime_ms`.
- Per-app `health = { interval_ms = 10000, failures = 3 }` keeps running the app's `tcp`/`http` `ready` check after startup. After that many failures in a row the app shows `unhealthy` in its tile and in `status` (`"healthy": false`, with a reason) until a check passes. Changes go to the event log.
- A crashed app (non-zero exit or failed launch, not a stop) gets a red tile header until it's launched again, and the taskbar button flashes until the window is activated. `flash_on_crash = false` turns off the flashing. `status` shows `crashed`.
- The window's position, size, maximized state and zoomed tile are saved per config in `%LOCALAPPDATA%\brady-bunch\layouts` and restored on the next start, falling back to the default placement if that monitor is gone. `remember_layout = false` turns this off.
- `install.ps1`, published with each release: installs or updates brady-bunch into `%LOCALAPPDATA%\Programs\brady-bunch` after checking the release's SHA-256, and adds that folder to the user PATH. It refuses while brady-bunch is running from there.

## [0.5.1] - 2026-11-25

### Fixed

- Apps waiting on a dependency started up to 4s after it was ready. Windows takes about 2s to refuse a connection to a closed port, and `localhost` is tried on both `::1` and `127.0.0.1`, so a failed `http` check (or `skip_if_ready` check) took about 4s, and a `tcp` check about 1s. Checks against a closed local port now fail in about 200ms.
- A batch wrapper such as `solr.cmd` that stopped on Ctrl+C, leaving `cmd.exe` at `Terminate batch job (Y/N)?`, was logged as `stopped by close`. It's now `stopped by ctrl-c`, and the log says the leftover prompt was closed.

## [0.5.0] - 2026-11-25

### Added

- `depends_on = ["app", ...]` holds an app until those apps are ready. Waiting tiles show `waiting for <app>…`, and say why if the dependency is stuck.
- `ready = { tcp = port }`, `{ http = "url" }` or `{ log = 'regex' }`, with optional `timeout_ms`, says when an app counts as ready. HTTP checks accept any response below 500 and ignore certificate errors.
- `skip_if_ready = true`: an app whose `ready` check already passes isn't launched, and shows `already running`.
- `[[task]]` entries: setup steps (builds, installs) that run one at a time, each to completion, before any app starts. A failed task stops startup and stays on screen with its output.
- `[defaults]` table: per-app keys shared by every `[[app]]`.
- `--only a,b` and `--skip a,b` on the command line pick which apps to run from one config. `--only` brings in the apps they depend on.
- `%VAR%` in `command`, `cwd`, `env` values, `log`, `url` and `ready` is expanded from the app's `env` and the host's environment, e.g. `env = { PATH = 'C:\tools;%PATH%' }`.
- `brady-ctl.exe`, a console program for controlling a running host: `status` prints every app's state, readiness, exit code and the reason it's waiting or failed as JSON; `start`, `stop` and `restart` act on one app; `tail` prints an app's console text. `brady-bunch.exe` accepts the same commands as `--status` etc.
- Per-app `log = 'path'` copies the app's console output to a file while it still shows in the tile.
- Top-level `event_log = 'path'` records launches, exits, readiness, restarts, and each shutdown step with which step stopped each app and how long it took. `brady-ctl status` shows the same as `last_stop`.
- Per-app `url`: a link in the tile header that opens in the browser.
- Per-app `quick_edit = true` to keep the console's QuickEdit mode.

### Fixed

- Commands with nested quotes, such as `cmd /c ""%TOOL%" --flag"` with a path containing spaces, failed to start: `conhost.exe` re-parses its command line and dropped the inner quotes. Console apps now start through `brady-shim.exe`, which runs the command line exactly as written.
- Clicking a console tile could freeze its app: with QuickEdit on, the click started a text selection that blocked the app's next console write. QuickEdit is now off in embedded consoles.
- An app's exe is looked up on the PATH from its own `env`, and relative to its own `cwd`. A missing exe now shows `failed: <name> not found` instead of a launch error from conhost.

### Changed

- Console apps keep their console open after they exit, showing their last output and exit code. Press Enter in it to run the app again.
- Shutdown stops each app after the apps that depend on it, otherwise in reverse config order as before.
- Release zips include `brady-shim.exe` and `brady-ctl.exe`, which must stay next to `brady-bunch.exe`.

## [0.4.0] - 2026-11-24

### Added

- Per-app `env` table: extra environment variables for the app.
- Per-app `exit_with = "main"`: the app counts as exited when its launched process exits, and anything it left running is killed. Fixes undetected exits and slow shutdown for `dotnet run`, whose build servers otherwise keep the app counted as alive.
- Clicking inside a tile gives that app keyboard focus, and its header is highlighted. Focus returns to that tile when the host window is reactivated.
- .NET and Java (Solr) sections in `AGENTS.md`.

### Fixed

- Typing into embedded console apps. Clicking a tile left keyboard focus on the host window, so prompts such as `Console.ReadLine` never received input.

### Changed

- Tiles have a 10px margin from the window edges and an 8px gap between them (scaled for DPI), instead of 4px.
- A relative `cwd` is relative to the config file's folder, not the host's working directory.
- After Ctrl+C, an app whose only remaining processes are `cmd.exe` and `conhost.exe` moves straight to closing its console. Batch wrappers such as `solr.cmd`, left at `Terminate batch job (Y/N)?`, and plain `cmd` shells no longer wait out the full shutdown timeout.

## [0.3.0] - 2026-11-24

### Added

- `restart = "on-failure" | "always"` relaunches an app after it exits on its own, waiting `restart_delay_ms` (default 1000) first. The delay doubles, up to 30s, while the app keeps exiting within 10s of starting.
- Clicking the body of an exited or failed tile restarts that app.
- Double-clicking a tile's header zooms it to fill the window; double-click again to return to the grid.

### Fixed

- Console apps show their own exit code instead of `conhost.exe`'s, which was always 0.

## [0.2.1] - 2026-11-17

### Added

- `AGENTS.md` in release zips: a usage guide for AI agents covering the command line, the config reference, shutdown behaviour and common pitfalls.

## [0.2.0] - 2026-11-17

### Changed

- Shutdown stops apps one at a time in reverse config order, waiting for each to exit before the next, instead of all at once. Tiles waiting their turn show "waiting to stop…".

## [0.1.1] - 2026-11-17

### Changed

- Release zips include this changelog, and each release's notes come from its changelog section.

## [0.1.0] - 2026-11-17

### Added

- Host window that launches apps from a TOML config and tiles their windows in a grid.
- Graceful shutdown: Ctrl+C for console apps, `WM_CLOSE` for GUI apps, then a forced kill after a timeout.
- Per-app job objects, so children never outlive the host.
- GitHub release workflow that publishes a zip with a SHA256 checksum.
