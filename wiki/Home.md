# brady-bunch

![brady-bunch: all your apps, one tiled window](https://raw.githubusercontent.com/ryancole/brady-bunch/master/assets/banner.svg)

brady-bunch runs a set of Windows programs as one unit. It starts them all, tiles their windows in a grid inside one host window, and stops them all cleanly when you close that window.

It's built for the everyday "I need six terminals open to work on this project" problem: a database, an API, a web dev server, a worker, a log tail. You describe them once in a `brady.toml` file, and from then on one command (or a double-click) brings the whole stack up in the right order.

## What it does

- **Tiles every app's window** (console or GUI) in one host window. Click a tile to type into it; double-click its header to zoom it.
- **Starts things in order.** An app can wait until another one is ready: listening on a port, answering HTTP, or printing a line in its log.
- **Runs setup tasks first**, such as a build or `npm install`, and holds the apps until they succeed.
- **Shuts down gracefully.** Closing the window sends Ctrl+C (or `WM_CLOSE`) to each app in reverse dependency order, and only kills what's left after a timeout. If the host crashes, Windows still kills every child process.
- **Can be scripted.** `brady-ctl up` starts the stack and returns once it's ready; `brady-ctl down` stops it. In between you can check status, restart one app, or rebuild while the rest keeps running.

## Where to start

1. [Installation](Installation)
2. [Getting Started](Getting-Started): your first `brady.toml`, step by step
3. [Configuration](Configuration): every key, with examples

Then, as you need them:

- [Startup Order and Readiness](Startup-Order-and-Readiness): `depends_on`, `ready`, `health`, `running_if`
- [Tasks and Rebuilds](Tasks-and-Rebuilds): builds before startup, and rebuilding while running
- [Profiles and Selections](Profiles-and-Selections): run part of a config with `--profile`, `--only`, `--skip`
- [Using the Window](Using-the-Window): tiles, focus, zoom, crash highlighting
- [brady-ctl](brady-ctl): command-line control and the status JSON
- [Shutdown](Shutdown): how apps are stopped, and how long it can take
- [Logs](Logs): `brady-ctl tail`, app `log` files and the `event_log`
- [Recipes](Recipes): .NET, Node, Java/Solr, Python and more
- [Troubleshooting](Troubleshooting): common problems and limitations

## Requirements

Windows. Nothing else: the release is three standalone exes.
