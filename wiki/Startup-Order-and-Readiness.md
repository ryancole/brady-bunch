# Startup Order and Readiness

## The order things start in

1. Every `[[task]]` runs, one at a time, in config order. See [Tasks and Rebuilds](Tasks-and-Rebuilds).
2. Apps without `depends_on` launch at once, in config order.
3. An app with `depends_on = ["db", "cache"]` waits until every app it names is **ready**. Its tile shows `waiting for db…`.

Dependency cycles and unknown names are config errors.

## When is an app ready?

Without a `ready` key, an app is ready as soon as it's launched. Otherwise, use exactly one of:

| Check | Ready when |
|---|---|
| `ready = { tcp = 5432 }` | Something accepts TCP connections on localhost port 5432 (IPv4 or IPv6). `tcp = "127.0.0.1:5432"` checks a specific address. |
| `ready = { http = "https://localhost:8002/health" }` | A GET returns any HTTP status below 500. Certificates aren't checked, redirects aren't followed, and no proxy is used. |
| `ready = { log = 'Now listening on' }` | The regex matches console output written since this launch. Console apps only; this pipes the app's output, with the same caveat as `log` (see [Logs](Logs)). |

Checks run every 250ms.

### `timeout_ms`

```toml
ready = { http = "https://localhost:8002", timeout_ms = 120000 }
```

The default is 60000. After the timeout the app shows `not ready`, and its dependents say so, but **nothing is killed and checking continues**: dependents still start as soon as it passes. It's a "this is taking too long" signal, not a failure.

### Dependencies only matter at launch

If a dependency exits or restarts later, apps already running are left alone. An app that restarts (automatically, or with `brady-ctl start`/`restart`) waits for its dependencies again.

## Health checks

`ready` is checked until it first passes. To keep checking afterwards, add `health`:

```toml
ready = { http = "https://localhost:8002/health" }
health = { interval_ms = 10000, failures = 3 }   # these are the defaults; health = {} uses them
```

After `failures` failed checks in a row, the tile shows `unhealthy` and the change is written to the `event_log`. It goes back to normal when a check passes. Nothing is restarted, and dependents aren't affected.

Without its own check, `health` reuses `ready`, which must then be `tcp` or `http`. For an app whose `ready` is a log regex, give `health` its own:

```toml
ready = { log = 'compiled successfully' }
health = { http = "http://localhost:3000" }
```

## Apps that may already be running

Sometimes a service is already running on your machine, started by hand or by another project. brady-bunch can notice that and not launch a second copy.

```toml
running_if = { process = "MyWorkerService" }
# or running_if = { tcp = 8983 }
# or running_if = { http = "http://localhost:8983/solr/" }
```

`running_if` is checked once before each launch. If it passes, the app isn't launched; its tile shows `already running` and it counts as ready for its dependents. Click the tile to check again.

- `process` matches a running process's image name, case-insensitively, with or without `.exe`. Use a path (containing `\` or `/`) when the name alone is too common. Processes started by this host don't count.
- `running_if` is separate from `ready`, so it works for apps whose `ready` is a log regex, and for apps with no endpoint at all.

`skip_if_ready = true` is the shorthand: it runs the app's own `ready` check (tcp or http only) once before launching, and skips the launch if it already passes.

```toml
[[app]]
name = "solr"
command = 'C:\solr\bin\solr.cmd start -f -p 8983'
ready = { http = "http://localhost:8983/solr/" }
skip_if_ready = true
```

## Restarting apps that exit

```toml
restart = "on-failure"   # or "always"
restart_delay_ms = 1000
```

`on-failure` covers a non-zero exit code or a failed launch. The delay doubles, up to 30s, each time the app exits within 10s of starting, and resets once it runs longer. Apps are never restarted during shutdown, and a stop with `brady-ctl stop` doesn't trigger one.

Without `restart`, an app that exits shows `exited (<code>)` and its console stays open with the last output. Press Enter in it, click the tile, or run `brady-ctl start <app>` to run it again.
