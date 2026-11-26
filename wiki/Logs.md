# Logs

There are three ways to see what happened.

## `brady-ctl tail <app>`: what's in the console now

```powershell
brady-ctl tail api --lines 100
```

Prints the app's console text. It needs no config changes and works after the app has exited, since its console stays open. Start here.

## `log`: a full copy of an app's output

```toml
[[app]]
name = "api"
command = 'dotnet run --no-build --project src\Api'
log = 'logs\api.log'
```

The output still shows in the tile, and is also written to the file as UTF-8 with colour codes removed. The file is emptied when the host starts; restarts append to it, with a `---- <time> start` line before each run and `---- <time> exited with code N` after.

**The trade-off:** to copy the output, the app's stdout and stderr become a pipe instead of the console. Some programs behave differently when they don't see a console: npm and rspack drop colours and progress bars, and Python (without `-u`) and some C programs buffer their output. .NET, Java and Node servers are fine. `ready = { log = ... }` has the same effect.

**Encoding:** output is read as UTF-8 by default, and the console is switched to UTF-8 so .NET and cmd write UTF-8 too. For a program that always writes in the OEM or ANSI code page, set `encoding = "oem"` or `"ansi"`.

## `event_log`: the host's own record

```toml
event_log = 'brady-events.log'
```

Records launches, exits, readiness, restarts, health changes and each shutdown step with timings:

```
2026-11-25 10:15:39.315-06:00 api: stopping
2026-11-25 10:15:39.381-06:00 api: sent ctrl-c
2026-11-25 10:15:41.215-06:00 api: stopped by ctrl-c after 1900 ms
```

Times are local with their UTC offset, so they line up with the UTC times in `brady-ctl status`. Read the end of it with:

```powershell
brady-ctl events --app api
```

This reads the file, so it works after the host has exited or failed to start.

## Previous runs

Each time the host starts, the last run's `event_log` and `log` files become `name.1.log`, so the record of a crashed session survives a relaunch. `keep_logs = 3` keeps three generations; `0` keeps none.
