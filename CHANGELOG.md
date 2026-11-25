# Changelog

All notable changes to brady-bunch. Each release's section is used as its GitHub release notes.

## [Unreleased]

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
