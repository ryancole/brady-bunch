# Troubleshooting

Run `brady-ctl check` first. It catches most config problems, including exes and folders that can't be found, with line numbers.

## Startup

**A message box says the config is invalid.** Fix the line it names, or run `brady-ctl check` for the full list. To get errors on stderr instead of a dialog (for scripts), use `brady-ctl up` or pass `--no-dialog`.

**`failed: <name> not found`.** The exe isn't in the app's `cwd` or on PATH as the app sees it. Use a full path, or add its folder with `path_prepend`. For `npm`, `yarn` and similar, use `cmd /c npm ...`.

**An app is stuck on `waiting for <app>…`.** The dependency isn't ready. Check its tile: if it says `not ready`, its `ready` check isn't passing. Is the port or URL right? Does a `log` regex match what it actually prints? `brady-ctl status` shows the `reason`.

**`waiting for build (exited with code 1)`.** A task failed. Read its tile, fix the problem, and press Enter in its console or run `brady-ctl run build`.

**An app shows `already running` but it isn't.** Its `running_if` or `skip_if_ready` check found something else on that port or with that process name. Use a more specific check, such as a process path instead of a name. Click the tile to check again.

**Launching the config just brings an existing window to the front.** There's one host per config. Use `--new-instance` for a second one.

## While running

**A console app froze.** You probably started a text selection in it. Press Esc. QuickEdit is off by default to prevent this; check you haven't set `quick_edit = true`.

**An app exited but its tile still says running** (or `restart` never fires). It left child processes behind, typically `dotnet` build servers. Set `exit_with = "main"`.

**No colours, or output arrives in bursts, after adding `log`.** The app's output now goes through a pipe. That's expected for some programs; drop `log` and use `brady-ctl tail`, or for Python add `-u`.

**Garbled characters in a `log` file.** The program writes in a legacy code page. Set `encoding = "oem"` (or `"ansi"`).

**A rebuild fails with MSB3027/MSB3021.** Running apps lock the output. Add them to the task's `stops` and use `brady-ctl run build`.

**A window opened outside the host.** Store/UWP apps (new Notepad, Calculator, Windows Terminal) and elevated apps (when the host isn't elevated) can't be embedded. They still run and are still shut down.

## Shutdown

**Shutdown takes ages.** Check `brady-ctl events`: lines like `stopped by kill after 15000 ms` show which apps ignore Ctrl+C. Set `exit_with = "main"` for `dotnet` apps, lower their `shutdown_timeout_ms`, or use `shutdown = "kill"`. Closing the window again kills everything left.

**An app gets killed before it finishes shutting down.** Raise its `shutdown_timeout_ms`.

**Apps weren't shut down gracefully.** The host was force-killed (`taskkill /f`, `Stop-Process`). Use `brady-ctl down`, or `taskkill` without `/f`.

## Getting more information

- `brady-ctl status` for every app's state and reason.
- `brady-ctl tail <app>` for an app's console output.
- `event_log = 'brady-events.log'` in the config, then `brady-ctl events`, for a timeline of everything the host did. The previous run's log is kept as `brady-events.1.log`.

Still stuck? [Open an issue](https://github.com/ryancole/brady-bunch/issues) with your config and the `event_log`.
