# Shutdown

## What starts a shutdown

A close request to the host window: clicking X, `brady-ctl down`, `Process.CloseMainWindow()`, or `taskkill /pid <host pid>` **without** `/f`.

Force-killing the host (`taskkill /f`, `Stop-Process`, Task Manager's End task on the process) skips graceful shutdown. Every child is still killed, because each app runs in a job object that Windows closes with the host, but nothing gets a chance to clean up.

## Order

Apps stop one at a time. Each app stops after every app that depends on it; otherwise in reverse config order, so the first apps in the config stop last. Each stop finishes before the next begins.

## How each app is stopped

Each step waits up to the app's `shutdown_timeout_ms` (default 5000) for it to exit, then moves on:

| `shutdown` | Steps |
|---|---|
| `auto`, console app | Ctrl+C → close the console window → kill |
| `auto`, GUI app | `WM_CLOSE` → kill |
| `ctrl-c` | Ctrl+C → kill |
| `close` | `WM_CLOSE` → kill |
| `kill` | kill immediately |

A batch file left asking `Terminate batch job (Y/N)?` after Ctrl+C is handled: once only `cmd.exe` is left, its console is closed right away rather than after the timeout.

## Tuning it

- **Worst case** is roughly the sum over all apps of (steps × timeout + 2s). If shutdown feels slow, check the `event_log` for which step stopped each app.
- **Give slow servers more time.** ASP.NET Core waits up to 30s for in-flight work by default; Solr takes a while to close cores. Raise `shutdown_timeout_ms` for them, or the app gets killed part-way through.
- **Skip the ceremony** for apps that don't need it with `shutdown = "kill"`, or lower their `shutdown_timeout_ms`.
- **`dotnet run`/`dotnet watch`** need `exit_with = "main"`, or shutdown waits out every timeout on the build servers they leave behind. See [Recipes](Recipes#net).
- **Close the window a second time** during shutdown to kill everything left immediately.

## Knowing when it's done

The host exits once every app has stopped. `brady-ctl down` waits for that and prints one line per app:

```
api: stopped by ctrl-c after 1834 ms
```

It exits with code 4 if any app had to be killed, or the host is still shutting down after `--timeout` (default 300s). `stopped by kill` means the app ignored the graceful steps.
