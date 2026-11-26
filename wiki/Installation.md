# Installation

## With the install script (recommended)

In PowerShell:

```powershell
irm https://github.com/ryancole/brady-bunch/releases/latest/download/install.ps1 | iex
```

This downloads the latest release, checks it against the release's SHA-256, installs it into `%LOCALAPPDATA%\Programs\brady-bunch`, and adds that folder to your user PATH. Open a new shell afterwards so `brady-ctl` is found.

Check it worked:

```powershell
brady-ctl --version
```

### Updating

Run the same command again. It refuses while brady-bunch is running from the install folder, so stop your hosts first (`brady-ctl down`, or close their windows).

### Options

To pass options, run the script as a script block:

```powershell
& ([scriptblock]::Create((irm https://github.com/ryancole/brady-bunch/releases/latest/download/install.ps1))) -Version 0.6.0
```

| Option | Does |
|---|---|
| `-Version 0.6.0` | Install a specific version. |
| `-InstallDir <path>` | Install somewhere other than `%LOCALAPPDATA%\Programs\brady-bunch`. |
| `-NoPath` | Don't add the folder to PATH. |
| `-Check` | Only report whether an update is available. |
| `-Config <brady.toml>` | Install what that config's `min_version` needs. Does nothing, without going online, if the installed version is already new enough. |

`-Config` is handy in a project's own setup script: it keeps everyone on a version that can run the project's config, without reinstalling every time.

## From the zip

Download the zip from [Releases](https://github.com/ryancole/brady-bunch/releases) and extract it anywhere. Keep the three exes in the same folder:

| File | Role |
|---|---|
| `brady-bunch.exe` | The host window. |
| `brady-shim.exe` | Runs each console app inside its console. Without it console apps still run, but app `log` files, `ready = { log = ... }`, QuickEdit control, kept-open consoles and exact command-line quoting don't work. |
| `brady-ctl.exe` | Command-line control of a running host. See [brady-ctl](brady-ctl). |

The zip also contains `brady.example.toml` and `AGENTS.md`, a reference written for AI coding agents that drive brady-bunch.

## Pinning a minimum version in your config

If your config uses newer keys, add:

```toml
min_version = "0.8.0"
```

An older brady-bunch then refuses the config with a message saying which version it needs and the command that installs it, instead of a confusing "unknown key" error.

Next: [Getting Started](Getting-Started).
