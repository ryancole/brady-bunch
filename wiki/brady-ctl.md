# brady-ctl

`brady-ctl.exe` controls brady-bunch from a shell or script. It's a console program, so shells wait for it and capture its output (`brady-bunch.exe` is a GUI program and they don't).

```
brady-ctl check [config] [--profile name] [--only app,app] [--skip app,app]
brady-ctl up [config] [--profile name] [--only app,app] [--skip app,app] [--exact] [--timeout secs]
brady-ctl down [config|pid] [--timeout secs]
brady-ctl status [config|pid]
brady-ctl wait <app> [config|pid] [--timeout secs]
brady-ctl start|stop|restart <app> [config|pid]
brady-ctl run <task> [config|pid] [--stop app,app] [--timeout secs]
brady-ctl tail <app> [config|pid] [--lines N]
brady-ctl events [config|pid] [--lines N] [--app name]
brady-ctl --version
```

It finds the running host by config path (default: `brady.toml` in the current folder, else next to the exe) or by the host's process id. Pass an absolute config path in scripts to avoid surprises. App names are case-insensitive.

## Everyday commands

| Command | What it does |
|---|---|
| `check` | Validates the config without launching anything, including whether each exe and `cwd` can be found. Prints the tasks, apps and start order, or one `file:line: app: problem` line per problem. |
| `up` | Starts the host (or uses the one already running) and waits until every task is done and every app is ready. Default timeout 300s. |
| `down` | Closes the host like clicking X, and waits until everything has exited. Prints how each app was stopped. |
| `status` | Prints the host and every app as JSON (below). |
| `tail <app>` | Prints the last 40 lines of the app's console. Works after the app exited, too. |
| `events` | Prints the end of the `event_log`, optionally for one app. Works after the host has exited. |

## Single apps

| Command | What it does |
|---|---|
| `start <app>` | Starts a stopped, exited or failed app, after its dependencies are ready. Returns at once. |
| `stop <app>` | Stops the app the same way shutdown does, and returns once it has stopped. It won't restart automatically. |
| `restart <app>` | Stops it if running, then starts it. Returns once it's launched, not once it's ready. |
| `wait <app>` | Waits until the app is ready. Use after `start` or `restart`. Fails at once if it, or something it depends on, fails. |
| `run <task>` | Reruns a task, stopping and restarting its `stops` apps around it. See [Tasks and Rebuilds](Tasks-and-Rebuilds). |

For example, after changing the API's code:

```powershell
brady-ctl restart api
brady-ctl wait api
```

## Exit codes

| Code | Meaning |
|---|---|
| `0` | Done. |
| `1` | Usage or config error. |
| `2` | No running host found. |
| `3` | The host refused: unknown app, shutting down, no console to read, a task is still running. |
| `4` | Failed or timed out (`up`, `down`, `wait`, `run`). |

When `up`, `wait` or `run` fail, stdout has the status JSON and stderr names the app and the reason, e.g. `api: exited with code 1`. `brady-ctl tail api` then shows its output.

## Using it in scripts

```powershell
$cfg = 'C:\src\app\brady.toml'
brady-ctl up $cfg --timeout 300
if ($LASTEXITCODE -ne 0) { brady-ctl tail api $cfg; exit 1 }

# ... run tests against the stack ...

brady-ctl down $cfg
```

A quick table of app states:

```powershell
brady-ctl status | ConvertFrom-Json | % apps | ft name, state, ready, reason
```

## Status JSON

```json
{
  "version": "0.8.0",
  "host_pid": 1234,
  "config": "C:\\src\\app\\brady.toml",
  "title": "my stack",
  "profiles": ["webdev"],
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
      "depends_on": ["build", "db"],
      "url": "https://localhost:8002",
      "healthy": null,
      "crashed": false,
      "launched_at": "2026-11-25T17:02:14.930Z",
      "ready_at": null,
      "uptime_ms": 450190
    }
  ]
}
```

(Abridged. The full field list is in [AGENTS.md](https://github.com/ryancole/brady-bunch/blob/master/docs/AGENTS.md#status-json).)

| `state` | Meaning |
|---|---|
| `waiting` | Waiting for dependencies or earlier tasks; `reason` says which. |
| `checking` | Checking whether it's already running elsewhere. |
| `external` | Found running elsewhere, so not launched. Counts as ready. |
| `skipped` | A task that wasn't run. Counts as done. |
| `starting` | Launched; `ready` hasn't passed yet. |
| `not-ready` | Launched, but `ready` didn't pass within `timeout_ms`. Still being checked. |
| `running` | Launched and ready. |
| `unhealthy` | Ready earlier, but `health` keeps failing. |
| `done` | A task that succeeded. |
| `exited` | Exited on its own; see `exit_code` and `restart_in_ms`. |
| `failed` | Couldn't be launched; see `reason`. |
| `stopping` / `stopped` | Being stopped / stopped; `last_stop` says how. |

Times are UTC. `launched_at` changes on every launch, so after `restart` it tells you whether you're looking at the new process.

## Ctrl+C utility

`brady-bunch.exe --send-ctrl-c <pid>` sends Ctrl+C to the console that process is attached to (and so to every process on it), then exits. Since it's a GUI program, use `Start-Process -Wait -PassThru` and read `.ExitCode`: `0` sent, `1` bad pid, `2` couldn't attach, `3` send failed.
