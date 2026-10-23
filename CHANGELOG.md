# Changelog

All notable changes to tile-rs are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project
follows [Semantic Versioning](https://semver.org) (see [docs/RELEASING.md](docs/RELEASING.md)).

## [Unreleased]

### Added
- Window sizes you set by dragging or with the keyboard are remembered per screen and workspace
  and come back after a restart.
- `Alt+[` / `Alt+]` focus the previous / next monitor, `Alt+Shift+[` / `Alt+Shift+]` send the
  window there. Older configs get the keys added on start-up.
- `Alt+/` opens the settings on the Hotkeys page.
- tile-rs says once per program when it leaves a window alone because the program runs as
  administrator.
- `live_resize = "live" | "preview" | "auto"`: with outlines, a separator or edge drag shows the
  new tiles and the windows move once on release. `auto` (default) switches to outlines with more
  than five windows on the monitor or on a slow machine.
- Separator highlights fade on hover; a dragged window outlines the tile it would land in.
- Switching workspace slides the windows in a short way; on arrival each window repaints
  completely, so browsers and Electron apps do not leave their content behind.

### Changed
- A window dropped onto a workspace dot takes you along to that workspace. Moving the active
  window from the dot's right-click menu still leaves you where you are.
- Dragging a separator is smoother with many windows: the windows on both sides of it move
  together, windows further away update less often, and the pointer moves in small steps.
- A layout change during a running animation keeps the windows' speed instead of restarting it.

### Fixed
- A window maximized on one workspace no longer stays on top when you switch to another
  workspace: it is minimized and comes back maximized when you return. A window you minimized
  yourself is left alone, and one you bring back from the taskbar joins the workspace you are on.
  This also covers windows that open maximized (browsers, Explorer) and floating windows that fill
  their monitor, such as a video player or a photo viewer in fullscreen.
- More video players are recognised and left floating: Movies & TV, the Media Player of Windows 11,
  SMPlayer, GOM Player, Kodi, Plex, Stremio and others.
- With "No gaps for a single window" (`smart_gaps`) turned on, windows are tiled again: the lone
  first window filled the screen without gaps but was then checked against the gapped area,
  rejected, and every window was left floating.
- Moving several windows at once no longer tries a batch that Windows always rejected: each frame
  used to fail, write a line to the log and only then place the windows one by one. Drags and
  animations, especially right after start, react faster, and window edges no longer jitter
  while you drag a separator.
- The settings window no longer shows parts of other windows after it is restored, resized,
  shown again or moved to another display: it repaints completely then.
- The workspace dots stay above a window you are dragging, also above other always-on-top windows.
- After the computer wakes from sleep or hibernation the layout is rebuilt, and once more a few
  seconds later for monitors that come back late.

## [0.7.1] - 2026-10-23

### Security
- Updates are now verified with a digital signature in addition to the checksum, so an update
  can only be installed if it was published by the tile-rs maintainer. Installing an update no
  longer runs PowerShell.

### Fixed
- Windows left on other workspaces are brought back before Windows signs out or shuts down, so
  apps no longer remember an off-screen position and reopen out of sight.
- Moving a window to another monitor no longer freezes tile-rs while that app is not responding.
- The list of hidden windows is written safely, so a crash cannot leave it half-written.

### Changed
- With monitors of different refresh rates, animations on the slower monitor no longer run at
  the faster monitor's pace.
- The settings window code was reorganised; it looks and behaves the same.

## [0.7.0] - 2026-10-23

### Added
- Ways to support the project: a "Support tile-rs" item in the tray menu and in About, donation
  links in the README.
- Drag a window onto a workspace dot to send it to that workspace; right-click a dot to open it or
  move the active window there.
- First-run tour in the settings window that ticks off three things to try, with a way to pause
  tiling for people who do not want it.
- Settings: autosave (no Save button), -/+ steppers with range checks, hotkeys refused by Windows
  are marked, one-click Alt or Win + Alt key sets, rules from any `.exe`, Reset to defaults, a
  paused banner with Resume.
- Config: punctuation key names `semicolon`, `slash`, `grave`, `bracketleft`, `backslash`,
  `bracketright`, `quote`.

### Changed
- The whole program is in English.
- Settings has four pages: General (with More options), Hotkeys, Apps, About & updates.
- Tray: a left click opens the settings (double-click no longer pauses); technical items moved to
  a More submenu. Pausing shows a notification.
- Hotkeys are recorded by the physical key, so they work on any keyboard layout and with
  Shift + digit; hints show arrow keys.

### Fixed
- Workspace dots stay above a dragged window from the first drag, not only from the second.
- The "some hotkeys were not registered" message no longer pops up after every settings change.

## [0.6.0] - 2026-10-21

### Added
- Redesigned settings launcher: light soft-dashboard look with white rounded cards, an icon
  sidebar, authored outline icons, pill buttons and the blue accent; the live layout miniature
  follows the layout, gap and master count; one keycap per hotkey binding; light window caption.
- Settings launcher: hotkey editor (record a combination by pressing it, add a second one, clear,
  reset all, conflict warnings), application rules editor with a picker of running windows, `smart_gaps`
  and `master_count` settings, an unsaved-changes indicator and a one-time welcome card with the
  main shortcuts. The logic lives in `launcher_model.rs`, independent of the UI toolkit.
- `tile-rs.exe --restore-windows` brings back windows left parked off-screen after a hard kill.
- About card (with the Slint attribution) in Settings -> Advanced.
- Monocle (`toggle_monocle`, `alt+m`): show the focused window alone on the whole screen and
  cycle through the workspace's windows with `focus_*`; press again to restore the layout.
- Several master windows in master-stack (`master_count`, `master_count_inc` / `master_count_dec`);
  growing a stack window with `grow_width` now narrows the master column.
- `focus_last` (`alt+z`) and `workspace_back_and_forth` (`alt+shift+z`).
- `focus_next_monitor`/`focus_prev_monitor` and `move_to_next_monitor`/`move_to_prev_monitor`
  actions, and a `smart_gaps` option that removes the gaps around a lone window.
- Suspend animations for 15 s when frames repeatedly take far longer than the frame interval
  (slow machines), and trim the working set after 20 s of idleness.

### Changed
- Resize real application windows during edge and divider gestures, without screenshot overlays.
  Coalesce each window's pending target, pace slow clients independently and apply final sizes
  immediately on release. Divider dragging no longer depends on snapshot capture or settings.
- Remove snapshot rendering and its launcher toggle; existing `snapshot_animations` keys are ignored.
- Cap animation work at a configurable 60 FPS by default; cache display refresh queries and
  avoid monitor-list copies, redundant border paints and divider-window recreation.
- Coalesce deferred motion events between lifecycle barriers and bound replay work so callback
  bursts yield to frame scheduling. Bound retained motion keys and profiling samples.
- Reuse dependency caches across CI/release jobs, lock dependency resolution in build scripts,
  and reuse the packaged executable when building the release installer.
- Tests that change the shared config now serialize on one lock, so `cargo test` is reliable
  without `--test-threads=1`; the build scripts load the MSVC environment themselves
  (`scripts/vcenv.ps1`).
- Smaller executable (15.75 MB to 9.8 MB): Slint is built with software rendering only, the
  release profile uses `opt-level = "s"` and `panic = "abort"` (the panic hook still restores
  parked windows).
- Submit every animation frame as one `DeferWindowPos` batch (shrinking tiles first) so a layout
  changes within a single composition frame, and time frames to the display refresh from DWM
  timing info instead of a free-running periodic timer (kept periodic when `animation_fps_limit`
  is below the refresh rate).

### Fixed
- Restore the automatic per-workspace tile limit (derived from the monitor's work area and the
  minimum tile size, 2 to 8 tiles): with `auto_overflow` on, the next window now opens on the next
  workspace instead of being squeezed in. The workspace indicator dots are slightly larger.
- Redesign the workspace indicator as a white pill with soft shadows (8 px padding and gap, a
  24x12 capsule for the current workspace, 12x12 dots for the others, an outer ring around the
  current one), anti-aliased and scaled per monitor DPI. Its colours come from the new
  `accent_color` setting (hex, also in the settings launcher). It reserves 36 px at the top of the
  work area (28 px pill + 8 px margin) at 100% scale.
- Park inactive-workspace windows at their own off-screen spot instead of the one Windows uses for
  minimized windows, track them (with original positions) in a persisted list, and restore them
  from it after a panic or crash on the next start.
- Keep every placement inside the work area of its monitor, move windows onto the destination
  monitor before sizing them (avoids DPI rescaling missing the target), and ask new windows for
  their minimum size before layout so tiles are sized for them up front.
- Confirm client tracking minimums before learning rejected sizes, preserving neighbours' minimums
  in every layout and using actual layout capacity for workspace overflow or an explained float fallback.
- Resolve media and explicit floating rules before fullscreen workspace assignment, recognize
  Telegram's English/Russian media viewer and browser PiP, and admit supported console host windows.
- Preserve window workspace/order across temporary hide, minimize and maximize states; reconcile
  late titles and restored windows, and keep manual floating consistent across event and scan paths.
- Add timestamped decisions and read-only JSON window diagnostics through the tray or `diagnose`.
- Discard learned window minimums and stale placement state after a successful settings reload,
  so lowering the configured minimum size permits smaller tiles without restarting the manager.
- Keep divider gestures active when clients clamp sizes or DWM reports delayed geometry; learn
  confirmed minimum-size feedback without rolling the layout back. Keep captured handles highlighted
  outside their bounds and clip input surfaces to actual gaps instead of covering client content.
- Keep frame-border offsets stable during live resize and skip redundant native geometry/size
  notifications, including moves that preserve the application's viewport size.
- Restore surviving windows on divider cancellation and prevent expected live geometry events
  from cancelling the gesture; the captured handle follows the moving gap.
- Avoid desktop rescans for unrelated hidden/created/destroyed windows; bound reconciliation
  retries when clients repeatedly reject an unchanged position and stop extending debounce
  deadlines indefinitely during sustained event bursts.
- Run tray update checks off the manager thread, suppress duplicate requests, reject incomplete
  or unsuccessful HTTP responses, and compare prerelease versions correctly.
- Save launcher/layout settings and migration backups through complete sibling files before
  replacement, preserving the previous file when writing or replacement fails.

## [0.5.0] - 2026-10-18

### Added
- Draggable tile gaps in Spiral and Master/stack: frozen previews follow the pointer while actual
  windows keep their sizes until release. Escape cancels; known minimum sizes constrain the drag.
- Frozen-image resize and layout transitions, using hardware Direct2D with GDI fallback, plus a
  directional fade when switching workspaces. The launcher exposes `snapshot_animations`.
- The settings launcher is now a Slint UI in a separate process, so its GUI event loop cannot block
  window tracking or tiling. Existing launcher actions remain available, and saved settings are
  validated before the running manager is asked to reload them.
- Windows x64 releases statically include the MSVC runtime, so users do not need to install the
  Visual C++ Redistributable separately.
- The settings launcher now uses a clearer two-column layout, dark contrast, direct GitHub access,
  and exposes the existing `focus_follows_mouse` option. The workspace switcher uses compact,
  unnumbered dots with blue active/occupied states, plus small, medium, and large size options.
- Multi-monitor tiling: every screen keeps its own layout and current workspace, and a window is
  tiled on the screen it is on (move it to another screen and it re-homes there). Workspace hotkeys
  and the workspace actions apply to the screen under the pointer (or the screen of the focused
  window). Fullscreen windows are tracked per screen, so switching one screen's workspace does not
  hide a game running on another. Each monitor also has its own clickable workspace indicator and
  reserves the indicator bar from its tiling area. The `list` command groups windows by monitor,
  and the one-shot `tile` command lays out windows on each monitor.
- A "Check for updates" item in the tray menu and in the settings launcher. It asks the GitHub
  releases API for the latest tag and reports whether a newer version is available. It runs only
  when you ask for it; there is no background network access.
- `workspace_next` / `workspace_prev` and `move_to_next_workspace` / `move_to_prev_workspace`
  actions that step through workspaces and wrap around at the ends (default keys `Alt+Shift+.`/`,`
  and `Ctrl+Alt+.`/`,`).
- Separate `inner_gap` (between windows) and `outer_gap` (around the screen edges) settings. The
  old single `gap` still works and sets both, so existing configs are unchanged.
- A `focus_follows_mouse` setting (default `false`) that moves focus to the tiled window under the
  mouse pointer when the pointer moves onto it. It runs on a timer only while enabled.
- A `toggle_float` action (default `Alt+Shift+F`) that floats the focused window in place, or tiles
  it again if it is already floating. The choice lasts for the session (a config reload keeps it).
- A `cycle_layout` action (`Ctrl+Alt+L`) that switches to the next layout and saves it, so the
  layout can be changed without opening the settings launcher.
- Three layouts beside the spiral: `master_stack` (a master column on the left, the rest stacked on
  the right), `grid` and `columns`. The `layout` setting and the settings-launcher picker now work
  (before they fell back to the spiral with a "not implemented" warning). Spiral and master-stack
  resize by dragging an edge; grid and columns use equal, fixed tiles.
- Grid layouts now adapt their row and column count to the monitor shape and avoid sparse rows.
- Moving the focused window to another workspace with `Alt+Shift+1`…`9` now follows it: the target
  workspace opens with the window focused (previously the window was parked and you stayed put).
- A `close` action (default `Alt+Shift+Q`) that closes the focused window, like Alt+F4. It is added
  to existing `config.toml` files that lack it, so the key works even on an old config.
- An existing `config.toml` that predates the `quit` hotkey gets it added on start-up, so
  `Ctrl+Alt+Q` quits tile-rs even on an old config (a commented-out or removed one is left alone).

### Fixed
- Workspace transitions cover outgoing windows before switching, avoiding a new/old/new flash.
- Minimum-size detection treats width and height independently and rejects stale DWM frames,
  preventing a height constraint from blocking horizontal resizing in the opposite direction.
- Resize hotkeys and swaps avoid synchronous monitor capture; held resize keys commit neighbours
  together immediately instead of accumulating snapshot animations.
- Cancel stale motion on workspace switches and pause, preserve an unchanged animation target,
  recognize geometry events per window, and queue reentrant WinEvents until the layout releases
  its state. Final window placements use a batch with bounded individual fallback.
- Window tracking now drops destroyed HWND state immediately, migrates windows deliberately when
  dragged between monitors, preserves workspace state when monitor enumeration changes, verifies
  settled placements, bounds retries for rejected target geometry, recovers a lost move-size end
  event, and records Win32 placement/hook failures for diagnosis.
- Packaging now preserves other files in `release/`, and the release workflow uploads only assets
  matching the tag version.
- Fullscreen windows moved between monitors update their monitor assignment, and workspace
  selection safely ignores a stale monitor callback during display hot-plug.
- Windows of elevated (administrator) processes are no longer tiled while tile-rs itself runs
  without rights: a non-elevated tile-rs cannot move them, so they used to sit frozen in a tile.
  They are shown as skipped in `tile-rs.exe list` with the reason "elevated".
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
- Reconciliation uses indexed membership sets for present, new, and currently animating windows,
  avoiding repeated nested scans when a large number of windows are open.

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

[Unreleased]: https://github.com/ponslookspain/tile-rs/compare/v0.7.1...HEAD
[0.7.1]: https://github.com/ponslookspain/tile-rs/compare/v0.7.0...v0.7.1
[0.7.0]: https://github.com/ponslookspain/tile-rs/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/ponslookspain/tile-rs/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/ponslookspain/tile-rs/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/ponslookspain/tile-rs/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/ponslookspain/tile-rs/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/ponslookspain/tile-rs/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/ponslookspain/tile-rs/releases/tag/v0.1.0
