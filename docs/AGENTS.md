# brady-bunch: guide for AI agents

`brady-bunch.exe` is a Windows GUI program. It launches a list of programs as child processes, embeds their windows in one tiled host window, and shuts them all down gracefully when the host closes. Use it to run several long-lived processes (dev servers, watchers, tails, consoles) as one unit that a human can see and that you can stop cleanly with one action.

## Quick start

1. Write a config file, e.g. `brady.toml`:

   ```toml
   title = "my stack"

   [[app]]
   name = "api"
   command = 'cargo run --bin api'
   cwd = 'C:\src\project'

   [[app]]
   name = "web"
   command = 'cmd /c npm run dev'
   cwd = 'C:\src\project\web'
   ```

2. Start it without waiting. It's a GUI program and keeps running until it is closed:

   ```powershell
   $bb = Start-Process .\brady-bunch.exe -ArgumentList '"C:\path\to\brady.toml"' -PassThru
   ```

3. Stop it gracefully and wait for everything to exit:

   ```powershell
   $bb.CloseMainWindow() | Out-Null; $bb.WaitForExit()
   ```

## Command line

```
brady-bunch.exe [config.toml]
brady-bunch.exe --send-ctrl-c <pid>
```

- **Config path:** if no path is given, it uses `brady.toml` in the working directory, otherwise `brady.toml` next to the exe. Pass an absolute path to avoid ambiguity.
- **`--send-ctrl-c <pid>`:** a standalone utility. It sends Ctrl+C to the console that `<pid>` is attached to, which hits every process on that console, then exits. Exit codes: `0` sent, `1` bad pid, `2` couldn't attach (the process has no console, or doesn't exist), `3` send failed. Because the program is GUI-subsystem, the shell doesn't wait for it; use `Start-Process -Wait -PassThru` and read `.ExitCode`.
- **No console output.** brady-bunch never prints to stdout or stderr.

## Config reference (TOML)

Top level:

| Key | Type | Default | Meaning |
|---|---|---|---|
| `title` | string | `"Brady Bunch"` | Host window title. |
| `shutdown_timeout_ms` | integer | `5000` | How long each shutdown step waits before escalating. |
| `columns` | integer | ceil(√n) | Fixed number of grid columns. |
| `maximized` | bool | `false` | Start the host maximized. |
| `[[app]]` | table array | required, at least one | One entry per program, in launch order. |

Per `[[app]]`:

| Key | Type | Default | Meaning |
|---|---|---|---|
| `command` | string | required | Full command line, including arguments. |
| `name` | string | exe name | Label shown above the tile. |
| `cwd` | string | host's cwd | Working directory for the app. |
| `console` | bool | auto-detected | Treat as a console app (`true`) or GUI app (`false`). Auto-detection reads the exe's PE header; `.bat`/`.cmd` count as console. |
| `shutdown` | `"auto"`, `"ctrl-c"`, `"close"` or `"kill"` | `"auto"` | How this app is stopped (see Shutdown). |
| `shutdown_timeout_ms` | integer | top-level value | Per-app override. |
| `restart` | `"never"`, `"on-failure"` or `"always"` | `"never"` | Relaunch the app after it exits on its own. `on-failure` covers a non-zero exit code or a failed launch. |
| `restart_delay_ms` | integer | `1000` | Wait before an automatic restart. It doubles, up to 30s, each time the app exits within 10s of starting, and resets once it runs longer. |

Config rules:

- Unknown keys are errors.
- Use TOML literal strings (single quotes) for Windows paths and commands, so backslashes need no escaping: `command = '"C:\Program Files\Tool\tool.exe" --flag'`.
- Quote an exe path containing spaces inside `command`, as shown above.
- The exe is looked up through the host's PATH and working directory, not the app's `cwd`. Use an absolute exe path if in doubt.
- Only real `.exe` files launch directly. Run `.cmd`/`.bat` wrappers (`npm`, `npx`, `yarn`, `pnpm`), shell builtins and pipelines through `cmd /c`: `command = 'cmd /c "npm run build && npm start"'`.
- After Ctrl+C, a batch wrapper may stop at `Terminate batch job (Y/N)?`. That's harmless: the console window is closed when the timeout runs out. Set a shorter `shutdown_timeout_ms` on these apps to speed this up.

## Shutdown

Shutdown starts when the host window receives a close request: the user clicking X, `Process.CloseMainWindow()`, or `taskkill /pid <host pid>` without `/f`.

Apps are stopped one at a time in reverse config order: the last `[[app]]` stops first and the first stops last. Put the most important apps, and those others depend on, first. Each app's stop has to finish before the next one starts.

For each app, steps escalate. Each step waits up to the app's timeout for every process the app started to exit:

| `shutdown` | Steps |
|---|---|
| `auto`, console app | Ctrl+C, then close the console window (`CTRL_CLOSE_EVENT`), then kill |
| `auto`, GUI app | `WM_CLOSE`, then kill |
| `ctrl-c` | Ctrl+C, then kill |
| `close` | `WM_CLOSE`, then kill |
| `kill` | kill immediately |

- **Worst-case shutdown time** is the sum over all apps of (steps × timeout + about 2s). Lower `shutdown_timeout_ms` for apps known to ignore signals, or set `shutdown = "kill"` for apps that don't need a graceful stop.
- **A second close request** during shutdown kills everything that's left immediately.
- **The host exits** once every app has stopped. Wait on the host process to know shutdown is complete.

## Process model and gotchas

- **Process tree:** each app runs in its own job object set to kill on close. Every process an app starts belongs to that app. If the host dies abruptly, Windows kills all of them.
- **Don't force-kill the host if you want a clean stop.** `Stop-Process`, `taskkill /f` and `TerminateProcess` skip graceful shutdown: the children are killed instantly with no Ctrl+C. Use `CloseMainWindow()` or `taskkill` without `/f`.
- **Console apps run under `conhost.exe`.** brady-bunch starts `conhost.exe <command>`, so a console app's parent process is `conhost.exe`, which is a child of `brady-bunch.exe`. Their output appears only in the tile; it can't be captured from brady-bunch. To keep logs, redirect inside the command: `command = 'cmd /c "server.exe > C:\logs\server.log 2>&1"'`.
- **Errors appear as a message box.** A missing or invalid config file shows a modal error dialog, and the exe exits with code 1 only after someone clicks OK. Validate the config before launching it unattended, and treat a host that doesn't show its window promptly as possibly stuck on that dialog.
- **Launch failures don't stop the host.** If an app's command can't start, its tile shows `failed: <reason>` and the other apps keep running.
- **Apps that exit on their own** leave a tile showing `exited (<code>)`. They're relaunched only if `restart` says so; otherwise clicking the tile restarts them. Apps are never restarted during shutdown.
- **Exit codes of console apps** are the app's own, not `conhost.exe`'s.
- **Windows that can't be embedded.** Store/UWP apps (the new Notepad, Calculator, Windows Terminal) and elevated apps (when the host isn't elevated) still run and are still shut down, but their windows stay outside the host.

## Tile states

Each tile's header shows the app name and one of these states:

| State | Meaning |
|---|---|
| `starting…` | Running, no window found yet. |
| `pid N` | Running and embedded. |
| `exited (N)` | Exited on its own with code N. The tile body says `Restarting in Ns…` if a restart is scheduled; otherwise `Click to restart`. |
| `failed: …` | Could not be launched. Restarts like an exited app. |
| `waiting to stop…` | Shutdown in progress; waiting for apps after it to stop. |
| `stopping…` | Currently being stopped. |
| `stopped` | Stopped during shutdown. |
