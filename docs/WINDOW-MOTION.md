# Live window motion and resize

All tile resizing uses real application windows. There are no screenshots, frozen previews,
capture sessions or GPU image uploads. Applications reflow and repaint their own content as their
native window sizes change. Layout changes use ordinary geometry animations; switching workspace
slides the incoming windows a short way in (4% of the monitor width, at most 80 px, position only,
up to six windows). Chromium and Electron windows can keep drawing their content at an old spot
after a move without a size change, so every slid window gets a frame recalculation and a full
invalidation (both posted, never waited for) when it arrives.

`live_resize` chooses what a separator or edge drag does to the other windows: `live` resizes
them while the pointer moves, `preview` shows translucent outlines of the new tiles and glides the
windows into them once on release, and `auto` (default) is live up to five windows on the monitor
and outlines above that or while animations are suspended on a slow machine.

## Dragging a tile divider

Spiral and Master/stack expose a mouse handle inside each adjustable gap when `inner_gap > 0`.
Hover over the gap until the resize cursor and blue highlight appear, then drag with the left
mouse button. The handle is up to 8 physical pixels wide and stays inside the gap. The captured
handle follows the split without losing capture; other handles are hidden until release.
Grid and Columns retain equal-size tiles. Native window edges remain available in every layout.
Neither `animations` nor the old `snapshot_animations` switch disables divider dragging.

A gesture derives every target from its original geometry. Known minimum window sizes constrain
all affected descendants. Ratios are committed on release; Escape, capture loss, a workspace,
configuration or display change, or an incompatible window event cancels the gesture and restores
surviving windows to their original geometry. Hidden, minimized or departed windows are not restored.

## Scheduling and responsiveness

- The refresh-derived frame timer samples the latest pointer position. Intermediate mouse events
  are not queued as geometry animations. `animation_fps_limit` also caps live resize (60 by default).
- Each affected window has at most one unsent target, replaced by newer input. Unchanged windows
  are skipped. A client whose previous request is not yet reflected in observed bounds is given
  up to 90 ms before another request; responsive windows can continue at the normal frame rate.
  This limits submissions to slow clients, but does not cancel requests already posted to Windows
  or guarantee that an unresponsive application's message queue remains empty.
- During a divider drag the windows bordering the dragged separator move together: while one of
  them has not applied its previous size (for at most 34 ms, two frames), its partners wait, so
  the separator never opens into a gap or an overlap. Windows away from the separator are resized
  at most every 33 ms. The pointer moves in steps of 4 px at 96 DPI (scaled with the monitor);
  release uses the exact position.
- A frame sends one `SetWindowPos` request per window, back to back, shrinking windows first so
  a tile vacates its space before its neighbour grows into it. Every request uses
  `SWP_ASYNCWINDOWPOS`; the manager does not wait for a foreign UI thread to finish processing a
  resize. `DeferWindowPos` is not used: it rejects `SWP_ASYNCWINDOWPOS` (`ERROR_INVALID_PARAMETER`),
  and a synchronous batch would let a hung application block the manager. No process is suspended
  or sent `WM_SETREDRAW`.
- Release discards unsent intermediate targets and immediately submits the final geometry,
  bypassing the interactive throttle. With outlines the windows glide into their final tiles.
- A layout change that arrives while windows are still gliding keeps their speed: the new glide
  starts with the current velocity of every edge (capped so it never overshoots) instead of
  restarting the easing curve.
- Separator highlights fade in and out on hover (they are tile-rs's own windows).
- During native edge resizing Windows owns the selected window; tile-rs updates its neighbours.
  The selected application's own native resize loop is outside tile-rs's frame limit.
- During a divider gesture, a settled larger size can raise the relevant minimum and clamp the
  split without cancellation. Feedback requires observed progress; an unchanged busy window is
  not learned as a minimum. Full adaptive fitting and corrective verification resume after release. A posted
  request is not evidence of its completion: a settled larger frame must also agree with the client's
  explicit tracking minimum before it can raise a learned limit. A timed-out client query receives
  up to two seconds to recover; the query itself waits at most 10 ms per attempt. Geometry retries
  remain bounded per unchanged target. Repeatedly rejecting clients float with a recorded reason.
- Delayed location events and application-enforced sizes do not cancel divider capture. Explicit
  native move/resize starts and lifecycle changes still cancel it. Input surfaces are clipped against
  observed window bounds; the active highlight survives the cursor leaving a clamped handle.
- Invisible frame-border offsets are captured once per gesture so lagging DWM bounds cannot
  introduce viewport jitter. Identical native rectangles are skipped; position-only changes use
  `SWP_NOSIZE`, preserving the application's current viewport dimensions.
- The focus border follows observed geometry. Frame timers have a cancellable thread-timer
  fallback, and stop once both gestures and ordinary animations have finished.

Native content is always visible, including video and text. Smoothness depends on the application:
its layout and paint work can take longer than a frame even when tile-rs submits positions quickly.
The manager can coalesce and pace requests, but cannot guarantee 60 FPS in every foreign process.
On a slower computer, try `animation_fps_limit = 30`; turning off `animations` removes timed layout
transitions while retaining live resize. The legacy `snapshot_animations` key is accepted and
ignored so existing configuration files continue to load; it is no longer generated or shown.
Saving settings through the launcher, or choosing Reload config after editing the file, clears
previously learned size constraints before rebuilding the layout. Configured minimums are limits,
not a command to set every window to that size; existing split ratios remain unchanged.

All four layouts reserve configured and confirmed per-window minimums before placing windows.
Admission uses a prospective valid layout, including monitor shape and gaps. If no workspace fits,
the extra window stays floating rather than collapsing its neighbour. Temporary hide, minimize,
maximize and cloak states preserve workspace membership; only available tiled windows occupy a tile.
Native title-change notifications can admit a late-initialized window without reopening it.
See [configuration and window diagnostics](CONFIGURATION.md#window-diagnostics).

A website may rebuild its player when the browser crosses a responsive layout breakpoint. This
also occurs during ordinary browser resizing with tile-rs paused. Live resizing cannot suppress
that page behaviour; keeping the browser above that width (or raising `min_tile_width` for the
layout) avoids crossing the breakpoint. The manager never reloads browser tabs.

## Verification

Automated checks cover latest-target replacement, independent pacing of slow and responsive
windows, separator partners waiting for each other, far-window pacing, stepped live targets,
velocity-preserving retargeting, unchanged geometry, expected-event ownership, cancelled pending motion, geometry ratios,
minimum-size reversal, frame-timer fallback, hidden-workspace guards and deferred event replay.
A Win32 regression test moves off-screen fixture windows, cancels a divider gesture and verifies
that their original native geometry is restored. Another fixture stops pumping messages for
900 ms and verifies that individual and batch submissions do not wait for it.

```powershell
cargo test --offline --bin tile-rs -- --skip autostart_registry_roundtrip
cargo clippy --offline --all-targets -- -D warnings
```

Manual verification remains necessary: browser, Explorer, Telegram and an editor; fast reversals,
very narrow/wide tiles, Escape and capture loss; opening/closing/minimizing a neighbour; switching
workspaces; mixed-DPI displays at 60 and 144+ Hz. Compare the same scenarios at 30 and 60 FPS.
Automated fixtures do not establish actual frame rates or visual smoothness in these applications.

Set `TILE_RS_PROFILE=1` before starting tile-rs to log layout/batch durations, placement counts,
frame intervals and event queue/coalescing counters. Animation percentiles retain at most the latest
4096 samples per metric. See the [performance audit](PERFORMANCE-AUDIT.md) for remaining work.
