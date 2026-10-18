# Changelog

All notable changes to tile-rs are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project
follows [Semantic Versioning](https://semver.org) (see [docs/RELEASING.md](docs/RELEASING.md)).

## [Unreleased]

### Added
- Moving the focused window to another workspace with `Alt+Shift+1`…`9` now follows it: the target
  workspace opens with the window focused (previously the window was parked and you stayed put).
- An existing `config.toml` that predates the `quit` hotkey gets it added on start-up, so
  `Ctrl+Alt+Q` quits tile-rs even on an old config (a commented-out or removed one is left alone).

### Fixed
- The focus border now appears at once when you click a tile. A workspace-indicator hover timer
  that was never stopped kept firing every 15 ms (and collided with the re-tile debounce timer),
  which made focus updates lag more and more the longer tile-rs ran.
- The above also fixes the indicator's hover animation, which could not run while the timer message
  was being swallowed by the message loop.

### Changed
- Less work while idle and while arranging windows: unrelated window events no longer schedule a
  full re-scan of every window, and the re-scan no longer builds the skip list it does not use.
- Process-name lookups reuse a small buffer instead of allocating 64 KB per process, the log file
  is kept open (and capped in size) instead of being reopened for every line, and configuration is
  read from a snapshot instead of taking a lock for each setting.
- The workspace indicator is not repositioned or repainted when nothing about it changed.

## [0.4.0] - 2026-10-18

### Added
- Live drag & drop preview: while you drag a tiled window over another one's tile, that window
  glides into the slot you are leaving, so it is clear the two will swap places when you let go.
  Hovering away slides it back.
- The workspace dots at the top can now be used with the mouse: hovering a dot brightens and gently
  enlarges it (with a short animated transition), a hand cursor appears over it, and clicking a dot
  switches to that workspace.
- `media_windows` setting (`"float"`, the default, or `"tile"`): how to treat a window with a photo
  or video open (a viewer / player showing a media file). `"float"` leaves it as the app opened it,
  so a photo viewer or video player is not crammed into a tile; `"tile"` tiles it like any other
  window. It can be overridden per app with a `[[rules]]` `action = "float"` / `"tile"`, and it is in
  the settings launcher.

### Changed
- Animation frames now follow the primary monitor's refresh rate instead of a fixed 16 ms timer, so
  window movement lines up with the display: it no longer judders against a 60 Hz screen and uses
  the extra headroom on 120/144 Hz. An unreadable refresh rate falls back to the previous 16 ms.
- While a window is gliding, the focus border is repositioned with fewer system calls per frame (no
  window visibility toggle or background erase), keeping the animation cheaper.
- The settings launcher scales with the display's DPI, so its window, controls and text stay
  readable on high-DPI screens instead of rendering tiny.
- The workspace indicator is centred and easier to read: round dots, the current workspace drawn as
  a filled dot with a ring around it, a workspace that has windows filled in, and empty ones dimmed.

### Fixed
- Window titles and executable paths are now read with much larger buffers, so very long titles and
  deeply nested installs (`process` / `title` rules) are no longer silently truncated.
- The focus border is rounded again, at both its outer edge and its inner hole, instead of a square
  ring around a rounded opening.
- The focus border no longer sits on top of a right-click (context) menu: it is hidden while a popup
  menu is open and brought back once the menu closes.
- The focus border no longer flickers while a tiled window is being dragged or resized: it is hidden
  for the duration of the drag and restored once the window snaps to its new tile.
- Resizing a tile by dragging an edge no longer makes the whole layout jump: while the edge is being
  dragged the neighbouring tiles follow the cursor proportionally, and the adaptive refit (minimum
  sizes, narrowed master tile) is applied once when the drag ends.

## [0.3.0] - 2026-10-18

### Added
- A **settings launcher** (tray menu -> *Settings...*, or on first start): every option in one
  window, a layout picker (spiral for now; master-stack, grid and columns are "coming soon"), and
  a "check for updates" link.
- `layout` setting (only `spiral` is implemented so far).
- `easing` setting to pick the animation curve: `linear`, `out_quad`, `out_cubic` (the default),
  `out_quart`, `in_out_cubic`.
- The focus border is now drawn by tile-rs itself (a small click-through overlay window) instead
  of the Windows DWM border colour, so it always shows - including for console windows such as
  Windows PowerShell. It has rounded corners and a new `border_width` setting.
- Adaptive layout: `min_tile_width`/`min_tile_height` keep tiles usable (the master tile is
  narrowed to make room for the rest), `max_tiles` (`0` = automatic from the screen size) caps the
  tiles per workspace, and `auto_overflow` moves a new window to the next workspace (and switches
  to it) when the current workspace is full.
- A small workspace indicator (dots) that reserves its own bar at the top of the work area, so
  windows are tiled below it and it never overlaps them; new `workspace_indicator` setting.
- A fullscreen game or video player now opens on a workspace of its own and tile-rs switches to it.
- Fullscreen windows (games, video players — windows that cover a whole monitor without a title
  bar) now belong to a workspace: tile-rs minimizes them when you switch to another workspace and
  restores them when you come back. The new `fullscreen_windows = "minimize" | "ignore"` setting
  chooses the behaviour (`"ignore"` = the previous "never touch them" behaviour).
- `minimize` action to minimize the focused window, bound to `Alt+Shift+M` by default.

### Fixed
- The launcher's hotkey editor wrote the bindings without quotes, which made the config invalid on
  "Save & apply"; it now always writes valid TOML (quoted strings / lists).
- The focus border is a transparent-centred colour-key frame, so it can no longer appear as a solid
  colour while a window is being placed or resized; it is also hidden for maximized windows.
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

[Unreleased]: https://github.com/ponslookspain/tile-rs/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/ponslookspain/tile-rs/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/ponslookspain/tile-rs/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/ponslookspain/tile-rs/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/ponslookspain/tile-rs/releases/tag/v0.1.0
