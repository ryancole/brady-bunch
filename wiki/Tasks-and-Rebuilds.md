# Tasks and Rebuilds

`[[task]]` entries are setup steps, such as builds, package installs and migrations, that must finish before the apps start.

```toml
[[task]]
name = "install"
command = 'cmd /c npm ci'
cwd = 'web'

[[task]]
name = "build"
command = 'dotnet build src\App.sln -nodeReuse:false'
stops = ["api", "worker"]
```

## How tasks run

- One at a time, in config order, each to completion, before any app is launched.
- The running task has a tile. Once every task has succeeded, the task tile disappears and the apps start.
- Tasks default to `exit_with = "main"`, so build servers left running don't hold them open.

## When a task fails

If a task exits with a non-zero code, everything after it waits. Its tile stays with the error output, and the apps show `waiting for build (exited with code 1)`.

Fix the problem, then either press Enter in the task's console or run:

```powershell
brady-ctl run build
```

When it succeeds, startup carries on.

## Rebuilding while the apps run

Once the stack is up, `brady-ctl run <task>` reruns a task. The problem is that running apps usually lock the files a build writes (MSBuild reports MSB3027/MSB3021). List those apps in the task's `stops`:

```toml
[[task]]
name = "build"
command = 'dotnet build src\App.sln -nodeReuse:false'
stops = ["api", "worker"]
```

Then:

```powershell
brady-ctl run build
```

`run` stops `api` and `worker` (dependents first), runs the build in its tile, starts them again, and waits until they're ready:

```
build: done (8.4s); restarted api, worker; ready (14.8s)
```

`--stop app,app` adds more apps for a single run. If the build fails, `run` exits with code 4, the task's tile shows the errors, and the stopped apps wait for the task to succeed. Fix it and run `brady-ctl run build` again.

## Skipping tasks

- **For one launch:** `--skip build` (or a profile with `skip = ["build"]`) doesn't run the task at startup. It shows as `skipped`, nothing waits for it, and `brady-ctl run build` still runs it later. This makes a fast "start without building" profile possible:

  ```toml
  [profiles]
  quick = { skip = ["build"] }
  ```

- **While another copy is running:** `skip_if_running = ["web"]` runs `web`'s `running_if` (or `skip_if_ready`) check first, and skips the task if `web` is already running elsewhere. For example, so `npm install` doesn't rewrite `node_modules` under another running dev server:

  ```toml
  [[task]]
  name = "install"
  command = 'cmd /c npm install'
  cwd = 'web'
  skip_if_running = ["web"]

  [[app]]
  name = "web"
  command = 'cmd /c npm start'
  cwd = 'web'
  running_if = { tcp = 3000 }
  ```

  The named apps need such a check, but they don't need to be in the current selection.
