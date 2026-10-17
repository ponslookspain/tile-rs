//! Pure geometry of the layout: no Windows calls, so everything here is unit-tested.
//!
//! The spiral: window 0 takes the left part of the screen, window 1 the top part of what is
//! left, window 2 the left part of the rest, and so on (strictly alternating). Every split has
//! a ratio, which the user changes by resizing windows.

use std::collections::HashMap;

use windows::Win32::Foundation::RECT;

use crate::config::{self, Dir};

pub const MIN_RATIO: f32 = 0.12;
pub const MAX_RATIO: f32 = 0.88;
/// Pixels an edge must move before it counts as "the user is resizing".
pub const RESIZE_PX: i32 = 6;

/// Spiral / dwindle: the first window takes the left half of the free area, the next takes
/// the top half of what is left, then left again, and so on (strict alternation); the last
/// window takes the remainder.
///
/// `ratios[i]` is the share (0..1) of split `i` that window `i` gets (default 0.5); the user
/// changes them by dragging window edges.
/// Returns (tile of every window, free region that window `i` was cut out of).
pub fn spiral(area: RECT, n: usize, ratios: &[f32]) -> (Vec<RECT>, Vec<RECT>) {
    let gap = config::gap();
    let mut tiles = Vec::with_capacity(n);
    let mut regions = Vec::with_capacity(n);
    if n == 0 {
        return (tiles, regions);
    }
    let mut r = RECT {
        left: area.left + gap,
        top: area.top + gap,
        right: area.right - gap,
        bottom: area.bottom - gap,
    };
    for i in 0..n {
        regions.push(r);
        if i == n - 1 {
            tiles.push(r);
            break;
        }
        let ratio = ratios
            .get(i)
            .copied()
            .unwrap_or(0.5)
            .clamp(MIN_RATIO, MAX_RATIO);
        let horizontal = i % 2 == 0; // strict alternation: right, down, right, down, ...
        if horizontal {
            let mid = r.left + ((r.right - r.left - gap) as f32 * ratio) as i32;
            tiles.push(RECT {
                left: r.left,
                top: r.top,
                right: mid,
                bottom: r.bottom,
            });
            r.left = mid + gap;
        } else {
            let mid = r.top + ((r.bottom - r.top - gap) as f32 * ratio) as i32;
            tiles.push(RECT {
                left: r.left,
                top: r.top,
                right: r.right,
                bottom: mid,
            });
            r.top = mid + gap;
        }
    }
    (tiles, regions)
}

/// The user dragged edges of window `k`; turn that into new split ratios.
/// In the spiral, window `k`'s edges belong to these splits (j = split index):
///   k even: left -> j=k-2, top -> j=k-1, right -> j=k      (bottom is the screen edge)
///   k odd : left -> j=k-1, top -> j=k-2, bottom -> j=k     (right is the screen edge)
/// Returns true if any ratio changed.
pub fn apply_resize(
    ratios: &mut Vec<f32>,
    tiles: &[RECT],
    regions: &[RECT],
    k: usize,
    new: RECT,
) -> bool {
    let n = tiles.len();
    if k >= n || regions.len() != n {
        return false;
    }
    let gap = config::gap();
    let t = tiles[k];
    if ratios.len() < n.saturating_sub(1) {
        ratios.resize(n.saturating_sub(1), 0.5);
    }
    let moved = |a: i32, b: i32| (a - b).abs() > 3;
    let mut changed = false;

    // set the ratio of split j so that window j's far edge (right for even j, bottom for odd j)
    // lands at `edge`
    let mut set = |j: usize, edge: i32, ratios: &mut Vec<f32>| {
        if j >= n - 1 {
            return;
        }
        let q = regions[j];
        let r = if j.is_multiple_of(2) {
            (edge - q.left) as f32 / ((q.right - q.left - gap).max(1)) as f32
        } else {
            (edge - q.top) as f32 / ((q.bottom - q.top - gap).max(1)) as f32
        }
        .clamp(MIN_RATIO, MAX_RATIO);
        if (r - ratios[j]).abs() > 0.002 {
            ratios[j] = r;
            changed = true;
        }
    };

    if k.is_multiple_of(2) {
        if k >= 2 && moved(new.left, t.left) {
            set(k - 2, new.left - gap, ratios);
        }
        if k >= 1 && moved(new.top, t.top) {
            set(k - 1, new.top - gap, ratios);
        }
        if k < n - 1 && moved(new.right, t.right) {
            set(k, new.right, ratios);
        }
    } else {
        if moved(new.left, t.left) {
            set(k - 1, new.left - gap, ratios);
        }
        if k >= 3 && moved(new.top, t.top) {
            set(k - 2, new.top - gap, ratios);
        }
        if k < n - 1 && moved(new.bottom, t.bottom) {
            set(k, new.bottom, ratios);
        }
    }
    changed
}

fn tiles_at_least(tiles: &[RECT], min_w: i32, min_h: i32) -> bool {
    tiles
        .iter()
        .all(|t| (t.right - t.left) >= min_w && (t.bottom - t.top) >= min_h)
}

/// Like `spiral`, but keeps every tile usable:
///  * the first (master) split is narrowed (down to a floor) so the rest of the screen has more
///    room, which keeps the smallest tiles from collapsing when there are many windows;
///  * the tiles of windows that cannot shrink below their own minimum size (`mins`) are grown, the
///    neighbours give up the space instead, so such a window does not overlap them.
///
/// `min_w`/`min_h` is the comfortable minimum tile; `mins` maps a window handle to the minimum
/// size that window's application enforces.
pub fn spiral_fitted(
    area: RECT,
    windows: &[isize],
    ratios: &[f32],
    mins: &HashMap<isize, (i32, i32)>,
    min_w: i32,
    min_h: i32,
) -> (Vec<RECT>, Vec<RECT>) {
    let n = windows.len();
    let mut rs = ratios.to_vec();
    rs.resize(n.saturating_sub(1), 0.5);

    // narrow the master tile while that makes the other tiles fit the comfortable minimum; the
    // master itself never goes below `min_w`
    if n >= 2 {
        let gap = config::gap();
        let master_span = (area.right - area.left - 2 * gap - gap).max(1);
        let floor = (min_w as f32 / master_span as f32).clamp(MIN_RATIO, 0.5);
        let mut r0 = rs[0].clamp(floor, MAX_RATIO);
        while r0 > floor {
            let mut trial = rs.clone();
            trial[0] = r0;
            if tiles_at_least(&spiral(area, n, &trial).0, min_w, min_h) {
                break;
            }
            r0 = (r0 - 0.02).max(floor);
        }
        rs[0] = r0;
    }

    let mut out = spiral(area, n, &rs);
    // each pass grows the tiles that are still too small; a few passes are enough in practice
    for _ in 0..4 {
        let (tiles, regions) = (out.0.clone(), out.1.clone());
        let mut changed = false;
        for (k, h) in windows.iter().enumerate() {
            let Some(&(mw, mh)) = mins.get(h) else {
                continue;
            };
            let t = tiles[k];
            let dw = (mw - (t.right - t.left)).max(0);
            let dh = (mh - (t.bottom - t.top)).max(0);
            if dw == 0 && dh == 0 {
                continue;
            }
            // move only the edges that actually belong to a split of this window
            let last = k == n - 1;
            let mut d = t;
            if dw > 0 {
                if k.is_multiple_of(2) {
                    if last {
                        d.left -= dw;
                    } else {
                        d.right += dw;
                    }
                } else {
                    d.left -= dw;
                }
            }
            if dh > 0 {
                if k % 2 == 1 {
                    if !last {
                        d.bottom += dh;
                    } else if k >= 3 {
                        d.top -= dh;
                    }
                } else if k >= 1 {
                    d.top -= dh;
                }
            }
            if apply_resize(&mut rs, &tiles, &regions, k, d) {
                changed = true;
            }
        }
        if !changed {
            break;
        }
        out = spiral(area, n, &rs);
    }
    out
}

/// Which split controls the width (or height) of window `k` among `n` windows, and in which
/// direction: returns (split index, +1 if raising that ratio makes the window larger, else -1).
///   width : k even, not last -> own split k (right edge moves)   k even, last -> split k-2
///           k odd            -> split k-1 (left edge moves)
///   height: k odd, not last  -> own split k (bottom edge moves)  k odd, last   -> split k-2
///           k even >= 2      -> split k-1 (top edge moves)
pub fn split_for(k: usize, n: usize, width: bool) -> Option<(usize, f32)> {
    if n < 2 || k >= n {
        return None;
    }
    let last = k == n - 1;
    let r = if width {
        match (k.is_multiple_of(2), last) {
            (true, false) => (k, 1.0),
            (true, true) if k >= 2 => (k - 2, -1.0),
            (false, _) => (k - 1, -1.0),
            _ => return None,
        }
    } else {
        match (k % 2 == 1, last) {
            (true, false) => (k, 1.0),
            (true, true) if k >= 3 => (k - 2, -1.0),
            (false, _) if k >= 2 => (k - 1, -1.0),
            _ => return None,
        }
    };
    Some(r)
}

/// Nearest tiled neighbour of `cur` in direction `dir`, judged by the centres of the rects.
pub fn neighbour(rects: &[RECT], cur: usize, dir: Dir) -> Option<usize> {
    let c = |r: &RECT| {
        (
            (r.left + r.right) as f64 / 2.0,
            (r.top + r.bottom) as f64 / 2.0,
        )
    };
    let (cx, cy) = c(&rects[cur]);
    let mut best: Option<(usize, f64)> = None;
    for (j, r) in rects.iter().enumerate() {
        if j == cur {
            continue;
        }
        let (x, y) = c(r);
        let (dx, dy) = (x - cx, y - cy);
        let (along, across) = match dir {
            Dir::Left => (-dx, dy.abs()),
            Dir::Right => (dx, dy.abs()),
            Dir::Up => (-dy, dx.abs()),
            Dir::Down => (dy, dx.abs()),
        };
        if along <= 1.0 {
            continue; // not in that direction
        }
        let score = along + 2.0 * across;
        if best.is_none_or(|(_, s)| score < s) {
            best = Some((j, score));
        }
    }
    best.map(|(j, _)| j)
}

/// A key held down repeats faster than a person taps it: use smaller steps then.
pub fn effective_step(step: i32, held: bool) -> i32 {
    if held {
        (step / 3).max(4)
    } else {
        step
    }
}

/// Fast start, gentle stop.
pub fn ease_out_cubic(t: f32) -> f32 {
    let u = 1.0 - t.clamp(0.0, 1.0);
    1.0 - u * u * u
}

/// Easing of the window animations (`config.toml` -> `easing`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Easing {
    Linear,
    OutQuad,
    #[default]
    OutCubic,
    OutQuart,
    InOutCubic,
}

/// Applies the easing curve to a normalized time `t` (clamped to 0..1). `ease(_, 0) = 0` and
/// `ease(_, 1) = 1` for every curve.
pub fn ease(e: Easing, t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    match e {
        Easing::Linear => t,
        Easing::OutQuad => 1.0 - (1.0 - t) * (1.0 - t),
        Easing::OutCubic => ease_out_cubic(t),
        Easing::OutQuart => {
            let u = 1.0 - t;
            1.0 - u * u * u * u
        }
        Easing::InOutCubic => {
            if t < 0.5 {
                4.0 * t * t * t
            } else {
                1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
            }
        }
    }
}

/// Duration of an animation for a window that has to travel `dist_px` (the largest edge move).
/// Short trips are quicker; a full reflow keeps the configured duration. `base_ms` is
/// `animation_ms` from the config.
pub fn anim_duration(base_ms: u32, dist_px: i32) -> u32 {
    if base_ms == 0 {
        return 0;
    }
    let f = (dist_px.max(0) as f32 / 400.0).clamp(0.45, 1.0);
    (base_ms as f32 * f).round() as u32
}

/// What to do with a tracked fullscreen window when its workspace is shown or hidden.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FsAction {
    /// Restore it (it was minimized by us while its workspace was inactive).
    Show,
    /// Minimize it (its workspace is not the current one).
    Hide,
    /// Leave it as it is.
    None,
}

/// Decides the visibility action for a fullscreen window. `hidden` = we minimized it earlier
/// (so we may restore it); `minimized` = it is minimized right now (by us or by the user).
pub fn fullscreen_action(ws: u8, cur: u8, hidden: bool, minimized: bool) -> FsAction {
    if ws == cur {
        if hidden {
            FsAction::Show
        } else {
            FsAction::None
        }
    } else if !hidden && !minimized {
        FsAction::Hide
    } else {
        FsAction::None
    }
}

pub fn lerp_i(a: i32, b: i32, t: f32) -> i32 {
    a + ((b - a) as f32 * t).round() as i32
}

pub fn lerp_rect(a: &RECT, b: &RECT, t: f32) -> RECT {
    RECT {
        left: lerp_i(a.left, b.left, t),
        top: lerp_i(a.top, b.top, t),
        right: lerp_i(a.right, b.right, t),
        bottom: lerp_i(a.bottom, b.bottom, t),
    }
}

pub fn same_rect(a: &RECT, b: &RECT, tol: i32) -> bool {
    (a.left - b.left).abs() <= tol
        && (a.top - b.top).abs() <= tol
        && (a.right - b.right).abs() <= tol
        && (a.bottom - b.bottom).abs() <= tol
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: RECT = RECT {
        left: 0,
        top: 0,
        right: 3440,
        bottom: 1392,
    };

    fn layout(n: usize, ratios: &[f32]) -> (Vec<RECT>, Vec<RECT>) {
        spiral(AREA, n, ratios)
    }

    /// Drag one edge of window k to `v`, rebuild the layout, return the new tiles.
    fn drag(n: usize, k: usize, edit: impl Fn(&mut RECT)) -> Vec<RECT> {
        let (tiles, regions) = layout(n, &[]);
        let mut new = tiles[k];
        edit(&mut new);
        let mut ratios = vec![0.5; n - 1];
        assert!(
            apply_resize(&mut ratios, &tiles, &regions, k, new),
            "ratios should change"
        );
        layout(n, &ratios).0
    }

    fn near(a: i32, b: i32) -> bool {
        (a - b).abs() <= 2
    }

    #[test]
    fn default_layout_is_a_spiral() {
        let (t, _) = layout(3, &[]);
        assert_eq!(t.len(), 3);
        // window 0: left half; window 1: top-right; window 2: bottom-right
        assert!(t[0].right < t[1].left);
        assert!(t[1].bottom < t[2].top);
        assert_eq!(t[1].left, t[2].left);
        assert_eq!(t[1].right, t[2].right);
    }

    #[test]
    fn dragging_own_right_edge_of_first_window() {
        let t = drag(3, 0, |r| r.right = 2000);
        assert!(near(t[0].right, 2000), "{:?}", t[0]);
    }

    #[test]
    fn a_minimum_size_grows_the_tile_and_pushes_neighbours() {
        let windows = [10isize, 20, 30];
        let (base, _) = spiral(AREA, 3, &[]);
        let w0 = base[0].right - base[0].left;
        let min_w = w0 + 200;
        let mut mins = HashMap::new();
        mins.insert(10isize, (min_w, 0));
        let (t, _) = spiral_fitted(AREA, &windows, &[], &mins, 0, 0);
        assert!(
            (t[0].right - t[0].left) >= min_w - 4,
            "tile 0 grew to fit its minimum: {:?}",
            t[0]
        );
        // the layout is still a valid, non-overlapping spiral
        assert!(t[0].right <= t[1].left, "{t:?}");
        assert!(t[1].bottom <= t[2].top, "{t:?}");
        assert_eq!(t[1].left, t[2].left);
        assert_eq!(t[1].right, t[2].right);
        // without a minimum and no comfortable minimum the layout is unchanged
        let (same, _) = spiral_fitted(AREA, &windows, &[], &HashMap::new(), 0, 0);
        let (plain, _) = spiral(AREA, 3, &[]);
        assert_eq!(same, plain);
    }

    #[test]
    fn the_master_tile_is_narrowed_so_every_tile_fits_the_comfortable_minimum() {
        // many windows: with equal halves the last tiles are narrower than the comfortable minimum
        let windows: Vec<isize> = (0..6).map(|i| 100 + i).collect();
        let (plain, _) = spiral(AREA, 6, &[]);
        let (fitted, _) = spiral_fitted(AREA, &windows, &[], &HashMap::new(), 480, 320);
        assert!(
            !tiles_at_least(&plain, 480, 320),
            "with equal halves a tile is too narrow: {plain:?}"
        );
        assert!(tiles_at_least(&fitted, 480, 320), "{fitted:?}");
        // the master is narrower than in the plain layout, the rest is wider
        assert!(fitted[0].right < plain[0].right);
        // still a valid, non-overlapping spiral
        assert!(fitted[0].right <= fitted[1].left, "{fitted:?}");
    }

    #[test]
    fn a_minimum_size_of_a_last_window_grows_it_to_the_left() {
        let windows = [10isize, 20, 30];
        let (base, _) = spiral(AREA, 3, &[]);
        let w2 = base[2].right - base[2].left;
        let min_w = w2 + 150;
        let mut mins = HashMap::new();
        mins.insert(30isize, (min_w, 0)); // window 2 is the last one
        let (t, _) = spiral_fitted(AREA, &windows, &[], &mins, 0, 0);
        assert!(
            (t[2].right - t[2].left) >= min_w - 4,
            "last tile grew to the left: {:?}",
            t[2]
        );
        assert!(t[0].right <= t[1].left, "{t:?}");
        assert!(t[1].bottom <= t[2].top, "{t:?}");
    }

    #[test]
    fn dragging_left_edge_of_second_window_moves_first_split() {
        let t = drag(3, 1, |r| r.left = 1500);
        assert!(near(t[1].left, 1500), "{:?}", t[1]);
        assert!(
            near(t[2].left, 1500),
            "bottom-right moves with it: {:?}",
            t[2]
        );
    }

    #[test]
    fn dragging_bottom_edge_of_second_window() {
        let t = drag(3, 1, |r| r.bottom = 900);
        assert!(near(t[1].bottom, 900), "{:?}", t[1]);
        assert!(t[2].top > t[1].bottom);
    }

    #[test]
    fn dragging_top_and_left_edges_of_last_window() {
        let t = drag(3, 2, |r| r.top = 800);
        assert!(near(t[2].top, 800), "{:?}", t[2]);
        let t = drag(3, 2, |r| r.left = 1900);
        assert!(near(t[2].left, 1900), "{:?}", t[2]);
    }

    #[test]
    fn dragging_the_screen_edge_changes_nothing() {
        // last window, right/bottom edges are screen edges
        let (tiles, regions) = layout(3, &[]);
        let mut new = tiles[2];
        new.right -= 200;
        new.bottom -= 200;
        let mut ratios = vec![0.5; 2];
        assert!(!apply_resize(&mut ratios, &tiles, &regions, 2, new));
    }

    #[test]
    fn ratios_are_clamped() {
        let t = drag(2, 0, |r| r.right = 3400);
        let (full, _) = layout(2, &[]);
        assert!(t[0].right > full[0].right);
        assert!(
            t[1].right - t[1].left > 200,
            "other window keeps a sane minimum width"
        );
    }

    #[test]
    fn easing_is_monotonic_and_hits_the_ends() {
        assert_eq!(ease_out_cubic(0.0), 0.0);
        assert_eq!(ease_out_cubic(1.0), 1.0);
        assert_eq!(ease_out_cubic(2.0), 1.0, "clamped above 1");
        assert_eq!(ease_out_cubic(-1.0), 0.0, "clamped below 0");
        let mut prev = 0.0;
        for i in 1..=100 {
            let v = ease_out_cubic(i as f32 / 100.0);
            assert!(v >= prev, "not monotonic at {i}");
            prev = v;
        }
        // fast start: more than half of the way after a quarter of the time
        assert!(ease_out_cubic(0.25) > 0.5);
    }

    #[test]
    fn easing_curves_are_monotonic_and_hit_the_ends() {
        let curves = [
            Easing::Linear,
            Easing::OutQuad,
            Easing::OutCubic,
            Easing::OutQuart,
            Easing::InOutCubic,
        ];
        for e in curves {
            assert_eq!(ease(e, 0.0), 0.0, "{e:?} at 0");
            assert_eq!(ease(e, 1.0), 1.0, "{e:?} at 1");
            assert_eq!(ease(e, -1.0), 0.0, "{e:?} clamped below 0");
            assert_eq!(ease(e, 2.0), 1.0, "{e:?} clamped above 1");
            let mut prev = 0.0;
            for i in 1..=100 {
                let v = ease(e, i as f32 / 100.0);
                assert!(v >= prev - 1e-6, "{e:?} not monotonic at {i}");
                prev = v;
            }
        }
    }

    #[test]
    fn anim_duration_scales_with_distance() {
        assert_eq!(anim_duration(0, 1000), 0, "0 ms stays 0 ms");
        // short trip: the lower bound (45% of the base)
        assert_eq!(anim_duration(200, 0), 90);
        // a full-screen reflow keeps the base duration
        assert_eq!(anim_duration(200, 400), 200);
        assert_eq!(anim_duration(200, 10_000), 200, "clamped at the base");
        // negative distance is treated as zero
        assert_eq!(anim_duration(200, -50), 90);
        // monotonic in distance
        let mut prev = 0;
        for d in 0..=500 {
            let v = anim_duration(200, d);
            assert!(v >= prev, "not monotonic at {d}");
            prev = v;
        }
        // monotonic in the base duration
        assert!(anim_duration(200, 400) > anim_duration(100, 400));
    }

    #[test]
    fn fullscreen_action_visibility() {
        // on the current workspace
        assert_eq!(fullscreen_action(1, 1, false, false), FsAction::None); // visible, leave it
        assert_eq!(fullscreen_action(1, 1, true, true), FsAction::Show); // we hid it: restore
        assert_eq!(fullscreen_action(1, 1, false, true), FsAction::None); // user minimized it
                                                                          // on another workspace
        assert_eq!(fullscreen_action(1, 2, false, false), FsAction::Hide); // visible: minimize
        assert_eq!(fullscreen_action(1, 2, true, true), FsAction::None); // already hidden by us
        assert_eq!(fullscreen_action(1, 2, false, true), FsAction::None); // minimized: leave it
    }

    #[test]
    fn lerp_rect_interpolates_and_ends_exactly() {
        let a = RECT {
            left: 0,
            top: 0,
            right: 100,
            bottom: 50,
        };
        let b = RECT {
            left: 200,
            top: 100,
            right: 400,
            bottom: 250,
        };
        let m = lerp_rect(&a, &b, 0.5);
        assert_eq!((m.left, m.top, m.right, m.bottom), (100, 50, 250, 150));
        let e = lerp_rect(&a, &b, 1.0);
        assert!(same_rect(&e, &b, 0));
        let s0 = lerp_rect(&a, &b, 0.0);
        assert!(same_rect(&s0, &a, 0));
    }

    #[test]
    fn same_rect_tolerance() {
        let a = RECT {
            left: 10,
            top: 10,
            right: 110,
            bottom: 60,
        };
        let mut b = a;
        b.right += 1;
        assert!(same_rect(&a, &b, 1));
        assert!(!same_rect(&a, &b, 0));
    }

    #[test]
    fn held_keys_use_smaller_steps() {
        assert_eq!(effective_step(40, false), 40);
        assert_eq!(effective_step(40, true), 13);
        assert_eq!(effective_step(6, true), 4, "never below 4 px");
        assert!(effective_step(500, true) < 500);
    }

    #[test]
    fn split_for_width_and_height() {
        // 3 windows: 0 = left half, 1 = top right, 2 = bottom right
        assert_eq!(split_for(0, 3, true), Some((0, 1.0)));
        assert_eq!(split_for(1, 3, true), Some((0, -1.0)));
        assert_eq!(split_for(2, 3, true), Some((0, -1.0)));
        assert_eq!(split_for(0, 3, false), None); // full height already
        assert_eq!(split_for(1, 3, false), Some((1, 1.0)));
        assert_eq!(split_for(2, 3, false), Some((1, -1.0)));
        assert_eq!(split_for(0, 1, true), None); // a single window
        assert_eq!(split_for(5, 3, true), None); // out of range
    }

    #[test]
    fn grow_width_really_widens_the_window() {
        for n in 2..=6usize {
            let (base, _) = layout(n, &[]);
            for k in 0..n {
                let Some((j, sign)) = split_for(k, n, true) else {
                    continue;
                };
                let mut ratios = vec![0.5; n - 1];
                ratios[j] = (ratios[j] + 0.08 * sign).clamp(MIN_RATIO, MAX_RATIO);
                let (t, _) = layout(n, &ratios);
                let (w0, w1) = (base[k].right - base[k].left, t[k].right - t[k].left);
                assert!(w1 > w0, "n={n} k={k}: width {w0} -> {w1}");
            }
        }
    }

    #[test]
    fn grow_height_really_heightens_the_window() {
        for n in 3..=6usize {
            let (base, _) = layout(n, &[]);
            for k in 0..n {
                let Some((j, sign)) = split_for(k, n, false) else {
                    continue;
                };
                let mut ratios = vec![0.5; n - 1];
                ratios[j] = (ratios[j] + 0.08 * sign).clamp(MIN_RATIO, MAX_RATIO);
                let (t, _) = layout(n, &ratios);
                let (h0, h1) = (base[k].bottom - base[k].top, t[k].bottom - t[k].top);
                assert!(h1 > h0, "n={n} k={k}: height {h0} -> {h1}");
            }
        }
    }

    #[test]
    fn four_windows_chain() {
        // window 3 (odd): its left edge belongs to split 2
        let t = drag(4, 3, |r| r.left += 150);
        let (base, _) = layout(4, &[]);
        assert!(t[3].left > base[3].left);
    }
}
