# Profiles and Selections

You don't need a separate config for every combination of apps. Keep one config with everything, and pick what to run.

## From the command line

```powershell
brady-bunch.exe brady.toml --only api
brady-bunch.exe brady.toml --skip worker,functions
```

The same flags work with `brady-ctl up` and `brady-ctl check`.

- **`--only a,b`** runs only these apps, plus everything they depend on (transitively), plus every task.
- **`--skip a,b`** leaves these apps out. Apps that depend on a skipped app don't wait for it. A skipped task isn't run at startup but `brady-ctl run <task>` can still run it.
- Unknown names are config errors, so typos are caught.

## Named profiles

```toml
[profiles]
webdev = { skip = ["worker", "functions"] }
api-only = { only = ["api"] }
quick = { skip = ["build"] }
```

```powershell
brady-ctl up --profile webdev
brady-ctl up --profile webdev,quick
```

Repeat `--profile`, or give a comma-separated list, to combine profiles. `--only` and `--skip` add to them.

## Changing what's running

There's one host per config. Launching a config that's already running brings its window to the front instead of starting a second copy. If the new launch selects apps the running host doesn't have, those are started first.

`brady-ctl up` does the same, and reports what changed:

```powershell
brady-ctl up --profile webdev      # starts what webdev adds; leaves everything else running
brady-ctl up --profile webdev --exact   # also stops apps webdev leaves out
```

Tasks aren't rerun when the selection changes. A task that was skipped when the host started stays skipped; `up` tells you, and `brady-ctl run <task>` runs it.

To run a second, independent host for the same config, pass `--new-instance`. `brady-ctl` can then only reach it by its process id.
