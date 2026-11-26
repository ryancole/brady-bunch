# Getting Started

This walks through building a config for a small web stack: a database, an API that needs the database, and a front-end dev server.

## 1. Start with one app

Create a folder for your config (often the root of your project) and add `brady.toml`:

```toml
title = "my stack"

[[app]]
name = "web"
command = 'cmd /c npm start'
cwd = 'web'
```

Run it:

```powershell
brady-bunch.exe
```

With no path, brady-bunch uses `brady.toml` in the current folder, or next to the exe if there isn't one there. You can also pass a path: `brady-bunch.exe C:\src\app\brady.toml`. Relative paths inside the config (like `cwd = 'web'`) are relative to the config file's folder, not to where you ran the command.

A few things to know already:

- **Use single quotes** for commands and paths. TOML single-quoted strings don't treat `\` as an escape, so `'C:\tools\db.exe'` works as written.
- **`npm`, `npx`, `yarn` and `pnpm` need `cmd /c`.** Only real `.exe`, `.bat` and `.cmd` files launch directly; wrappers without an extension, shell built-ins and `&&` chains go through `cmd /c`.
- **Close the window to stop everything.** Console apps get Ctrl+C first (GUI apps get a close message), and are only killed if they don't exit in time.

## 2. Add more apps

```toml
title = "my stack"

[[app]]
name = "db"
command = 'C:\tools\db\db.exe --port 5432'

[[app]]
name = "api"
command = 'dotnet run --project src\Api'

[[app]]
name = "web"
command = 'cmd /c npm start'
cwd = 'web'
```

Each `[[app]]` gets a tile, in config order. The grid has about √n columns; set `columns = 3` at the top to fix it.

## 3. Start things in order

The API shouldn't start until the database is accepting connections. Tell brady-bunch how to recognise "ready", and what depends on what:

```toml
[[app]]
name = "db"
command = 'C:\tools\db\db.exe --port 5432'
ready = { tcp = 5432 }

[[app]]
name = "api"
command = 'dotnet run --project src\Api'
depends_on = ["db"]
ready = { http = "https://localhost:8002/health" }
url = "https://localhost:8002"
```

Now `api`'s tile shows `waiting for db…` until something listens on port 5432. `url` adds a clickable link to the tile header. See [Startup Order and Readiness](Startup-Order-and-Readiness) for all the ways to define "ready".

## 4. Build first

Add a task. Tasks run one at a time, to completion, before any app starts:

```toml
[[task]]
name = "build"
command = 'dotnet build src\App.sln -nodeReuse:false'
stops = ["api"]
```

and change the API to run the built output: `command = 'dotnet run --no-build --project src\Api'`.

If the build fails, its tile stays on screen with the errors and the apps wait. Fix the code and press Enter in the task's console to run it again. `stops` is for later: `brady-ctl run build` stops `api`, rebuilds, and starts it again. See [Tasks and Rebuilds](Tasks-and-Rebuilds).

## 5. Check the config

```powershell
brady-ctl check
```

This validates the file without starting anything: unknown keys, bad names, dependency cycles, and whether every exe and working directory can actually be found. Problems are reported with line numbers:

```
C:\src\app\brady.toml:12: api: dotnet2 not found...
```

## 6. Start and stop from the command line

```powershell
brady-ctl up --timeout 300
```

`up` starts the host and returns once every task is done and every app is ready. If something fails, it exits with code 4 and tells you which app and why. If the host is already running, it just waits on that one.

```powershell
brady-ctl down
```

`down` closes the host and returns once everything has exited. See [brady-ctl](brady-ctl) for everything else it does.

## The finished config

```toml
title = "my stack"
event_log = 'brady-events.log'

[[task]]
name = "build"
command = 'dotnet build src\App.sln -nodeReuse:false'
stops = ["api"]

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

`event_log` records every launch, exit and shutdown step; see [Logs](Logs). `exit_with = "main"` is needed for `dotnet run`; see [Recipes](Recipes#net).
