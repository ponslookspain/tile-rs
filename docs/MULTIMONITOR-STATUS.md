# Multi-monitor support: implementation status and remaining work

This tracked status document reflects the implementation in `src/`. The original working plan,
`PLAN-multimonitor.md`, remains locally excluded by `.git/info/exclude` and may be stale in other
checkouts.

## Current design

- Each monitor has its own current workspace and tiling order; layout choice remains global.
- A visible window is assigned to the nearest monitor reported by Windows. Its monitor assignment
  is preserved while it is parked off-screen.
- A manual cross-monitor drag moves the window to the destination monitor's current workspace;
  monitor workspaces are remapped by display-device name after topology refresh.
- Workspace hotkeys target the monitor containing the foreground window when known, otherwise the
  monitor under the pointer. Clicking a workspace dot targets the monitor that owns that dot.
- Workspaces are emulated by parking inactive windows at `(-32000, -32000)`; fullscreen windows are
  tracked and minimized/restored separately on their owning monitor.
- At most nine monitor slots are tracked (`MON` in `main.rs`). Monitor-scoped state is matched by
  the Windows display-device name when enumeration changes; visible windows are rechecked against
  the refreshed topology, and parked windows from a removed display move to the nearest survivor.
- Monitor topology and work areas are refreshed at start and on display/work-area notifications.
- One workspace indicator is maintained for every connected monitor. The bar height is removed from
  each monitor's tiling area when the indicator setting is enabled.

## Progress

| Stage | Status | Evidence / remaining work |
|---|---|---|
| M1: monitor discovery and work areas | Implemented; physical hot-plug verification remains | `monitor.rs` reads display-device names, bounds and work areas; `request_relayout()` remaps state after refresh. |
| M2: per-monitor tiling and workspaces | Implemented; physical drag/hot-plug verification remains | `State` keeps per-monitor order, ratios, rectangles and workspace; a cross-monitor drag rehomes to the destination's active workspace. |
| M3: parking and fullscreen tracking | Implemented | Window monitor assignments are retained while parked; fullscreen workspace/monitor state is reconciled and restored independently. |
| M4: per-monitor indicator | Implemented; Windows manual verification remains | `overlay.rs` associates each overlay with a monitor slot; minimal dots show active/occupied workspaces and clicks target that monitor. |
| M5: diagnostics and documentation | Implemented in this change; manual verification remains | `tile-rs.exe list` groups tileable windows by monitor and reports its work area; README and configuration docs describe per-monitor behavior. |

## Remaining work

1. Run the full `./scripts/check.ps1` suite in an ordinary Windows user session. In the current
   managed session, Clippy and a release build succeeded, but the registry round-trip unit test could
   not write its test value (`ERROR_ACCESS_DENIED`).
2. Manually verify on two monitors: independent workspace switching by pointer, clicking workspace dots on
   each display, moving tiled windows between monitors, fullscreen on one monitor while switching
   the other, hot-plug/unplug, and taskbar/work-area changes.
3. Validate mixed-DPI behavior and monitor reordering on real hardware. The `\\.\DISPLAYn` name
   supports enumeration remapping but is not a hardware serial; verify reassignment after cable or
   adapter changes.
4. Record a `TILE_RS_PROFILE=1` pass on a multi-monitor setup if layout responsiveness is in question.

## Known constraints and risks

- Window placement uses global desktop coordinates. Per-monitor DPI awareness is enabled, but
  mixed-DPI and hot-plug behavior still need manual validation.
- Display-device names can be reassigned by Windows after some adapter/topology changes; the
  nearest-monitor fallback and live window positions need physical hot-plug verification.
- The indicator has a fixed capacity of nine, matching `MON`.
- `grid` and `columns` use equal fixed-size tiles; edge resizing is supported by spiral and
  master-stack layouts.

## Verification checklist

- [x] `cargo fmt --all -- --check`
- [x] `cargo clippy --all-targets -- -D warnings`
- [x] `cargo test --all-targets -- --skip autostart_registry_roundtrip` (50 passed)
- [ ] Full registry round-trip test (blocked here by registry test access)
- [x] `cargo build --release --target-dir target/multimon-build`
- [ ] Two-monitor manual scenarios listed above
