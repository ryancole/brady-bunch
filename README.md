# brady-bunch

Launches a list of programs as child processes and tiles their windows inside one host window. Closing the host shuts the children down cleanly.

```
brady-bunch.exe [config.toml]
```

The config defaults to `brady.toml` in the working directory, or next to the exe if there isn't one there. See [brady.example.toml](brady.example.toml).

## Install

Download the zip from [Releases](https://github.com/ryancole/brady-bunch/releases), extract it anywhere, and copy `brady.example.toml` to `brady.toml` next to the exe. The exe has no dependencies.

To publish a release, bump `version` in `Cargo.toml`, commit, then push a matching tag:

```
git tag v0.1.0
git push origin v0.1.0
```

## Behaviour

- **Launch:** each app runs in its own job object set to kill on close. If the host crashes or is force-killed, Windows kills every child and grandchild.
- **Console apps:** detected from the exe's PE header, or set with `console = true`. They run under an explicit `conhost.exe` so they get an embeddable console window even when Windows Terminal is the default terminal.
- **Tiling:** the first visible top-level window of any process in the app's job is reparented into a grid cell. If that window closes, for example a splash screen, the next one is picked up.
- **Shutdown** (closing the window, or `taskkill` without `/f`):
  - Console apps get Ctrl+C. If they're still running, their console window is closed (`CTRL_CLOSE_EVENT`).
  - GUI apps get `WM_CLOSE`.
  - Each step waits `shutdown_timeout_ms` before escalating. Anything left after that is terminated.
  - Closing the host a second time kills everything immediately.

## Limitations

- Store/UWP apps (the new Notepad, Calculator, Windows Terminal) don't embed.
- Elevated apps can't be embedded by a host that isn't elevated.
- `taskkill /f` on the host skips graceful shutdown; the job objects still kill the children.

## Testing

`cargo build --example probe` builds a console app that logs which control event it received. Point an `[[app]]` at `target\debug\examples\probe.exe <logfile>`.
