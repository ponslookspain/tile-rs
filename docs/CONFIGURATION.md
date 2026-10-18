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
| `easing` | `"out_cubic"` | Animation curve: `linear`, `out_quad`, `out_cubic`, `out_quart`, `in_out_cubic`. |
| `resize_step` | `40` | Pixels per key press of the grow/shrink hotkeys, `1`–`500`. A held key uses smaller, instant steps. |
| `fullscreen_windows` | `"minimize"` | What to do with a window that runs fullscreen or borderless fullscreen (a game, a video player): `"minimize"` hides it while its workspace is not shown and restores it when you switch back; `"ignore"` leaves it completely alone. |
| `media_windows` | `"float"` | What to do with a window that has a photo or video open (a viewer / player showing a media file, e.g. `holiday.jpg - Photos` or `clip.mp4 - VLC media player`): `"float"` leaves it as the app opened it (a movable window you can put anywhere), `"tile"` tiles it like any other window. Fullscreen playback is still handled by `fullscreen_windows`. |
| `border` | `true` | Draw a border around the focused tiled window (drawn by tile-rs itself, so it works for every window). |
| `border_color` | `"#8dbcff"` | Border colour, `"#rrggbb"`. |
| `border_width` | `2` | Border thickness in pixels, `0`–`20`. |
| `min_tile_width` | `480` | Comfortable minimum tile width; the master tile is narrowed to keep the rest at least this wide. |
| `min_tile_height` | `320` | Comfortable minimum tile height. |
| `max_tiles` | `0` | Maximum tiles per workspace; `0` = chosen automatically from the screen size. |
| `auto_overflow` | `true` | When a workspace is full, open a new window on the next workspace and switch to it. |
| `workspace_indicator` | `true` | Small row of workspace dots at the top of the screen. Hover a dot to highlight it and click it to switch to that workspace. |
| `focus_follows_mouse` | `false` | Move focus to the tiled window under the mouse pointer when the pointer moves onto it. |

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
| `grow_width`, `shrink_width` | Make the focused window wider / narrower. Repeats while the key is held. |
| `grow_height`, `shrink_height` | Make the focused window taller / shorter. Repeats while the key is held. |
| `minimize` | Minimize the focused window (a fullscreen game or a normal tiled window). |
| `toggle_float` | Float the focused window in place, or tile it again. Lasts for the session. |
| `close` | Close the focused window (like Alt+F4). |
| `pause` | Pause / resume tiling. While paused only `pause`, `quit`, `reload_config` and `edit_config` keep their keys. |
| `quit` | Quit; all windows are brought back to the current workspace. |
| `retile` | Re-apply the layout and reset all sizes to equal halves. |
| `cycle_layout` | Switch to the next tiling layout (spiral → master-stack → grid → columns → spiral) and save it. |
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
| `grow_width` / `shrink_width` | `alt+plus`, `alt+numpad_add` / `alt+minus`, `alt+numpad_subtract` |
| `minimize` | `alt+shift+m` |
| `toggle_float` | `alt+shift+f` |
| `close` | `alt+shift+q` |
| `cycle_layout` | `ctrl+alt+l` |
| `pause` | `ctrl+alt+p`, `alt+shift+p` |
| `quit` | `ctrl+alt+q` |

No default keys: `grow_height`, `shrink_height`, `retile`, `reload_config`, `edit_config`
(present as commented examples in the generated file).

An older `config.toml` is updated in place on start-up: missing `toggle_float`, `close`,
`cycle_layout` and `quit` bindings are added (a commented-out or removed one is left alone).

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
| `action` | `"float"`: never tile this window. `"tile"`: tile it even if `media_windows = "float"` would leave it floating. |
| `workspace` | `1`–`9`: new windows of this app open on that workspace. |

A rule needs at least one of `process`, `class`, `title`, and all given fields must match.
Use `tile-rs.exe list` to see what the windows on your screen are called.

Windows that are never tiled regardless of rules: invisible or minimized windows, dialogs
(owned windows), tool windows, windows that cannot be resized, the taskbar and desktop, and
windows you have maximized yourself.

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
