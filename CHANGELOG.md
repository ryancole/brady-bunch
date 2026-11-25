# Changelog

All notable changes to brady-bunch. Each release's section is used as its GitHub release notes.

## [Unreleased]

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
