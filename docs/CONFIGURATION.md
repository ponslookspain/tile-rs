# Configuration reference

tile-rs reads one file: `%APPDATA%\tile-rs\config.toml` ([TOML](https://toml.io) format).

- Open it from the tray icon: **Edit config** (it opens in Notepad).
- After saving, choose **Reload config** in the tray menu (or bind the `reload_config` action to a key).
- If the file contains an error, you get a message and the **previous settings stay active**.
- The file is created with documented defaults on the first start. To get the defaults back,
  delete it and start tile-rs again.
- Updating tile-rs never removes or rewrites your settings. Keys introduced by a newer version
  are appended to your file at startup (the old file is kept as `config.toml.bak`). Nothing you
  wrote is changed. **To switch a setting or hotkey off, comment it out with `#` rather than
  deleting the line**: a commented key is respected, but a key that is missing entirely is
  treated as "not set yet" and may be added back by an update.

Other files: log `%LOCALAPPDATA%\tile-rs\tile-rs.log` (reset at every start).

## General settings

These keys go at the top of the file, before any `[section]`.

| Key | Default | Meaning |
|---|---|---|
| `layout` | `"spiral"` | Tiling layout: `spiral` (dwindle, the default), `master_stack` (a master column on the left, the rest stacked on the right), `grid` or `columns`. |
| `inner_gap` | `10` | Pixels between neighbouring windows. `0`–`200`. |
| `outer_gap` | `10` | Pixels around the screen edges. `0`–`200`. The old `gap` still sets both. |
| `animations` | `true` | Smooth movement of windows when the layout changes. |
| `animation_ms` | `140` | Duration of the animation, `0`–`1000` ms. `0` turns animations off. Short moves take proportionally less time. |
| `animation_fps_limit` | `60` | Maximum layout animation and live tile resize rate, `15`–`240` FPS, also bounded by the monitor refresh rate. Set `30` to reduce resize work in heavy applications; `144` permits more frequent updates on a high refresh rate screen. Actual repaint rate depends on the application. Edit in the config and reload it from the tray. See [window motion](WINDOW-MOTION.md). |
| `easing` | `"out_cubic"` | Animation curve: `linear`, `out_quad`, `out_cubic`, `out_quart`, `in_out_cubic`. |
| `resize_step` | `40` | Pixels per key press of the grow/shrink hotkeys, `1`–`500`. A held key uses smaller, instant steps. |
| `fullscreen_windows` | `"minimize"` | What to do with a window that runs fullscreen or borderless fullscreen (a game, a video player): `"minimize"` hides it while its workspace is not shown and restores it when you switch back; `"ignore"` leaves it completely alone. |
| `media_windows` | `"float"` | `"float"` leaves recognized photo/video viewers completely unmanaged, including fullscreen viewers. This includes Telegram's Qt media viewer with its English or Russian title; its main window and chats remain eligible for tiling. `"tile"` allows ordinary media windows into the layout; genuine fullscreen windows still follow `fullscreen_windows`. Explicit application rules take precedence over this media heuristic. |
| `border` | `true` | Draw a border around the focused tiled window (drawn by tile-rs itself, so it works for every window). |
| `border_color` | `"#8dbcff"` | Border colour, `"#rrggbb"`. |
| `border_width` | `2` | Border thickness in pixels, `0`–`20`. |
| `min_tile_width` | `480` | Minimum tile width in every layout, together with confirmed application minimums. Splits reserve room for all neighbours. |
| `min_tile_height` | `320` | Comfortable minimum tile height. |
| `max_tiles` | `0` | Maximum tiles per workspace; `0` uses the actual capacity of the selected layout, screen shape, gaps and window minimums. An explicit limit also applies with overflow disabled. |
| `auto_overflow` | `true` | When the actual layout cannot fit another window, try the following workspaces and follow the admitted window. If disabled or no workspace fits, leave the window floating and log the reason instead of squeezing a neighbour below its minimum. Toggle float or reload settings to retry an automatic float. |
| `workspace_indicator` | `true` | Minimal workspace dots at the top of each monitor. The active dot is blue with an outline, occupied workspaces glow blue, and empty dots are dim. Click a dot to switch that monitor. |
| `workspace_indicator_size` | `"medium"` | Visual dot and click target size: `"small"`, `"medium"`, or `"large"`. This can also be changed in the launcher. |
| `accent_color` | `"#008cff"` | Accent colour of the workspace indicator (`#RRGGBB`): the current workspace, occupied and empty dots and the ring around the current one are drawn in it. Also available in the settings launcher. |
| `smart_gaps` | `false` | Drop the inner and outer gaps when a workspace holds a single window, so it fills the screen. |
| `master_count` | `1` | Master-stack only: how many windows share the master (left) column, 1-8. The `master_count_inc` / `master_count_dec` hotkeys change and save it. |
| `focus_follows_mouse` | `false` | Move focus to the tiled window under the mouse pointer when the pointer moves onto it. This option is also available in the settings launcher. |

## Hotkeys

```toml
[hotkeys]
focus_left  = ["alt+h", "alt+left"]    # a list: several keys for one action
quit        = "ctrl+alt+q"             # or a single key
# pause = "ctrl+alt+p"                 # put # in front of a line to disable the hotkey
```

### Key syntax

`modifier+modifier+key`, case-insensitive. At least one modifier is strongly recommended
(a bare key would be taken away from every application).

- **Modifiers**: `alt`, `ctrl` (or `control`), `shift`, `win` (also `super`, `meta`).
- **Keys**: letters `a`–`z`, digits `0`–`9`, `f1`–`f12`, `left`, `right`, `up`, `down`, `space`,
  `enter`, `tab`, `esc`, `backspace`, `home`, `end`, `pageup`, `pagedown`, `minus`, `plus`
  (the `=`/`+` key, also written `equal`), `comma`, `period`, `numpad_add`, `numpad_subtract`.

A combination that Windows or another program already uses cannot be registered. tile-rs tells
you which one and carries on with the rest.

### Actions

| Action | What it does |
|---|---|
| `focus_left`, `focus_down`, `focus_up`, `focus_right` | Move focus to the neighbouring tile in that direction. |
| `swap_left`, `swap_down`, `swap_up`, `swap_right` | Swap the focused window with its neighbour. |
| `workspace_1` … `workspace_9` | Switch to a workspace. |
| `move_to_workspace_1` … `move_to_workspace_9` | Move the focused window to a workspace and switch to it (the window keeps focus). |
| `workspace_next`, `workspace_prev` | Switch to the next / previous workspace, wrapping around at the ends. |
| `move_to_next_workspace`, `move_to_prev_workspace` | Move the focused window to the next / previous workspace (wrapping) and follow it. |
| `grow_width`, `shrink_width` | Make the focused window wider / narrower. Repeats while the key is held. |
| `grow_height`, `shrink_height` | Make the focused window taller / shorter. Repeats while the key is held. |
| `minimize` | Minimize the focused window (a fullscreen game or a normal tiled window). |
| `toggle_float` | Float the focused window in place, or tile it again. Lasts for the session. |
| `close` | Close the focused window (like Alt+F4). |
| `pause` | Pause / resume tiling. While paused only `pause`, `quit`, `reload_config` and `edit_config` keep their keys. |
| `quit` | Quit; all windows are brought back to the current workspace. |
| `retile` | Re-apply the layout and reset all sizes to equal halves. |
| `cycle_layout` | Switch to the next tiling layout (spiral → master-stack → grid → columns → spiral) and save it. |
| `toggle_monocle` | Show the focused window alone on the whole screen; the others of the workspace stay stacked underneath and `focus_*` cycles through them. Press again to restore the layout (not saved, per workspace). |
| `focus_last` | Focus the window that was focused before the current one, switching workspace if needed. |
| `workspace_back_and_forth` | Return to the workspace the active monitor showed before the current one. |
| `master_count_inc` / `master_count_dec` | Master-stack: put one more / one fewer window in the master column. |
| `focus_next_monitor`, `focus_prev_monitor` | Move keyboard focus to the next / previous monitor (no default key; bind it in `[hotkeys]`). |
| `move_to_next_monitor`, `move_to_prev_monitor` | Send the focused tiled window to the next / previous monitor, into the workspace that monitor is showing (no default key). |
| `reload_config` | Re-read `config.toml`. |
| `edit_config` | Open `config.toml` in Notepad. |

An unknown action name is an error (it is reported on reload, with the name).

### Default bindings

| Action | Keys |
|---|---|
| `focus_*` | `alt+h/j/k/l`, `alt+left/down/up/right` |
| `swap_*` | `alt+shift+h/j/k/l`, `alt+shift+left/down/up/right` |
| `workspace_N` | `alt+N` |
| `move_to_workspace_N` | `alt+shift+N` |
| `workspace_next` / `workspace_prev` | `alt+shift+period` / `alt+shift+comma` |
| `move_to_next_workspace` / `move_to_prev_workspace` | `ctrl+alt+period` / `ctrl+alt+comma` |
| `grow_width` / `shrink_width` | `alt+plus`, `alt+numpad_add` / `alt+minus`, `alt+numpad_subtract` |
| `minimize` | `alt+shift+m` |
| `toggle_float` | `alt+shift+f` |
| `close` | `alt+shift+q` |
| `cycle_layout` | `ctrl+alt+l` |
| `toggle_monocle` | `alt+m` |
| `focus_last` | `alt+z` |
| `workspace_back_and_forth` | `alt+shift+z` |
| `master_count_inc` / `master_count_dec` | `alt+period` / `alt+comma` |
| `pause` | `ctrl+alt+p`, `alt+shift+p` |
| `quit` | `ctrl+alt+q` |

No default keys: `grow_height`, `shrink_height`, `retile`, `reload_config`, `edit_config`
(present as commented examples in the generated file).

An older `config.toml` is updated in place on start-up: missing `toggle_float`, `close`,
`cycle_layout`, `toggle_monocle`, `focus_last`, `workspace_back_and_forth`, `master_count_inc`,
`master_count_dec`, `quit`, `workspace_next`, `workspace_prev`, `move_to_next_workspace` and
`move_to_prev_workspace` bindings are added (a commented-out or removed one is left alone).

## Window rules

```toml
[[rules]]
process = "Telegram"
workspace = 9

[[rules]]
title = "Settings"
action = "float"
```

Each `[[rules]]` block describes one rule. The **first** matching rule wins.

| Field | Meaning |
|---|---|
| `process` | Executable name, with or without `.exe` (`"Telegram"`, `"chrome.exe"`). Exact, case-insensitive. |
| `class` | Window class name. Exact, case-insensitive. (`tile-rs.exe list` prints classes.) |
| `title` | A part of the window title. Case-insensitive. |
| `action` | `"float"`: leave the window completely unmanaged, including fullscreen. `"tile"`: override media/PiP and non-resizable heuristics; a genuine fullscreen window still follows `fullscreen_windows`. |
| `workspace` | `1`–`9`: new windows of this app open on that workspace. |

A rule needs at least one of `process`, `class`, `title`, and all given fields must match.
Use `tile-rs.exe list` to see what the windows on your screen are called.

Windows that are never tiled regardless of rules: invisible or minimized windows, dialogs
(owned windows), child/no-activate/tool windows, the taskbar and desktop, windows
you have maximized yourself, and the windows of elevated (administrator) processes while tile-rs
itself is not running as administrator.

Known console hosts are eligible even when their native style omits the usual resizable flag.
Match the window host (`conhost`, `OpenConsole`, `WindowsTerminal`) rather than assuming that
the HWND belongs to `cmd` or `powershell`. Embedded terminal panes have no independent top-level window.
Minimizing, temporarily hiding or maximizing a managed window preserves its workspace and order.
Restoring it makes it eligible for that workspace again; it does not become a newly opened window.

## Window diagnostics

The tray's **Export window diagnostics** saves `%LOCALAPPDATA%\tile-rs\window-diagnostics.json`.
It records the running manager's assignments, placement generations, requested/observed frames,
confirmed minimums, automatic floating reasons and native classification facts. Window titles are
excluded by default. `tile-rs.exe diagnose --titles` explicitly requests titles in that live export.

`tile-rs.exe diagnose --output C:\path\windows.json` takes a read-only native snapshot in a short-lived
process. Its `manager_state_available` is false: use the tray export for the running manager's internal
state. Add `--titles` only when the window titles are needed. Diagnostics never start another tiling loop.
Placement details in the log are enabled with `TILE_RS_PROFILE=1`; ordinary decisions and errors include
Unix millisecond timestamps. HWND/PID/thread checks and retained process handles reject stale process
cache entries; if both destroy/create notifications are missed, same-thread HWND reuse cannot always
be distinguished from the original window.

## Example: a compact personal config

```toml
inner_gap = 4
outer_gap = 10
animation_ms = 100
border_color = "#ff9e64"

[hotkeys]
focus_left = "win+h"
focus_down = "win+j"
focus_up = "win+k"
focus_right = "win+l"
workspace_1 = "win+1"
workspace_2 = "win+2"
quit = "ctrl+alt+q"
pause = "ctrl+alt+p"

[[rules]]
process = "Spotify"
workspace = 9
```
