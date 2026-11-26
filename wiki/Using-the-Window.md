# Using the Window

Each app gets a tile in the host window, in config order. A tile has a header (name, link, state) and the app's own window below it.

## Typing into apps

- **Click inside a tile** to give that app keyboard focus. Its header turns blue.
- **Click a header or empty space** to give focus back to the host.
- When you switch back to the host window, focus returns to the tile that last had it.

## Selecting text in a console

QuickEdit is turned off in embedded consoles. Otherwise a click would start a text selection, which pauses the app's next console write and can freeze a chatty server.

To select text, press **Ctrl+M** (mark mode) in the console. To have normal mouse selection on an app, set `quick_edit = true` on it. Once an app has exited, its kept-open console has QuickEdit on.

## Zooming a tile

**Double-click a tile's header** to make it fill the window; the other apps keep running. Double-click again to go back to the grid. The zoomed tile is remembered for the next run, along with the window's position and size (`remember_layout = false` turns that off).

## Links

`url = "https://localhost:8002"` on an app shows a link in its header. Clicking it opens your default browser.

## Crashes

When an app exits with a non-zero code or fails to launch, its header turns red and the host's taskbar button flashes until you bring the window to the front. The red stays until the app is launched again. Stopping an app yourself never counts as a crash. `flash_on_crash = false` turns off the flashing.

## Running an app again

An app that exited keeps its console open, showing the last output and the exit code. To run it again:

- press **Enter** in that console,
- click the body of a tile that has no window (exited, stopped or failed apps), or
- run `brady-ctl start <app>`.

## Tile states

| Header shows | Meaning |
|---|---|
| `waiting for <app>…` | Not launched yet: waiting for a dependency or an earlier task. |
| `checking…` | Checking whether it's already running elsewhere (`running_if`/`skip_if_ready`). |
| `already running` | Found running elsewhere, so not launched. Click to check again. |
| `starting…` | Running; no window yet, or its `ready` check hasn't passed. |
| `not ready (pid N)` | Running, but `ready` didn't pass within `timeout_ms`. Still being checked. |
| `unhealthy (pid N)` | Running, but its `health` check keeps failing. |
| `pid N` | Running and ready. |
| `done` | A task that finished successfully. |
| `exited (N)` | Exited on its own with code N, with `restarting in Ns` if a restart is scheduled. |
| `failed: …` | Couldn't be launched, e.g. the exe wasn't found. |
| `waiting to stop…` | Shutting down; waiting for apps that depend on it to stop first. |
| `stopping…` | Being stopped. |
| `stopped` | Stopped. Click the tile to start it. |

## Windows that won't embed

These still run and are still shut down with the rest, but their windows stay outside the host:

- Store/UWP apps, such as the new Notepad, Calculator and Windows Terminal.
- Elevated (admin) apps, when the host itself isn't elevated.

GUI apps can also appear on the desktop for a moment before they're embedded, since brady-bunch can only pick up a window once the app shows it. Console apps don't do this.
