# Contributing

## Building

```powershell
cargo build --release
```

This builds the three exes into `target\release`: `brady-bunch.exe` (the host), `brady-shim.exe` (runs each console app inside its console) and `brady-ctl.exe` (command-line control). Keep them in the same folder when running.

## Testing

`cargo test` runs the unit tests and `tests/shim.rs`, which runs `brady-shim.exe` directly (including the command-line quoting regression).

`cargo build --example probe` builds a console app that logs which control event it received. Point an `[[app]]` at `target\debug\examples\probe.exe <logfile>`.

## Docs

- [docs/AGENTS.md](docs/AGENTS.md) is the complete reference, shipped in the release zip. Update it with every behaviour change.
- [wiki/](wiki) holds the user guide. It's a copy of the GitHub wiki, which is a separate git repo (`https://github.com/ryancole/brady-bunch.wiki.git`): after changing pages here, copy them into a clone of that repo and push it.
- [assets/gen-logo.js](assets/gen-logo.js) regenerates the logo and banner: `node assets/gen-logo.js assets`.

## Releasing

Bump `version` in `Cargo.toml`, rename the `[Unreleased]` section of [CHANGELOG.md](CHANGELOG.md) to that version, commit, then push a matching tag:

```
git tag v0.1.0
git push origin v0.1.0
```

The release fails if the changelog has no section for the version, and that section becomes the release notes.
