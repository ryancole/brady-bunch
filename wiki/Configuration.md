# Configuration

A config is a TOML file, `brady.toml` by default. It has some top-level settings, optional `[profiles]` and `[defaults]` tables, any number of `[[task]]` entries, and at least one `[[app]]`.

Start from [brady.example.toml](https://github.com/ryancole/brady-bunch/blob/master/brady.example.toml), which lists every key with a comment. Run `brady-ctl check` after editing: unknown keys and mistakes are errors, reported with line numbers.

## Writing values

- **Paths and commands:** use single-quoted (literal) strings, so backslashes need no escaping: `command = '"C:\Program Files\Tool\tool.exe" --flag'`. A literal string can't contain `'`; use `'''...'''` or a double-quoted string for those.
- **Exe paths with spaces** must be quoted inside `command`, as above. The command line reaches the program exactly as written.
- **Relative paths** (`cwd`, `log`, `event_log`, `path_prepend`...) are relative to the config file's folder.
- **`%VAR%`** is expanded in `command`, `cwd`, `log`, `url`, `path_prepend`, `path_append`, `ready` and `health`, from the app's own `env` first, then the host's environment. In `env` values it's expanded from the host's environment. Unset names are left as written.

## Top level

| Key | Default | Meaning |
|---|---|---|
| `title` | `"Brady Bunch"` | Host window title. |
| `columns` | ceil(√n) | Fixed number of grid columns. |
| `maximized` | `false` | Start the host maximized. A remembered layout wins over this. |
| `remember_layout` | `true` | Restore the window's position, size, maximized state and zoomed tile from the last run of this config. |
| `shutdown_timeout_ms` | `5000` | How long each shutdown step waits before escalating. See [Shutdown](Shutdown). |
| `event_log` | none | File the host writes its events to. See [Logs](Logs). |
| `keep_logs` | `1` | How many previous copies of `event_log` and app `log` files to keep (`name.1.log`, `name.2.log`...). |
| `flash_on_crash` | `true` | Flash the taskbar button when an app crashes. |
| `min_version` | none | Oldest brady-bunch that can run this config, e.g. `"0.8.0"`. |
| `[profiles]` | none | Named selections of apps. See [Profiles and Selections](Profiles-and-Selections). |
| `[defaults]` | none | Values every `[[app]]` gets unless it sets its own (below). |

## `[[app]]`

Only `command` is required.

### Launching

| Key | Default | Meaning |
|---|---|---|
| `command` | required | Full command line, including arguments. |
| `name` | exe name | Tile label, and the name used by `depends_on`, `--only`, `--skip` and `brady-ctl`. |
| `cwd` | host's working directory | Working directory. |
| `env` | none | Extra environment variables: `env = { ASPNETCORE_ENVIRONMENT = "Development" }`. `PATH = 'C:\tools;%PATH%'` prepends to PATH. |
| `path_prepend` | none | Folders to put in front of PATH: `['node_modules\.bin', 'C:\Program Files\Tool']`. Folders that don't exist are skipped, so you can list every place a tool might be installed. |
| `path_append` | none | The same, added to the end of PATH. |
| `console` | auto | `true` for a console app, `false` for a GUI app. Normally detected from the exe; `.bat`/`.cmd` count as console. |

**How the exe is found:** a path is relative to `cwd`. A bare name is looked for in `cwd`, then on PATH as the app will see it (so `env.PATH`, `path_prepend` and `path_append` count). `.exe` is added if there's no extension. If it isn't found, the tile shows `failed: <name> not found...`.

### Order and readiness

See [Startup Order and Readiness](Startup-Order-and-Readiness) for details.

| Key | Default | Meaning |
|---|---|---|
| `depends_on` | none | Apps that must be ready before this one launches. |
| `ready` | none | When the app counts as ready: one of `{ tcp = 5432 }`, `{ http = "..." }`, `{ log = 'regex' }`, plus optional `timeout_ms`. |
| `health` | none | Keep checking after it's ready: `{ interval_ms = 10000, failures = 3 }`, optionally with its own `tcp`/`http`. |
| `running_if` | none | Don't launch if it's already running elsewhere: one of `{ tcp = ... }`, `{ http = ... }`, `{ process = "Name" }`. |
| `skip_if_ready` | `false` | Don't launch if the `ready` check (tcp/http) already passes. |

### Exit, restart and shutdown

| Key | Default | Meaning |
|---|---|---|
| `exit_with` | `"all"` | `"all"`: the app has exited when every process it started has. `"main"`: when the launched process exits; leftovers are killed. Use `"main"` for `dotnet run`/`dotnet watch`. |
| `restart` | `"never"` | `"on-failure"` (non-zero exit or failed launch) or `"always"` relaunches it after it exits on its own. |
| `restart_delay_ms` | `1000` | Wait before a restart. Doubles, up to 30s, while the app keeps exiting within 10s of starting. |
| `shutdown` | `"auto"` | `"auto"`, `"ctrl-c"`, `"close"` or `"kill"`. See [Shutdown](Shutdown). |
| `shutdown_timeout_ms` | top-level value | Per-app override. |

### Output and the tile

| Key | Default | Meaning |
|---|---|---|
| `log` | none | Copy console output to this file, as UTF-8 without colour codes. Console apps only. See [Logs](Logs) for the trade-off. |
| `encoding` | `"utf-8"` | `"utf-8"`, `"oem"` or `"ansi"`: what the app writes, when its output is copied (`log` or `ready = { log = ... }`). |
| `url` | none | Clickable link in the tile header. |
| `quick_edit` | `false` | Leave the console's QuickEdit mode on. It's off by default so a stray click can't freeze the app. |

## `[[task]]`

Setup steps that run before the apps. They take `command` (required), `name`, `cwd`, `env`, `path_prepend`, `path_append`, `console`, `log`, `encoding`, `quick_edit`, `shutdown`, `shutdown_timeout_ms` and `exit_with` (default `"main"` for tasks), plus:

| Key | Meaning |
|---|---|
| `stops` | Apps that `brady-ctl run <task>` stops before rerunning the task and starts again after. |
| `skip_if_running` | Apps whose `running_if`/`skip_if_ready` check is run first; if one is running elsewhere, the task is skipped. |

See [Tasks and Rebuilds](Tasks-and-Rebuilds).

## `[defaults]`

Sets keys for every `[[app]]` that doesn't set them itself (not for tasks):

```toml
[defaults]
exit_with = "main"
shutdown_timeout_ms = 30000
env = { DOTNET_ENVIRONMENT = "Development" }
path_prepend = ['C:\Program Files\nodejs']
```

Allowed keys: `cwd`, `console`, `shutdown`, `shutdown_timeout_ms`, `restart`, `restart_delay_ms`, `env`, `path_prepend`, `path_append`, `encoding`, `exit_with`, `quick_edit`. `env` tables are merged (the app's values win), and `path_prepend`/`path_append` lists are joined (the app's folders first).
