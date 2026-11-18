# Changelog

All notable changes to brady-bunch. Each release's section is used as its GitHub release notes.

## [Unreleased]

## [0.1.1] - 2026-11-17

### Changed

- Release zips include this changelog, and each release's notes come from its changelog section.

## [0.1.0] - 2026-11-17

### Added

- Host window that launches apps from a TOML config and tiles their windows in a grid.
- Graceful shutdown: Ctrl+C for console apps, `WM_CLOSE` for GUI apps, then a forced kill after a timeout.
- Per-app job objects, so children never outlive the host.
- GitHub release workflow that publishes a zip with a SHA256 checksum.
