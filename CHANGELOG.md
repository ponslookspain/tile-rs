# Changelog

All notable changes to tile-rs are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project
follows [Semantic Versioning](https://semver.org) (see [docs/RELEASING.md](docs/RELEASING.md)).

## [Unreleased]

### Added
- `easing` setting to pick the animation curve: `linear`, `out_quad`, `out_cubic` (the default),
  `out_quart`, `in_out_cubic`.

### Changed
- The tray menu opens instantly: the "Start with Windows" state is now read directly from the
  registry instead of starting a `reg.exe` process every time the menu is opened.
- The layout is rebuilt automatically when the screen work area changes (resolution change,
  taskbar resized or set to auto-hide), instead of staying stale until the next manual action.
- Short moves are animated more quickly: a small resize step no longer takes as long as a full
  re-layout, and frames that would not move a window are skipped.
- Animation frames are driven by a high-resolution timer instead of the low-priority `WM_TIMER`,
  so windows keep moving smoothly while the mouse and keyboard are busy. tile-rs no longer raises
  the system-wide timer resolution.

## [0.2.0] - 2026-10-17

### Added
- Coloured border around the focused tiled window (Windows 11): `border` and `border_color` settings.
- The version is shown in the tray menu; `tile-rs.exe --version` and `--help`.
- Settings introduced by a newer version are appended to an existing `config.toml` automatically
  (the previous file is kept as `config.toml.bak`).
- Documentation in English and Russian, configuration reference, contributing guide,
  versioning and release rules, MIT license.
- Continuous integration (format, clippy, tests, build) and an automated release workflow.
- Scripts: `scripts/check.ps1`, `scripts/package.ps1`.

### Changed
- Layout maths moved into its own module (`geometry.rs`) with unit tests; no behaviour change.
- The generated config now tells you to comment a hotkey out (`#`) rather than delete it.

## [0.1.0] - 2026-10-17

First working prototype.

### Added
- Spiral (dwindle) layout; windows are arranged automatically as they open, close or minimize.
- New windows are placed in their tile immediately, other windows glide aside (animations).
- Live resizing by dragging window edges; `Alt +` / `Alt -` (hold to repeat) for width.
- Drag a window onto another tile to swap; dropping elsewhere snaps it back.
- 9 emulated workspaces, focus / swap / move hotkeys (letters and arrows).
- Tray icon with menu, autostart, single-instance guard, first-run dialog.
- `config.toml`: gap, animations, hotkeys, per-application rules (float / workspace).

[Unreleased]: https://github.com/ponslookspain/tile-rs/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/ponslookspain/tile-rs/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/ponslookspain/tile-rs/releases/tag/v0.1.0
