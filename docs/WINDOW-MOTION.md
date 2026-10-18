# Window motion and snapshot resize

`snapshot_animations = true` is enabled by default and exposed as **Растягивать снимки окон** in
the launcher's animation settings. `animations` and `animation_ms` control timed transitions;
snapshot preview during manual resize is controlled independently by `snapshot_animations`.

## Dragging a tile divider

With snapshots enabled, Spiral and Master/stack expose a mouse handle inside each adjustable
gap. Hover over the gap until the resize cursor and blue highlight appear, then drag with the
left mouse button. The handle is up to 8 physical pixels wide and stays inside the configured
gap. A zero inner gap disables these handles. Grid and Columns retain equal-size tiles.

During this gesture all actual application windows retain their sizes. Frozen images follow
the latest pointer position on the existing high-resolution frame scheduler; intermediate
pointer events are not queued as animations. Geometry is always derived from the initial
layout, and all affected tiles are constrained by their known minimum sizes. Release applies
the targets as a batch and briefly retains the preview while applications start repainting.

Escape, capture loss, a workspace/configuration/display change, or a window appearing,
disappearing or unexpectedly moving cancels the gesture. Original geometry and workspace
ratios are retained on cancellation. Ordinary window-edge resize remains available. If the
screen cannot be captured safely (occlusion, overlapping sources, capture budget), the divider
does not start; use the application's own edge instead.

This mode uses the existing Direct2D/GDI renderer. It does not cloak foreign windows or depend
on undocumented DWM interfaces. First-gesture capture/upload and the final repaint still have
costs; this implementation does not promise that every application repaints within one frame.

## Implementation

- Capture visible monitor pixels once at the beginning of an eligible resize/transition. Images
  stay in memory, are never saved, and are released on completion or cancellation.
- A tool overlay passes pointer input to the real windows. Its background is dark while the
  frozen tiles move. The monitor's taskbar and workspace dots are outside the scene.
- Upload the image once to a hardware Direct2D target. Subsequent frames transform image regions
  with linear interpolation. If target creation or drawing fails, use the GDI implementation.
- During a native mouse resize the selected application still receives Windows resize messages;
  its content changes behind the snapshot. Neighbouring applications retain their sizes until
  release. No process is suspended and no foreign window receives `WM_SETREDRAW`.
- Commit neighbour geometry through `DeferWindowPos`. Individual bounded placements recover from
  batch failure. Verify settled geometry and observe application minimum sizes afterwards.
- Resize hotkeys and tile swaps do not capture the screen: single resize taps use ordinary
  geometry animation, while held keys commit all affected windows together immediately.
  General layout reconciliation only reuses an existing mouse-resize scene; it does not
  synchronously capture and upload the monitor for each rearrangement. New captured windows fade in.
  Closing a window animates the remaining layout; a destroyed window has no exit image unless
  a scene already captured it. There is no continuous background capture to manufacture one.
- Workspace switches commit the actual workspace immediately, then show a short directional fade
  of the outgoing scene. Repeated switches cancel the previous scene.

## Adversarial review by stage

| Stage | Failure considered | Implemented response |
| --- | --- | --- |
| Motion lifecycle | A parked window's old animation brings it back | Cancel the monitor's old queue; validate workspace and drag ownership before every frame |
| Retargeting | Pointer events restart an unchanged animation | Keep an identical target and its original start time |
| Geometry transactions | One member rejects a batch, or stale DWM bounds look like a huge border | Bounded fallback and verification; validate border offsets |
| Resize preview | A neighbour is destroyed or a new window appears during drag | Cancel the stale scene and defer whole-layout reconciliation until the gesture ends |
| Event ownership | A global quiet period hides another window's genuine movement | Compare the most recent expected frame independently for each HWND |
| Callback reentrancy | Foreign window calls deliver another WinEvent while state is borrowed | Queue ordered events and replay after the transaction |
| GPU lifecycle | Initialization fails or the device is lost | Retain the CPU snapshot and switch to GDI for that scene |
| Capture validity | A popup or another window covers a source | Check Z order and rectangles; use ordinary placement instead of capturing the covering window |
| Memory | Many monitors or very large desktops allocate excessive captures | Limit each scene to 16 million pixels and mapped scenes together to 24 million; fall back above the limit |
| Timing | Missing/failed waitable timer or changed display refresh rate | Cancellable thread-timer fallback; refresh cadence for each motion burst |
| Cleanup | Pause, focus change, display changes, config reload, shutdown | Discard the affected scenes and release their window, bitmap, DC and GPU resources |
| Divider ownership | Verification, focus-follows-mouse or a hotkey fights a gesture | Pause verification during capture, disable focus-follow during a gesture and cancel before commands |
| Divider cancellation | Capture-loss callback arrives during a State borrow | Release capture immediately and finish cancellation on the next frame after the transaction |
| Divider constraints | Fast reversal drifts ratios or shrinks a descendant below its minimum | Derive every target from initial geometry and clamp against all affected tiles |

## Practical limits

This implementation captures visible screen pixels, not private application surfaces. Overlapping
source rectangles, occluding windows and partially off-screen sources disable the scene. This
often applies while a new window initially overlaps the existing layout. Protected content can
appear black and should use the ordinary mode. HDR colour fidelity is not guaranteed by the SDR
screen capture path.

GPU copies and the temporary pixel upload buffer consume memory in addition to the CPU bitmap
budget above. Scheduling uses refresh-derived timers, not a guarantee of display-vsync alignment.
Application-enforced minimum sizes are still handled most fully by the spiral layout; the other
layouts retain their existing constraints. The native selected window still performs its own
layout work. A future custom divider gesture could eliminate that work too.

## Verification

Automated checks cover geometry/configuration, hidden-workspace/pause/drag guards, unchanged
animation targets, independent expected geometry, destroyed-window cleanup, queued reentrant
destroy events, timer fallback and capture rejection. The autostart registry test is unrelated
and is excluded from the focused motion test run:

```powershell
cargo test --offline --bin tile-rs -- --skip autostart_registry_roundtrip
cargo clippy --offline --all-targets -- -D warnings
```

Manual visual verification is still required. It has not been replaced by the unit tests:

1. Browser, Explorer, Telegram and an editor: slow/fast resize, rapid direction reversals, all
   three workspace-dot sizes, and both spiral/master-stack layouts.
2. Open/close a neighbour during resize; drag a window to another monitor; open a modal dialog;
   Alt+Tab away while a transition runs; pause and resume.
3. Repeatedly switch workspaces and move the focused window between them during a transition.
4. Mixed-DPI monitors at 60 and 144+ Hz; disconnect/reconnect a monitor; compare the ordinary mode.
5. GPU initialization/device-loss fallback, protected video and HDR desktops.
6. Drag a gap in Spiral and Master/stack, reverse rapidly, release beyond the monitor, and
   press Escape. Open/close a window during the gesture. Compare gap dragging with native edge
   resizing; verify keyboard resizing still responds immediately while held.

Set `TILE_RS_PROFILE=1` before starting tile-rs to record layout/batch times, cumulative real-window
placement counts, selected snapshot renderer, scene frame counts and average rendering time in
the existing log. These are instrumentation, not a claim that desktop performance was measured.
