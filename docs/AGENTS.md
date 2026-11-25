# brady-bunch: guide for AI agents

`brady-bunch.exe` is a Windows GUI program. It launches a list of programs as child processes, embeds their windows in one tiled host window, and shuts them all down gracefully when the host closes. Use it to run several long-lived processes (dev servers, watchers, tails, consoles) as one unit that a human can see and that you can stop cleanly with one action.

`brady-ctl.exe` is its command-line companion: it reports every app's state as JSON, starts, stops and restarts single apps, and prints an app's console text. Use it to see why an app failed, and to restart one after a rebuild.

## Quick start

1. Write a config file, e.g. `brady.toml`:

   ```toml
   title = "my stack"
   event_log = 'brady-events.log'

   [[task]]
   name = "build"
   command = 'dotnet build src\App.sln -nodeReuse:false'

   [[app]]
   name = "db"
   command = 'C:\tools\db\db.exe --port 5432'
   ready = { tcp = 5432 }

   [[app]]
   name = "api"
   command = 'dotnet run --no-build --project src\Api'
   depends_on = ["db"]
   ready = { http = "https://localhost:8002/health" }
   exit_with = "main"
   url = "https://localhost:8002"

   [[app]]
   name = "web"
   command = 'cmd /c npm start'
   cwd = 'web'
   ```

2. Start it without waiting. It's a GUI program and keeps running until it is closed:

   ```powershell
   $bb = Start-Process .\brady-bunch.exe -ArgumentList '"C:\path\to\brady.toml"' -PassThru
   ```

3. Check on it. Poll until the apps you need report `"ready": true`:

   ```powershell
   .\brady-ctl.exe status $bb.Id | ConvertFrom-Json | % apps | ft name, state, ready, reason
   ```

4. Stop it gracefully and wait for everything to exit:

   ```powershell
   $bb.CloseMainWindow() | Out-Null; $bb.WaitForExit()
   ```

## Command line

```
brady-bunch.exe [config.toml] [--only app,app] [--skip app,app]
brady-bunch.exe --send-ctrl-c <pid>

brady-ctl.exe status [config|pid]
brady-ctl.exe start|stop|restart <app> [config|pid]
brady-ctl.exe tail <app> [config|pid] [--lines N]
```

- **Config path:** if no path is given, it uses `brady.toml` in the working directory, otherwise `brady.toml` next to the exe. Pass an absolute path to avoid ambiguity.
- **`--only a,b`:** run only these apps, plus the apps they depend on (transitively) and every `[[task]]`.
- **`--skip a,b`:** leave these apps or tasks out. Apps that depend on a skipped app don't wait for it. Use this instead of keeping a second, near-identical config: `brady-bunch.exe everything.toml --skip worker,functions`.
- **Unknown names** in `--only`/`--skip` are config errors.
- **`--send-ctrl-c <pid>`:** a standalone utility. It sends Ctrl+C to the console that `<pid>` is attached to, which hits every process on that console, then exits. Exit codes: `0` sent, `1` bad pid, `2` couldn't attach (the process has no console, or doesn't exist), `3` send failed. Because the program is GUI-subsystem, the shell doesn't wait for it; use `Start-Process -Wait -PassThru` and read `.ExitCode`.
- **brady-bunch.exe prints nothing.** All output and errors go to its window.

### brady-ctl

`brady-ctl.exe` is a console program, so shells wait for it and capture its output. It finds the host by config path (default as above) or by the host's process id. A host listens for the config it was started with as long as it's the only host using that config; the pid always works.

| Command | Does | Prints |
|---|---|---|
| `status` | Nothing | The host and every app, as JSON (below). |
| `start <app>` | Starts a stopped, exited or failed app. It waits for its dependencies first, like at startup. Returns at once. | The app's JSON. |
| `stop <app>` | Stops the app with the same steps as shutdown, and returns once it has stopped. It isn't restarted automatically. | The app's JSON, with `last_stop`. |
| `restart <app>` | Stops the app if it's running, then starts it. Returns once it has been launched (or is waiting for dependencies), not once it's ready. | The app's JSON. |
| `tail <app>` | Nothing | The last 40 lines (`--lines N`) of the app's console, as plain text. Works for a running console app and for one that exited, since its console stays open. |

App names are matched case-insensitively. Exit codes: `0` done, `1` usage error, `2` no running host found, `3` the host refused (unknown app, shutting down, no console to read).

`brady-bunch.exe` also accepts `--status`, `--start`, `--stop`, `--restart` and `--tail` with the same arguments, but it's a GUI program: PowerShell doesn't wait for it unless its output is captured or piped. Prefer `brady-ctl.exe`.

### Status JSON

```json
{
  "host_pid": 1234,
  "config": "C:\\src\\app\\brady.toml",
  "title": "my stack",
  "shutting_down": false,
  "apps": [
    {
      "name": "api",
      "task": false,
      "state": "starting",
      "ready": false,
      "reason": "waiting for http https://localhost:8002/health",
      "pid": 4242,
      "exit_code": null,
      "restarts": 0,
      "restart_in_ms": null,
      "depends_on": ["build", "db"],
      "command": "dotnet run --no-build --project src\\Api",
      "url": "https://localhost:8002",
      "log": null,
      "last_stop": null
    }
  ]
}
```

| `state` | Meaning |
|---|---|
| `waiting` | Not launched: waiting for `depends_on` apps or earlier tasks. `reason` names the first one, and why if it's stuck: `waiting for db (exited with code 1)`. |
| `checking` | `skip_if_ready`: checking whether it's already running elsewhere. |
| `external` | `skip_if_ready`: its `ready` check already passed, so it wasn't launched. Counts as ready. |
| `starting` | Launched; its `ready` check hasn't passed yet. |
| `not-ready` | Launched, but its `ready` check didn't pass within `timeout_ms`. Still running, and still being checked. Its dependents keep waiting. |
| `running` | Launched and ready (or has no `ready` check). |
| `done` | A task that finished with exit code 0. |
| `exited` | Exited on its own; `exit_code` says how. `restart_in_ms` is set if a restart is scheduled. |
| `failed` | Couldn't be launched; `reason` says why (for example, the exe wasn't found). |
| `stopping` | Being stopped (shutdown, or `brady-ctl stop`). |
| `stopped` | Stopped. `last_stop` says how: `{"by": "ctrl-c" \| "close" \| "kill" \| "exited", "ms": 1834}`. |

`ready` is what dependents wait for. `pid` is the launched process (for console apps, `conhost.exe`) and is `null` when nothing is running. To see an app's output, use `brady-ctl tail <app>`, or `log` in the config.

## Config reference (TOML)

Top level:

| Key | Type | Default | Meaning |
|---|---|---|---|
| `title` | string | `"Brady Bunch"` | Host window title. |
| `shutdown_timeout_ms` | integer | `5000` | How long each shutdown step waits before escalating. |
| `columns` | integer | ceil(√n) | Fixed number of grid columns. |
| `maximized` | bool | `false` | Start the host maximized. |
| `event_log` | path | none | File the host writes its events to: launches, exits, readiness, restarts, and each shutdown step with timings. Replaced each time the host starts. Relative to the config file. |
| `[defaults]` | table | none | Values for every `[[app]]` that doesn't set them. Allowed keys: `cwd`, `console`, `shutdown`, `shutdown_timeout_ms`, `restart`, `restart_delay_ms`, `env`, `exit_with`, `quick_edit`. `env` tables are merged, with the app's own values winning. Doesn't apply to `[[task]]`. |
| `[[task]]` | table array | none | Setup steps (below). |
| `[[app]]` | table array | required, at least one | One entry per program. Tiles are in config order. |

Per `[[app]]`:

| Key | Type | Default | Meaning |
|---|---|---|---|
| `command` | string | required | Full command line, including arguments. |
| `name` | string | exe name | Label shown above the tile, and the name used by `depends_on`, `--only`, `--skip` and `brady-ctl`. |
| `cwd` | string | host's cwd | Working directory for the app. A relative path is relative to the config file's folder. |
| `console` | bool | auto-detected | Treat as a console app (`true`) or GUI app (`false`). Auto-detection reads the exe's PE header; `.bat`/`.cmd` count as console. |
| `env` | table of strings | none | Extra environment variables, e.g. `env = { ASPNETCORE_ENVIRONMENT = "Development" }`. Added to the host's environment, replacing variables with the same name (case-insensitive). `%VAR%` in values is expanded from the host's environment, so `PATH = 'C:\tools;%PATH%'` prepends to PATH. |
| `depends_on` | array of names | none | Apps that must be ready before this one is launched (see Startup order). |
| `ready` | table | none | When the app counts as ready: exactly one of `tcp`, `http` or `log`, plus optional `timeout_ms` (see Startup order). |
| `skip_if_ready` | bool | `false` | Before launching, run the `ready` check once (`tcp` or `http` only). If it passes, the app is already running outside brady-bunch: it isn't launched, and shows `already running`. |
| `log` | path | none | Copy the app's console output to this file, with colour codes removed, while it still shows in the tile. Emptied when the host starts; restarts append, with a `---- <time> start` line before each run and `---- <time> exited with code N` after. Relative to the config file. Console apps only. See the caveat under Logs. |
| `url` | string | none | Shown as a link in the tile header; clicking it opens the default browser. |
| `quick_edit` | bool | `false` | Leave the console's QuickEdit mode as the user's console settings have it. By default it's turned off (see Tiles). |
| `exit_with` | `"all"` or `"main"` | `"all"` | What counts as the app exiting. `all`: every process it started has exited. `main`: the launched process has exited (for console apps, the program running inside the console), and anything it left behind is killed. Use `main` for `dotnet run`/`dotnet watch` and other tools that leave background servers running. |
| `shutdown` | `"auto"`, `"ctrl-c"`, `"close"` or `"kill"` | `"auto"` | How this app is stopped (see Shutdown). |
| `shutdown_timeout_ms` | integer | top-level value | Per-app override. |
| `restart` | `"never"`, `"on-failure"` or `"always"` | `"never"` | Relaunch the app after it exits on its own. `on-failure` covers a non-zero exit code or a failed launch. |
| `restart_delay_ms` | integer | `1000` | Wait before an automatic restart. It doubles, up to 30s, each time the app exits within 10s of starting, and resets once it runs longer. |

Per `[[task]]`: `command` (required), `name`, `cwd`, `env`, `console`, `log`, `quick_edit`, `shutdown`, `shutdown_timeout_ms`, and `exit_with`, which defaults to `"main"` for tasks so build servers left behind don't hold them open.

Config rules:

- Unknown keys are errors. So are unknown names in `depends_on`, a dependency cycle, and a `ready` table without exactly one check.
- Use TOML literal strings (single quotes) for Windows paths and commands, so backslashes need no escaping: `command = '"C:\Program Files\Tool\tool.exe" --flag'`. A literal string can't contain a single quote; use a double-quoted string (escaping backslashes) or `'''...'''` for those.
- **`%VAR%` expansion:** `command`, `cwd`, `log`, `url` and `ready` values see the app's own `env` first, then the host's environment; `env` values see the host's. Names that aren't set are left as written, like cmd.exe does.
- Quote an exe path containing spaces inside `command`, as shown above. The command line reaches the program exactly as written, including nested quotes such as `cmd /c ""%TOOL%" --flag"`.
- **Finding the exe:** a path is relative to the app's `cwd`. A bare name is looked for in the app's `cwd`, then on PATH as the app will see it (so `env = { PATH = ... }` counts). `.exe` is added if the name has no extension. If it isn't found, the tile shows `failed: <name> not found...` and nothing is launched.
- Only real `.exe` files and `.bat`/`.cmd` files launch directly. Run wrappers without an extension (`npm`, `npx`, `yarn`, `pnpm`), shell builtins and pipelines through `cmd /c`: `command = 'cmd /c "npm run build && npm start"'`.
- After Ctrl+C, a batch wrapper may stop at `Terminate batch job (Y/N)?`. That's handled: once only `cmd.exe` and console plumbing are left in the app, brady-bunch closes the console right away instead of waiting out the timeout.

## Startup order

Apps without `depends_on` launch at once, in config order. An app with `depends_on` waits until every app it names is ready; its tile shows `waiting for <app>…`. Every app also waits for all `[[task]]`s to finish.

An app is ready:

- with no `ready` check: as soon as it's launched;
- with `ready = { tcp = 10000 }`: once something accepts TCP connections on localhost port 10000 (IPv4 or IPv6). `tcp = "127.0.0.1:10000"` checks a specific address;
- with `ready = { http = "https://localhost:8002/health" }`: once a GET gets any HTTP response below 500. Certificates aren't checked, redirects aren't followed, and no proxy is used;
- with `ready = { log = 'Now listening on' }`: once the regex matches console output written since this launch. This copies the app's output through a pipe, with the same caveat as `log`;
- with `skip_if_ready`, when it was found already running.

Checks run every 250ms. `timeout_ms` (default 60000) only changes how a slow app is shown: after it, the app shows `not ready` and its dependents' tiles say so, but checking continues and the dependents start as soon as it passes. Nothing is killed.

Dependents only wait at launch. If a dependency exits or restarts later, apps already running are left alone. An app that restarts (automatically, or with `brady-ctl start`/`restart`) waits for its dependencies again.

## Tasks

`[[task]]` entries are setup steps such as builds and package installs. They run one at a time in config order, each to completion, before any app is launched. The running task has a tile; once every task has succeeded, the task tile disappears and the apps start.

If a task exits with a non-zero code, everything after it waits. Its tile stays, with the error output, and the apps show `waiting for <task> (exited with code N)`. Press Enter in the task's console, or run `brady-ctl start <task>`, to run it again; when it succeeds, startup continues.

## Logs

- **`brady-ctl tail <app>`** prints what's in the app's console. Use it first; it needs no config.
- **`log = 'path'`** keeps a full copy of the app's output in a file. To do that, the app's stdout and stderr become a pipe that brady-shim copies to both the console and the file. The app then no longer sees a console on those streams: some programs drop colours and progress bars (npm, rspack), and some buffer their output (Python without `-u`, some C programs). .NET, Java and Node servers are fine. Input still comes from the console.
- **`event_log`** is the host's own record. A typical shutdown:

  ```
  2026-11-25 10:15:39.315 api: stopping
  2026-11-25 10:15:39.381 api: sent ctrl-c
  2026-11-25 10:15:41.215 api: stopped by ctrl-c after 1900 ms
  ```

  `stopped by kill` means it ignored the graceful steps.

## .NET apps

`dotnet run`, `dotnet build` and `dotnet watch` start build servers (MSBuild worker nodes, the MSBuild server and `VBCSCompiler`) that keep running after the build. They belong to the app's job, so by default brady-bunch counts them as part of the app:

- An app that exits never shows `exited`, so `restart` never fires.
- Shutdown waits out every step's timeout before killing them, even after the app itself has stopped.

Set `exit_with = "main"` on these apps (a `[defaults]` table saves repeating it). The app's exit is then judged by `dotnet` itself, and the build servers are killed with it. Each restart then builds without warm build servers, so it's a few seconds slower.

```toml
[defaults]
exit_with = "main"
shutdown_timeout_ms = 30000

[[task]]
name = "build"
command = 'dotnet build src\App.sln -nodeReuse:false'
cwd = 'C:\src\myapp'

[[app]]
name = "api"
command = 'dotnet run --no-build --project src\Api'
cwd = 'C:\src\myapp'
ready = { http = "https://localhost:8002" }
restart = "on-failure"
env = { ASPNETCORE_ENVIRONMENT = "Development" }
```

- **Build once in a task, then `dotnet run --no-build`**: two apps building the same project at once with `dotnet run` can fail on locked files. Running the built output (`command = 'src\Api\bin\Debug\net9.0\Api.exe'`) also works, and starts faster.
- **Shutdown timeout:** Generic Host apps (`IHost`, ASP.NET Core) stop gracefully on Ctrl+C, but `HostOptions.ShutdownTimeout` defaults to 30s. Set `shutdown_timeout_ms` to at least the time your app needs to stop, or it gets `CTRL_CLOSE_EVENT` part-way through.
- **After a rebuild,** `brady-ctl restart api` restarts one app without touching the rest.
- **Exit codes** come from `dotnet`, which passes through the app's code.

## Java apps (Solr, etc.)

The JVM treats Ctrl+C as a normal shutdown and runs its shutdown hooks, so Java servers such as Solr and Jetty stop gracefully. Java starts no background helpers, so the default `exit_with` works.

```toml
[[app]]
name = "solr"
command = 'C:\solr\bin\solr.cmd start -f -p 8983'
ready = { http = "http://localhost:8983/solr/" }
skip_if_ready = true
shutdown_timeout_ms = 30000
env = { JAVA_HOME = 'C:\Program Files\Microsoft\jdk-21' }
```

- **Run in the foreground.** `solr.cmd start` without `-f` starts Java in the background and returns. The tile then shows `exited` while Solr keeps running, and Solr is no longer stopped gracefully on shutdown (it's killed with the job).
- **Shutdown timeout:** give the server enough time to stop. Solr may take several seconds to close its cores. If Java is still running when the timeout expires, its console is closed, which gives the JVM about 5 more seconds before Windows kills it. `event_log` shows which step stopped it.
- **Java location:** launchers such as `solr.cmd` look for `JAVA_HOME` or `java` on PATH. Set `JAVA_HOME` per app with `env` if the machine doesn't have it.

## Shutdown

Shutdown starts when the host window receives a close request: the user clicking X, `Process.CloseMainWindow()`, or `taskkill /pid <host pid>` without `/f`.

Apps are stopped one at a time: each app after every app that depends on it, and otherwise in reverse config order, so the first apps listed stop last. Each app's stop has to finish before the next one starts. Consoles kept open after an app exited are closed at once.

For each app, steps escalate. Each step waits up to the app's timeout for the app to exit:

| `shutdown` | Steps |
|---|---|
| `auto`, console app | Ctrl+C, then close the console window (`CTRL_CLOSE_EVENT`), then kill |
| `auto`, GUI app | `WM_CLOSE`, then kill |
| `ctrl-c` | Ctrl+C, then kill |
| `close` | `WM_CLOSE`, then kill |
| `kill` | kill immediately |

- **Worst-case shutdown time** is the sum over all apps of (steps × timeout + about 2s). Lower `shutdown_timeout_ms` for apps known to ignore signals, or set `shutdown = "kill"` for apps that don't need a graceful stop.
- **A second close request** during shutdown kills everything that's left immediately.
- **The host exits** once every app has stopped. Wait on the host process to know shutdown is complete. `brady-ctl status` keeps answering until then, with `"shutting_down": true`.

## Process model and gotchas

- **Process tree:** each app runs in its own job object set to kill on close. Every process an app starts belongs to that app. If the host dies abruptly, Windows kills all of them.
- **Don't force-kill the host if you want a clean stop.** `Stop-Process`, `taskkill /f` and `TerminateProcess` skip graceful shutdown: the children are killed instantly with no Ctrl+C. Use `CloseMainWindow()` or `taskkill` without `/f`.
- **Console apps run under `conhost.exe` and `brady-shim.exe`.** brady-bunch starts `conhost.exe brady-shim.exe`, and brady-shim starts the command, so a console app's parent is `brady-shim.exe`. When the app exits, brady-shim exits with the app's exit code and leaves a `brady-shim.exe --hold` process keeping the console open. If `brady-shim.exe` isn't next to `brady-bunch.exe`, conhost runs the command directly: quoting can break, QuickEdit isn't turned off, consoles close when the app exits, and apps with `log` or `ready.log` fail to launch.
- **Errors appear as a message box.** A missing or invalid config file shows a modal error dialog, and the exe exits with code 1 only after someone clicks OK. Validate the config before launching it unattended, and treat a host that doesn't show its window promptly (or whose `brady-ctl status` finds no host) as possibly stuck on that dialog.
- **Launch failures don't stop the host.** If an app's command can't start, its tile shows `failed: <reason>` and the other apps keep running.
- **Apps that exit on their own** show `exited (<code>)`. They're relaunched only if `restart` says so. Otherwise, press Enter in the kept-open console, click the body of a tile with no window, or run `brady-ctl start <app>`. Apps are never restarted during shutdown.
- **Exit codes of console apps** are the app's own, not `conhost.exe`'s.
- **Windows that can't be embedded.** Store/UWP apps (the new Notepad, Calculator, Windows Terminal) and elevated apps (when the host isn't elevated) still run and are still shut down, but their windows stay outside the host.

## Tiles

Clicking inside a tile gives that app keyboard focus, so you can type into console prompts; its header turns blue. Clicking a header or empty space returns focus to the host. When the host window is reactivated, focus returns to the tile that last had it.

QuickEdit is off in embedded consoles, so clicking one can't start a text selection, which would block the app's next console write and freeze a chatty server. To select text, press Ctrl+M (mark mode) in the console, or set `quick_edit = true` on the app. Once an app has exited, its kept-open console has QuickEdit on.

Double-clicking a tile's header zooms that tile to fill the window, hiding the others (their apps keep running). Zooming a tile also focuses it. Double-clicking again restores the grid.

Each tile's header shows the app name, its `url` as a link, and one of these states:

| State | Meaning |
|---|---|
| `waiting for <app>…` | Not launched yet: waiting for a dependency or an earlier task. |
| `checking…` | `skip_if_ready`: checking whether it's already running. |
| `already running` | `skip_if_ready`: found running elsewhere, so not launched. Click the tile to check again. |
| `starting…` | Running; no window found yet, or its `ready` check hasn't passed. |
| `not ready (pid N)` | Running, but its `ready` check didn't pass within `timeout_ms`. |
| `pid N` | Running, ready, and embedded. |
| `done` | A task that finished successfully. |
| `exited (N)` | Exited on its own with code N; `, restarting in Ns` if a restart is scheduled. |
| `failed: …` | Could not be launched. Restarts like an exited app. |
| `waiting to stop…` | Shutdown in progress; waiting for apps after it to stop. |
| `stopping…` | Currently being stopped. |
| `stopped` | Stopped during shutdown or with `brady-ctl stop`. Click the tile to start it. |
