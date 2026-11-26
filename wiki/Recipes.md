# Recipes

## .NET

`dotnet run`, `dotnet build` and `dotnet watch` leave build servers running (MSBuild nodes, `VBCSCompiler`). Since they belong to the app, brady-bunch would by default treat the app as still alive: exits are never noticed, `restart` never fires, and shutdown waits out every timeout. Set `exit_with = "main"` so the app's exit is judged by `dotnet` itself and the build servers are killed with it.

```toml
[defaults]
exit_with = "main"
shutdown_timeout_ms = 30000

[[task]]
name = "build"
command = 'dotnet build src\App.sln -nodeReuse:false'
stops = ["api", "worker"]

[[app]]
name = "api"
command = 'dotnet run --no-build --project src\Api'
ready = { http = "https://localhost:8002" }
restart = "on-failure"
env = { ASPNETCORE_ENVIRONMENT = "Development" }
url = "https://localhost:8002"

[[app]]
name = "worker"
command = 'dotnet run --no-build --project src\Worker'
depends_on = ["api"]
```

- **Build once in a task, then `--no-build`.** Two apps building the same project at the same time can fail on locked files. Running the built exe directly (`'src\Api\bin\Debug\net9.0\Api.exe'`) also works and starts faster.
- **Shutdown time.** ASP.NET Core's `HostOptions.ShutdownTimeout` defaults to 30s; give `shutdown_timeout_ms` at least as long as your app needs.
- **Rebuild without restarting everything:** `brady-ctl run build`. See [Tasks and Rebuilds](Tasks-and-Rebuilds).
- **`ready = { log = 'Now listening on' }`** works too, if the app has no endpoint you can poll.

## Node / npm

```toml
[[task]]
name = "install"
command = 'cmd /c npm ci'
cwd = 'web'
skip_if_running = ["web"]

[[app]]
name = "web"
command = 'cmd /c npm run dev'
cwd = 'web'
ready = { http = "http://localhost:5173" }
running_if = { tcp = 5173 }
url = "http://localhost:5173"
```

- `npm`, `npx`, `yarn` and `pnpm` need `cmd /c`.
- `path_prepend = ['node_modules\.bin']` lets you call local tools directly.
- Adding `log` makes npm drop its colours, since it no longer sees a console. Prefer `brady-ctl tail`.

## Java (Solr, Jetty, ...)

The JVM treats Ctrl+C as a normal shutdown and runs its shutdown hooks, so Java servers stop cleanly with the defaults.

```toml
[[app]]
name = "solr"
command = 'C:\solr\bin\solr.cmd start -f -p 8983'
ready = { http = "http://localhost:8983/solr/" }
skip_if_ready = true
shutdown_timeout_ms = 30000
env = { JAVA_HOME = 'C:\Program Files\Microsoft\jdk-21' }
```

- **Run in the foreground** (`-f` for Solr). Otherwise the launcher starts Java in the background and exits, the tile shows `exited`, and Solr is killed instead of stopped gracefully on shutdown.
- **Give it time to stop.** Solr can take several seconds to close its cores.

## Python

```toml
[[app]]
name = "worker"
command = 'python -u worker.py'
log = 'logs\worker.log'
```

Use `-u` (or `PYTHONUNBUFFERED=1` in `env`) whenever the output is piped (`log` or `ready = { log = ... }`), or output arrives in bursts.

## A shell or log tail

```toml
[[app]]
name = "shell"
command = 'powershell.exe -NoLogo -NoExit'
cwd = 'C:\src\app'

[[app]]
name = "errors"
command = 'powershell.exe -NoLogo -Command Get-Content logs\app.log -Wait -Tail 50'
shutdown = "kill"
```

## A service that might already be running

```toml
[[app]]
name = "worker-service"
command = 'C:\src\worker\bin\WorkerService.exe'
running_if = { process = "WorkerService" }
```

If it's already running (as a Windows service, or from another checkout), the tile shows `already running` and dependents start straight away. See [Startup Order and Readiness](Startup-Order-and-Readiness#apps-that-may-already-be-running).

## Tools in varying install locations

```toml
[defaults]
path_prepend = ['C:\Program Files\nodejs', 'D:\Tools\nodejs', '%LOCALAPPDATA%\Programs\nodejs']
```

Folders that don't exist are skipped, so one config works on machines with different layouts.

## A team-wide config

Check `brady.toml` into the repo, set `min_version`, and use profiles instead of personal copies:

```toml
min_version = "0.8.0"

[profiles]
frontend = { only = ["web"] }
backend = { skip = ["web"] }
quick = { skip = ["build"] }
```

A setup script can keep everyone's brady-bunch new enough:

```powershell
& ([scriptblock]::Create((irm https://github.com/ryancole/brady-bunch/releases/latest/download/install.ps1))) -Config .\brady.toml
```
