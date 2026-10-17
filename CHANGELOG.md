# Changelog

All notable changes to tile-rs are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project
follows [Semantic Versioning](https://semver.org) (see [docs/RELEASING.md](docs/RELEASING.md)).

## [Unreleased]

### Added
- `easing` setting to pick the animation curve: `linear`, `out_quad`, `out_cubic` (the default),
  `out_quart`, `in_out_cubic`.
- The focus border is now drawn by tile-rs itself (a small click-through overlay window) instead
  of the Windows DWM border colour, so it always shows - including for console windows such as
  Windows PowerShell. New `border_width` setting.
- Adaptive layout: `min_tile_width`/`min_tile_height` keep tiles usable (the master tile is
  narrowed to make room for the rest), `max_tiles` (`0` = automatic from the screen size) caps the
  tiles per workspace, and `auto_overflow` moves a new window to the next workspace (and switches
  to it) when the current workspace is full.
- A small workspace indicator at the top of the screen (dots); `workspace_indicator` setting.
- Fullscreen windows (games, video players — windows that cover a whole monitor without a title
  bar) now belong to a workspace: tile-rs minimizes them when you switch to another workspace and
  restores them when you come back. The new `fullscreen_windows = "minimize" | "ignore"` setting
  chooses the behaviour (`"ignore"` = the previous "never touch them" behaviour).
- `minimize` action to minimize the focused window, bound to `Alt+Shift+M` by default.

### Fixed
- A window that cannot shrink below its own minimum size (e.g. a browser) now gets a tile at least
  that big instead of overlapping its neighbour: the neighbours give up the space.
- A newly opened window with a minimum size now pushes the grid aside immediately, instead of
  overflowing the workspace until it is dragged.
- The focus border is re-applied after a window is moved or resized (DWM drops the custom border
  colour on move), so it now also shows for console windows such as Windows PowerShell.
- Moving a window is no longer mistaken for a resize when the app is larger than its tile (an
  application minimum size): that used to change the split sizes and make windows overlap.
- The focus border now also follows focus changes made by tile-rs itself (keyboard focus, workspace
  switch), not only focus changes made by clicking.
- A drag whose end event was missed (e.g. the window was closed while dragging) no longer leaves
  window management stuck.
- A window that cannot shrink to its tile (application minimum size) is no longer re-resized in a
  loop, which caused flicker.

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
