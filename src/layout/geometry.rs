//! Pure geometry of the layout: no Windows calls, so everything here is unit-tested.
//!
//! The spiral: window 0 takes the left part of the screen, window 1 the top part of what is
//! left, window 2 the left part of the rest, and so on (strictly alternating). Every split has
//! a ratio, which the user changes by resizing windows.

use std::collections::HashMap;

use windows::Win32::Foundation::RECT;

use crate::config::{Dir, Layout};

pub const MIN_RATIO: f32 = 0.12;
pub const MAX_RATIO: f32 = 0.88;
/// Pixels an edge must move before it counts as "the user is resizing".
pub const RESIZE_PX: i32 = 6;

/// The gaps of a layout: `inner` between neighbouring windows, `outer` around the screen edges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gaps {
    pub inner: i32,
    pub outer: i32,
}

/// A single number means "the same gap everywhere" (keeps older callers and the tests unchanged).
impl From<i32> for Gaps {
    fn from(g: i32) -> Self {
        Gaps { inner: g, outer: g }
    }
}

/// `area` shrunk by `m` pixels on every side.
fn inner(area: RECT, m: i32) -> RECT {
    RECT {
        left: area.left + m,
        top: area.top + m,
        right: area.right - m,
        bottom: area.bottom - m,
    }
}

/// Spiral / dwindle: the first window takes the left half of the free area, the next takes
/// the top half of what is left, then left again, and so on (strict alternation); the last
/// window takes the remainder.
///
/// `ratios[i]` is the share (0..1) of split `i` that window `i` gets (default 0.5); the user
/// changes them by dragging window edges. `gaps` are the gaps (passed in so this module stays
/// pure, with no global state).
/// Returns (tile of every window, free region that window `i` was cut out of).
pub fn spiral(
    area: RECT,
    n: usize,
    ratios: &[f32],
    gaps: impl Into<Gaps>,
) -> (Vec<RECT>, Vec<RECT>) {
    let gaps = gaps.into();
    let mut tiles = Vec::with_capacity(n);
    let mut regions = Vec::with_capacity(n);
    if n == 0 {
        return (tiles, regions);
    }
    let mut r = inner(area, gaps.outer);
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
            let mid = r.left + ((r.right - r.left - gaps.inner) as f32 * ratio) as i32;
            tiles.push(RECT {
                left: r.left,
                top: r.top,
                right: mid,
                bottom: r.bottom,
            });
            r.left = mid + gaps.inner;
        } else {
            let mid = r.top + ((r.bottom - r.top - gaps.inner) as f32 * ratio) as i32;
            tiles.push(RECT {
                left: r.left,
                top: r.top,
                right: r.right,
                bottom: mid,
            });
            r.top = mid + gaps.inner;
        }
    }
    (tiles, regions)
}

/// Places `n` windows on `area` using `layout`. `ratios` is only read by the layouts that split
/// (spiral, master-stack); grid and columns ignore it. Returns (tile of every window, free region
/// that window `i` was cut out of), like `spiral`.
pub fn place(
    layout: Layout,
    area: RECT,
    n: usize,
    ratios: &[f32],
    gaps: impl Into<Gaps>,
) -> (Vec<RECT>, Vec<RECT>) {
    let gaps = gaps.into();
    match layout {
        Layout::Spiral => spiral(area, n, ratios, gaps),
        Layout::MasterStack => master_stack(area, n, ratios, gaps),
        Layout::Grid => grid(area, n, gaps),
        Layout::Columns => columns(area, n, gaps),
    }
}

/// Monocle: every one of the `n` windows gets the whole area (they are stacked on top of each
/// other and the focused one is raised).
pub fn monocle(area: RECT, n: usize, gaps: impl Into<Gaps>) -> (Vec<RECT>, Vec<RECT>) {
    let r = inner(area, gaps.into().outer);
    (vec![r; n], vec![r; n])
}

/// Stacks `count` windows in equal-height bands inside `col`, pushing their tiles and regions.
fn push_bands(
    tiles: &mut Vec<RECT>,
    regions: &mut Vec<RECT>,
    col: RECT,
    region: RECT,
    count: usize,
    gap: i32,
) {
    let band = ((col.bottom - col.top - (count as i32 - 1) * gap).max(1) / count as i32).max(1);
    for i in 0..count {
        let top = col.top + (band + gap) * i as i32;
        let bottom = if i == count - 1 {
            col.bottom
        } else {
            top + band
        };
        tiles.push(RECT { top, bottom, ..col });
        regions.push(region);
    }
}

/// Master-stack: the first `master_count` windows (the masters, 1 by default) share a left column
/// whose width is `ratios[0]` (0.5 by default); the other windows share the right column in
/// equal-height bands. Dragging the master column's right edge changes `ratios[0]`, exactly like
/// the spiral's first split. With no more windows than masters, the masters fill the whole area.
pub fn master_stack(
    area: RECT,
    n: usize,
    ratios: &[f32],
    gaps: impl Into<Gaps>,
) -> (Vec<RECT>, Vec<RECT>) {
    master_stack_with(area, n, ratios, gaps, crate::config::master_count())
}

/// `master_stack` with an explicit number of master windows.
fn master_stack_with(
    area: RECT,
    n: usize,
    ratios: &[f32],
    gaps: impl Into<Gaps>,
    masters: usize,
) -> (Vec<RECT>, Vec<RECT>) {
    let gaps = gaps.into();
    let mut tiles = Vec::with_capacity(n);
    let mut regions = Vec::with_capacity(n);
    if n == 0 {
        return (tiles, regions);
    }
    let r = inner(area, gaps.outer);
    let masters = masters.min(n);
    if n == 1 || masters >= n {
        push_bands(&mut tiles, &mut regions, r, r, n, gaps.inner);
        return (tiles, regions);
    }
    let ratio = ratios
        .first()
        .copied()
        .unwrap_or(0.5)
        .clamp(MIN_RATIO, MAX_RATIO);
    let mid = r.left + ((r.right - r.left - gaps.inner).max(1) as f32 * ratio) as i32;
    let master = RECT { right: mid, ..r };
    let stack = RECT {
        left: mid + gaps.inner,
        ..r
    };
    // every master was cut out of the whole area, every stack window out of the right column
    push_bands(&mut tiles, &mut regions, master, r, masters, gaps.inner);
    push_bands(
        &mut tiles,
        &mut regions,
        stack,
        stack,
        n - masters,
        gaps.inner,
    );
    (tiles, regions)
}

/// Grid: choose the row/column count that best fits the monitor shape while avoiding mostly empty
/// rows. Cells target a comfortable landscape aspect ratio, and the last row/column absorbs pixels
/// left over by integer division.
pub fn grid(area: RECT, n: usize, gaps: impl Into<Gaps>) -> (Vec<RECT>, Vec<RECT>) {
    let gaps = gaps.into();
    let mut tiles = Vec::with_capacity(n);
    if n == 0 {
        return (Vec::new(), Vec::new());
    }
    let r = inner(area, gaps.outer);
    // Compare every possible column count. A shape-only score would choose layouts with a nearly
    // empty last row (e.g. 4 columns for 5 windows), so penalize unused cells as well.
    const TARGET_TILE_ASPECT: f64 = 1.5;
    const EMPTY_CELL_PENALTY: f64 = 2.0;
    let rw = (r.right - r.left).max(1) as f64;
    let rh = (r.bottom - r.top).max(1) as f64;
    let (cols, rows) = (1..=n)
        .map(|cols| {
            let rows = n.div_ceil(cols);
            let cell_w = (rw - gaps.inner as f64 * (cols - 1) as f64).max(1.0) / cols as f64;
            let cell_h = (rh - gaps.inner as f64 * (rows - 1) as f64).max(1.0) / rows as f64;
            let aspect_error = (cell_w / cell_h / TARGET_TILE_ASPECT).log2().abs();
            let empty_fraction = (rows * cols - n) as f64 / n as f64;
            let score = aspect_error + EMPTY_CELL_PENALTY * empty_fraction;
            (cols, rows, score)
        })
        .min_by(|a, b| a.2.total_cmp(&b.2))
        .map(|(cols, rows, _)| (cols, rows))
        .unwrap_or((1, 1));
    let cw = ((r.right - r.left - (cols as i32 - 1) * gaps.inner).max(1) / cols as i32).max(1);
    let ch = ((r.bottom - r.top - (rows as i32 - 1) * gaps.inner).max(1) / rows as i32).max(1);
    for i in 0..n {
        let (row, col) = (i / cols, i % cols);
        let left = r.left + (cw + gaps.inner) * col as i32;
        let top = r.top + (ch + gaps.inner) * row as i32;
        let right = if col == cols - 1 { r.right } else { left + cw };
        let bottom = if row == rows - 1 { r.bottom } else { top + ch };
        tiles.push(RECT {
            left,
            top,
            right,
            bottom,
        });
    }
    (tiles.clone(), tiles)
}

/// Columns: `n` equal-width columns spanning the full height; the last one takes the leftover
/// pixels so the columns exactly cover the area.
pub fn columns(area: RECT, n: usize, gaps: impl Into<Gaps>) -> (Vec<RECT>, Vec<RECT>) {
    let gaps = gaps.into();
    let mut tiles = Vec::with_capacity(n);
    if n == 0 {
        return (Vec::new(), Vec::new());
    }
    let r = inner(area, gaps.outer);
    let cw = ((r.right - r.left - (n as i32 - 1) * gaps.inner).max(1) / n as i32).max(1);
    for i in 0..n {
        let left = r.left + (cw + gaps.inner) * i as i32;
        let right = if i == n - 1 { r.right } else { left + cw };
        tiles.push(RECT {
            left,
            top: r.top,
            right,
            bottom: r.bottom,
        });
    }
    (tiles.clone(), tiles)
}

/// Fits the chosen layout to every application's and the configured minimum size.
/// When those constraints cannot coexist, returns an ordinary non-overlapping layout;
/// callers must use `layout_is_valid` before applying it or admitting another window.
pub fn place_fitted(
    layout: Layout,
    area: RECT,
    windows: &[isize],
    ratios: &[f32],
    mins: &HashMap<isize, (i32, i32)>,
    min: (i32, i32),
    gaps: impl Into<Gaps>,
) -> (Vec<RECT>, Vec<RECT>) {
    let gaps = gaps.into();
    let n = windows.len();
    if layout == Layout::Spiral {
        return spiral_fitted(area, windows, ratios, mins, min.0, min.1, gaps);
    }
    let required = window_minimums(windows, mins, min);
    let r = inner(area, gaps.outer);
    let fitted = match layout {
        Layout::MasterStack if n >= 2 => fitted_master_stack(r, &required, ratios, gaps.inner),
        Layout::Grid if n >= 2 => fitted_grid(r, &required, gaps.inner),
        Layout::Columns if n >= 2 => fitted_columns(r, &required, gaps.inner),
        _ => None,
    };
    fitted.unwrap_or_else(|| safe_fallback(layout, area, n, ratios, gaps))
}

fn window_minimums(
    windows: &[isize],
    mins: &HashMap<isize, (i32, i32)>,
    min: (i32, i32),
) -> Vec<(i32, i32)> {
    windows
        .iter()
        .map(|h| {
            let app = mins.get(h).copied().unwrap_or_default();
            (min.0.max(app.0).max(1), min.1.max(app.1).max(1))
        })
        .collect()
}

/// Verifies capacity from actual fitted rectangles, rather than from screen area alone.
pub fn layout_is_valid(
    area: RECT,
    windows: &[isize],
    tiles: &[RECT],
    mins: &HashMap<isize, (i32, i32)>,
    min: (i32, i32),
    gaps: impl Into<Gaps>,
) -> bool {
    let gaps = gaps.into();
    let r = inner(area, gaps.outer);
    if tiles.len() != windows.len() {
        return false;
    }
    let required = window_minimums(windows, mins, min);
    tiles.iter().zip(required).all(|(t, (mw, mh))| {
        t.left >= r.left
            && t.top >= r.top
            && t.right <= r.right
            && t.bottom <= r.bottom
            && t.right - t.left >= mw
            && t.bottom - t.top >= mh
    }) && tiles.iter().enumerate().all(|(i, a)| {
        tiles[i + 1..].iter().all(|b| {
            a.right.saturating_add(gaps.inner) <= b.left
                || b.right.saturating_add(gaps.inner) <= a.left
                || a.bottom.saturating_add(gaps.inner) <= b.top
                || b.bottom.saturating_add(gaps.inner) <= a.top
        })
    })
}

/// Equal bands where possible, with space reserved for every band that needs more.
fn fit_bands(span: i32, minimums: &[i32]) -> Option<Vec<i32>> {
    if minimums.is_empty() || minimums.iter().map(|v| i64::from(*v)).sum::<i64>() > i64::from(span)
    {
        return None;
    }
    let mut sizes = vec![0; minimums.len()];
    let mut remaining = span;
    loop {
        let count = sizes.iter().filter(|v| **v == 0).count();
        if count == 0 {
            return Some(sizes);
        }
        let share = remaining / count as i32;
        let mut fixed = false;
        for (i, minimum) in minimums.iter().enumerate() {
            if sizes[i] == 0 && *minimum > share {
                sizes[i] = *minimum;
                remaining -= *minimum;
                fixed = true;
            }
        }
        if !fixed {
            let mut remainder = remaining % count as i32;
            for size in sizes.iter_mut().filter(|v| **v == 0) {
                *size = share + i32::from(remainder > 0);
                remainder = (remainder - 1).max(0);
            }
            return Some(sizes);
        }
    }
}

fn fitted_columns(r: RECT, required: &[(i32, i32)], gap: i32) -> Option<(Vec<RECT>, Vec<RECT>)> {
    if required.iter().any(|(_, h)| *h > r.bottom - r.top) {
        return None;
    }
    let widths: Vec<i32> = required.iter().map(|(w, _)| *w).collect();
    let widths = fit_bands(
        r.right - r.left - gap * (required.len() as i32 - 1),
        &widths,
    )?;
    let mut left = r.left;
    let tiles: Vec<RECT> = widths
        .into_iter()
        .map(|width| {
            let tile = RECT {
                left,
                top: r.top,
                right: left + width,
                bottom: r.bottom,
            };
            left = tile.right + gap;
            tile
        })
        .collect();
    Some((tiles.clone(), tiles))
}

fn fitted_master_stack(
    r: RECT,
    required: &[(i32, i32)],
    ratios: &[f32],
    gap: i32,
) -> Option<(Vec<RECT>, Vec<RECT>)> {
    let masters = crate::config::master_count();
    if masters >= required.len() {
        return None; // everything is a master: the plain fallback fills the area
    }
    let height = r.bottom - r.top;
    let span = r.right - r.left - gap;
    let (master_req, stack_req) = required.split_at(masters);
    let master_width = master_req.iter().map(|(w, _)| *w).max()?;
    let stack_width = stack_req.iter().map(|(w, _)| *w).max()?;
    if master_width.saturating_add(stack_width) > span {
        return None;
    }
    let band_heights = |req: &[(i32, i32)]| {
        let heights: Vec<i32> = req.iter().map(|(_, h)| *h).collect();
        fit_bands(height - gap * (heights.len() as i32 - 1), &heights)
    };
    let master_heights = band_heights(master_req)?;
    let stack_heights = band_heights(stack_req)?;
    let desired = (span as f32 * ratios.first().copied().unwrap_or(0.5).clamp(0.0, 1.0)) as i32;
    let mid = r.left + desired.clamp(master_width, span - stack_width);
    let master = RECT { right: mid, ..r };
    let stack = RECT {
        left: mid + gap,
        ..r
    };
    let mut tiles = Vec::with_capacity(required.len());
    let mut regions = Vec::with_capacity(required.len());
    for (col, heights, region) in [(master, master_heights, r), (stack, stack_heights, stack)] {
        let mut top = r.top;
        for height in heights {
            tiles.push(RECT {
                top,
                bottom: top + height,
                ..col
            });
            regions.push(region);
            top += height + gap;
        }
    }
    Some((tiles, regions))
}

fn fitted_grid(r: RECT, required: &[(i32, i32)], gap: i32) -> Option<(Vec<RECT>, Vec<RECT>)> {
    let n = required.len();
    (1..=n)
        .filter_map(|cols| {
            let rows = n.div_ceil(cols);
            let mut widths = vec![1; cols];
            let mut heights = vec![1; rows];
            for (i, (w, h)) in required.iter().enumerate() {
                widths[i % cols] = widths[i % cols].max(*w);
                heights[i / cols] = heights[i / cols].max(*h);
            }
            let widths = fit_bands(r.right - r.left - gap * (cols as i32 - 1), &widths)?;
            let heights = fit_bands(r.bottom - r.top - gap * (rows as i32 - 1), &heights)?;
            let aspect = (r.right - r.left - gap * (cols as i32 - 1)) as f64
                / cols as f64
                / ((r.bottom - r.top - gap * (rows as i32 - 1)) as f64 / rows as f64);
            let score = (aspect / 1.5).log2().abs() + 2.0 * (rows * cols - n) as f64 / n as f64;
            let mut tiles = Vec::with_capacity(n);
            let mut top = r.top;
            for height in heights {
                let mut left = r.left;
                for width in &widths {
                    if tiles.len() == n {
                        break;
                    }
                    tiles.push(RECT {
                        left,
                        top,
                        right: left + width,
                        bottom: top + height,
                    });
                    left += width + gap;
                }
                top += height + gap;
            }
            Some((tiles, score))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(tiles, _)| (tiles.clone(), tiles))
}

fn safe_fallback(
    layout: Layout,
    area: RECT,
    n: usize,
    ratios: &[f32],
    gaps: Gaps,
) -> (Vec<RECT>, Vec<RECT>) {
    let result = place(layout, area, n, ratios, gaps);
    let windows: Vec<isize> = (0..n as isize).collect();
    if layout_is_valid(area, &windows, &result.0, &HashMap::new(), (1, 1), gaps) {
        return result;
    }
    // Even an impossible minimum or gap configuration must never produce inverted tiles.
    // The caller's validation still rejects these rectangles against the original constraints.
    if area.right - area.left >= n as i32 && area.bottom > area.top {
        columns(area, n, 0)
    } else if area.bottom - area.top >= n as i32 && area.right > area.left {
        let heights = vec![1; n];
        let Some(heights) = fit_bands(area.bottom - area.top, &heights) else {
            return (Vec::new(), Vec::new());
        };
        let mut top = area.top;
        let tiles: Vec<RECT> = heights
            .into_iter()
            .map(|height| {
                let tile = RECT {
                    top,
                    bottom: top + height,
                    ..area
                };
                top += height;
                tile
            })
            .collect();
        (tiles.clone(), tiles)
    } else {
        (Vec::new(), Vec::new())
    }
}

/// Which split controls the width (or height) of window `k` for `layout`, and in which direction,
/// like `split_for` (which is the spiral's implementation). Grid and columns are equal tiles with
/// no user-adjustable splits, so they return `None`.
pub fn split_for_layout(layout: Layout, k: usize, n: usize, width: bool) -> Option<(usize, f32)> {
    match layout {
        Layout::Spiral => split_for(k, n, width),
        // in master-stack only the master column's width (split 0) is adjustable; the bands are
        // equal. Growing a stack window shrinks the master column and vice versa.
        Layout::MasterStack if width && n >= 2 && crate::config::master_count() < n => Some((
            0,
            if k < crate::config::master_count() {
                1.0
            } else {
                -1.0
            },
        )),
        _ => None,
    }
}

/// The user dragged edges of window `k`; turn that into new split ratios, for `layout`.
pub fn apply_resize_for(
    layout: Layout,
    ratios: &mut Vec<f32>,
    tiles: &[RECT],
    regions: &[RECT],
    k: usize,
    new: RECT,
    gaps: impl Into<Gaps>,
) -> bool {
    let gaps = gaps.into();
    match layout {
        Layout::Spiral => apply_resize(ratios, tiles, regions, k, new, gaps),
        Layout::MasterStack => master_stack_resize(ratios, tiles, regions, k, new, gaps),
        Layout::Grid | Layout::Columns => false,
    }
}

/// Drag-resize for master-stack: moving a master's right edge (or a stack window's left edge)
/// changes the master's width ratio; anything else is ignored (the bands are equal).
fn master_stack_resize(
    ratios: &mut Vec<f32>,
    tiles: &[RECT],
    regions: &[RECT],
    k: usize,
    new: RECT,
    gaps: impl Into<Gaps>,
) -> bool {
    let gaps = gaps.into();
    let n = tiles.len();
    if n < 2 || k >= n || regions.is_empty() || crate::config::master_count() >= n {
        return false;
    }
    ratios.resize(1, 0.5);
    let q = regions[0]; // the whole area the master/stack split was cut from
    let span = (q.right - q.left - gaps.inner).max(1);
    let t = tiles[k];
    let moved = |a: i32, b: i32| (a - b).abs() > 3;
    let edge = if k < crate::config::master_count() {
        if !moved(new.right, t.right) {
            return false;
        }
        new.right
    } else {
        if !moved(new.left, t.left) {
            return false;
        }
        new.left - gaps.inner
    };
    let r = ((edge - q.left) as f32 / span as f32)
        .clamp(MIN_RATIO.min(ratios[0]), MAX_RATIO.max(ratios[0]));
    if (r - ratios[0]).abs() > 0.002 {
        ratios[0] = r;
        true
    } else {
        false
    }
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
    gaps: impl Into<Gaps>,
) -> bool {
    let gaps = gaps.into();
    let n = tiles.len();
    if k >= n || regions.len() != n {
        return false;
    }
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
            (edge - q.left) as f32 / ((q.right - q.left - gaps.inner).max(1)) as f32
        } else {
            (edge - q.top) as f32 / ((q.bottom - q.top - gaps.inner).max(1)) as f32
        }
        .clamp(MIN_RATIO.min(ratios[j]), MAX_RATIO.max(ratios[j]));
        if (r - ratios[j]).abs() > 0.002 {
            ratios[j] = r;
            changed = true;
        }
    };

    if k.is_multiple_of(2) {
        if k >= 2 && moved(new.left, t.left) {
            set(k - 2, new.left - gaps.inner, ratios);
        }
        if k >= 1 && moved(new.top, t.top) {
            set(k - 1, new.top - gaps.inner, ratios);
        }
        if k < n - 1 && moved(new.right, t.right) {
            set(k, new.right, ratios);
        }
    } else {
        if moved(new.left, t.left) {
            set(k - 1, new.left - gaps.inner, ratios);
        }
        if k >= 3 && moved(new.top, t.top) {
            set(k - 2, new.top - gaps.inner, ratios);
        }
        if k < n - 1 && moved(new.bottom, t.bottom) {
            set(k, new.bottom, ratios);
        }
    }
    changed
}

#[cfg(test)]
fn tiles_at_least(tiles: &[RECT], min_w: i32, min_h: i32) -> bool {
    tiles
        .iter()
        .all(|t| (t.right - t.left) >= min_w && (t.bottom - t.top) >= min_h)
}

/// Fits every split from the minimum extent of its whole remaining subtree.
/// This reserves the neighbours' minimums before growing an individual tile.
pub fn spiral_fitted(
    area: RECT,
    windows: &[isize],
    ratios: &[f32],
    mins: &HashMap<isize, (i32, i32)>,
    min_w: i32,
    min_h: i32,
    gaps: impl Into<Gaps>,
) -> (Vec<RECT>, Vec<RECT>) {
    let gaps = gaps.into();
    let required = window_minimums(windows, mins, (min_w, min_h));
    let n = required.len();
    if n == 0 {
        return (Vec::new(), Vec::new());
    }
    let mut subtree = required.clone();
    for i in (0..n - 1).rev() {
        let (w, h) = required[i];
        let (tail_w, tail_h) = subtree[i + 1];
        subtree[i] = if i.is_multiple_of(2) {
            (
                w.saturating_add(gaps.inner).saturating_add(tail_w),
                h.max(tail_h),
            )
        } else {
            (
                w.max(tail_w),
                h.saturating_add(gaps.inner).saturating_add(tail_h),
            )
        };
    }
    let mut r = inner(area, gaps.outer);
    if subtree[0].0 > r.right - r.left || subtree[0].1 > r.bottom - r.top {
        return safe_fallback(Layout::Spiral, area, n, ratios, gaps);
    }
    let mut tiles = Vec::with_capacity(n);
    let mut regions = Vec::with_capacity(n);
    for i in 0..n {
        regions.push(r);
        if i == n - 1 {
            tiles.push(r);
            break;
        }
        let ratio = ratios.get(i).copied().unwrap_or(0.5).clamp(0.0, 1.0);
        if i.is_multiple_of(2) {
            let span = r.right - r.left - gaps.inner;
            let width =
                ((span as f32 * ratio) as i32).clamp(required[i].0, span - subtree[i + 1].0);
            tiles.push(RECT {
                right: r.left + width,
                ..r
            });
            r.left += width + gaps.inner;
        } else {
            let span = r.bottom - r.top - gaps.inner;
            let height =
                ((span as f32 * ratio) as i32).clamp(required[i].1, span - subtree[i + 1].1);
            tiles.push(RECT {
                bottom: r.top + height,
                ..r
            });
            r.top += height + gaps.inner;
        }
    }
    (tiles, regions)
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

/// One edge on a cubic Hermite curve: leaves `a` with the slope `m0` (pixels per unit of `t`)
/// and arrives at `b` at rest. With `m0 = 3 * (b - a)` this is exactly `ease_out_cubic`.
pub fn hermite_i(a: i32, b: i32, m0: f32, t: f32) -> i32 {
    let t = t.clamp(0.0, 1.0);
    let (t2, t3) = (t * t, t * t * t);
    let p = (2.0 * t3 - 3.0 * t2 + 1.0) * a as f32
        + (t3 - 2.0 * t2 + t) * m0
        + (-2.0 * t3 + 3.0 * t2) * b as f32;
    p.round() as i32
}

/// Slope of `hermite_i` at `t`, in pixels per unit of `t`.
pub fn hermite_slope(a: i32, b: i32, m0: f32, t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let t2 = t * t;
    (6.0 * t2 - 6.0 * t) * a as f32
        + (3.0 * t2 - 4.0 * t + 1.0) * m0
        + (-6.0 * t2 + 6.0 * t) * b as f32
}

/// Starting slope for an animation that replaces a running one: the edge's current speed
/// `velocity` (px/s) over the new duration, limited so the edge never overshoots its target
/// (at most the ease-out-cubic slope) and only briefly runs on backwards.
pub fn carried_tangent(velocity: f32, delta: i32, dur_s: f32) -> f32 {
    if delta == 0 {
        return 0.0;
    }
    let d = delta as f32;
    let share = (velocity * dur_s / d).clamp(-1.5, 3.0);
    share * d
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

    /// The default gap (`config.toml` default), passed explicitly now that the module is pure.
    const GAP: i32 = 10;

    fn layout(n: usize, ratios: &[f32]) -> (Vec<RECT>, Vec<RECT>) {
        spiral(AREA, n, ratios, GAP)
    }

    /// Drag one edge of window k to `v`, rebuild the layout, return the new tiles.
    fn drag(n: usize, k: usize, edit: impl Fn(&mut RECT)) -> Vec<RECT> {
        let (tiles, regions) = layout(n, &[]);
        let mut new = tiles[k];
        edit(&mut new);
        let mut ratios = vec![0.5; n - 1];
        assert!(
            apply_resize(&mut ratios, &tiles, &regions, k, new, GAP),
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
        let (base, _) = spiral(AREA, 3, &[], GAP);
        let w0 = base[0].right - base[0].left;
        let min_w = w0 + 200;
        let mut mins = HashMap::new();
        mins.insert(10isize, (min_w, 0));
        let (t, _) = spiral_fitted(AREA, &windows, &[], &mins, 0, 0, GAP);
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
        let (same, _) = spiral_fitted(AREA, &windows, &[], &HashMap::new(), 0, 0, GAP);
        let (plain, _) = spiral(AREA, 3, &[], GAP);
        assert_eq!(same, plain);
    }

    #[test]
    fn descendant_splits_reserve_every_tiles_comfortable_minimum() {
        // many windows: with equal halves the last tiles are narrower than the comfortable minimum
        let windows: Vec<isize> = (0..6).map(|i| 100 + i).collect();
        let (plain, _) = spiral(AREA, 6, &[], GAP);
        let (fitted, _) = spiral_fitted(AREA, &windows, &[], &HashMap::new(), 480, 320, GAP);
        assert!(
            !tiles_at_least(&plain, 480, 320),
            "with equal halves a tile is too narrow: {plain:?}"
        );
        assert!(tiles_at_least(&fitted, 480, 320), "{fitted:?}");
        // The master need not move when adjusting descendant splits is sufficient.
        assert_eq!(fitted[0], plain[0]);
        assert_ne!(fitted, plain);
        // still a valid, non-overlapping spiral
        assert!(fitted[0].right <= fitted[1].left, "{fitted:?}");
    }

    #[test]
    fn a_minimum_size_of_a_last_window_grows_it_to_the_left() {
        let windows = [10isize, 20, 30];
        let (base, _) = spiral(AREA, 3, &[], GAP);
        let w2 = base[2].right - base[2].left;
        let min_w = w2 + 150;
        let mut mins = HashMap::new();
        mins.insert(30isize, (min_w, 0)); // window 2 is the last one
        let (t, _) = spiral_fitted(AREA, &windows, &[], &mins, 0, 0, GAP);
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
        assert!(!apply_resize(&mut ratios, &tiles, &regions, 2, new, GAP));
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
    fn surplus_ratios_from_a_bigger_layout_are_ignored() {
        // saved ratios of three windows while only the first window is being admitted at start
        let area = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        let gaps = Gaps {
            inner: 10,
            outer: 10,
        };
        let mins = HashMap::new();
        for layout in [
            Layout::Spiral,
            Layout::MasterStack,
            Layout::Grid,
            Layout::Columns,
        ] {
            for n in 1..=3usize {
                let windows: Vec<isize> = (1..=n as isize).collect();
                let exact = vec![0.4899; n - 1];
                let surplus = [0.4899, 0.4899, 0.4899];
                let (tiles, _) =
                    place_fitted(layout, area, &windows, &surplus, &mins, (460, 320), gaps);
                assert_eq!(
                    tiles,
                    place_fitted(layout, area, &windows, &exact, &mins, (460, 320), gaps).0,
                    "{layout:?} with {n} window(s)"
                );
                assert!(layout_is_valid(
                    area,
                    &windows,
                    &tiles,
                    &mins,
                    (460, 320),
                    gaps
                ));
                assert_eq!(
                    place(layout, area, n, &surplus, gaps).0,
                    place(layout, area, n, &exact, gaps).0
                );
            }
        }
    }

    #[test]
    fn hermite_with_the_ease_out_slope_is_ease_out_cubic() {
        for i in 0..=20 {
            let t = i as f32 / 20.0;
            assert_eq!(
                hermite_i(100, 500, 3.0 * 400.0, t),
                lerp_i(100, 500, ease_out_cubic(t))
            );
        }
        assert_eq!(hermite_i(100, 500, 0.0, 1.0), 500);
        assert!((hermite_slope(100, 500, 1200.0, 0.0) - 1200.0).abs() < 0.01);
        assert!(
            hermite_slope(100, 500, 1200.0, 1.0).abs() < 0.01,
            "arrives at rest"
        );
    }

    #[test]
    fn a_carried_slope_never_overshoots_and_only_briefly_reverses() {
        // moving fast towards the new target: capped at the ease-out slope
        assert_eq!(carried_tangent(10_000.0, 100, 0.14), 300.0);
        // moving away from it: some run-on, then back
        assert_eq!(carried_tangent(-10_000.0, 100, 0.14), -150.0);
        assert_eq!(carried_tangent(500.0, 0, 0.14), 0.0);
        // with any allowed slope the edge stays within [a - run-on, b]
        for m0 in [-150.0, 0.0, 150.0, 300.0] {
            for i in 0..=50 {
                let p = hermite_i(0, 100, m0, i as f32 / 50.0);
                assert!((-30..=100).contains(&p), "m0 {m0}: {p}");
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

    fn overlaps(a: &RECT, b: &RECT) -> bool {
        a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
    }

    #[test]
    fn place_matches_spiral() {
        for n in 0..=8usize {
            assert_eq!(
                place(Layout::Spiral, AREA, n, &[0.3], GAP),
                spiral(AREA, n, &[0.3], GAP)
            );
        }
    }

    #[test]
    fn every_layout_covers_the_area_without_overlap() {
        let layouts = [
            Layout::Spiral,
            Layout::MasterStack,
            Layout::Grid,
            Layout::Columns,
        ];
        for layout in layouts {
            for n in 0..=12usize {
                let (tiles, regions) = place(layout, AREA, n, &[], GAP);
                assert_eq!(tiles.len(), n, "{layout:?} n={n}");
                assert_eq!(regions.len(), n, "{layout:?} n={n}");
                let r = inner(AREA, GAP);
                for (i, t) in tiles.iter().enumerate() {
                    assert!(
                        t.left >= r.left
                            && t.top >= r.top
                            && t.right <= r.right
                            && t.bottom <= r.bottom,
                        "{layout:?} n={n} tile {i} {t:?} outside {r:?}"
                    );
                    assert!(
                        t.right > t.left && t.bottom > t.top,
                        "{layout:?} n={n} tile {i} is empty"
                    );
                }
                for i in 0..n {
                    for j in (i + 1)..n {
                        assert!(
                            !overlaps(&tiles[i], &tiles[j]),
                            "{layout:?} n={n}: tiles {i} and {j} overlap"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn master_stack_puts_the_master_on_the_left() {
        let r = inner(AREA, GAP);
        let (t, _) = master_stack(AREA, 3, &[], GAP);
        assert_eq!(t[0].top, r.top);
        assert_eq!(t[0].bottom, r.bottom);
        assert!(t[0].right <= t[1].left);
        assert_eq!(t[1].left, t[2].left);
        assert_eq!(t[1].right, t[2].right);
        assert!(t[1].bottom <= t[2].top);
        // a bigger master ratio widens the master
        let (wide, _) = master_stack(AREA, 3, &[0.7], GAP);
        assert!(wide[0].right > t[0].right);
        // a single window fills the whole area, like the spiral
        assert_eq!(master_stack(AREA, 1, &[], GAP).0[0], r);
    }

    #[test]
    fn columns_are_equal_width() {
        let r = inner(AREA, GAP);
        let (t, _) = columns(AREA, 4, GAP);
        let w = |x: &RECT| x.right - x.left;
        assert!(near(w(&t[0]), w(&t[1])));
        assert!(near(w(&t[2]), w(&t[1])));
        assert!(
            (w(&t[3]) - w(&t[0])).abs() <= 3,
            "last column absorbs the remainder"
        );
        for x in &t {
            assert_eq!(x.top, r.top);
            assert_eq!(x.bottom, r.bottom);
        }
    }

    #[test]
    fn master_stack_with_two_masters_splits_both_columns() {
        let r = inner(AREA, GAP);
        let (t, regions) = master_stack_with(AREA, 4, &[], GAP, 2);
        assert_eq!(t.len(), 4);
        // both masters share the left column, the two others the right one
        assert_eq!(t[0].left, r.left);
        assert_eq!(t[0].left, t[1].left);
        assert_eq!(t[0].right, t[1].right);
        assert_eq!(t[2].left, t[3].left);
        assert!(t[0].right < t[2].left);
        assert_eq!(t[0].top, r.top);
        assert_eq!(t[1].bottom, r.bottom);
        assert!(t[0].bottom < t[1].top);
        assert_eq!(regions[0], r);
        assert_eq!(regions[1], r);
        assert_eq!(regions[2], regions[3]);
    }

    #[test]
    fn master_stack_with_all_masters_fills_the_area() {
        let r = inner(AREA, GAP);
        let (t, _) = master_stack_with(AREA, 2, &[], GAP, 3);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].left, r.left);
        assert_eq!(t[0].right, r.right);
        assert_eq!(t[1].right, r.right);
        assert_eq!(t[1].bottom, r.bottom);
    }

    #[test]
    fn monocle_gives_every_window_the_whole_area() {
        let r = inner(AREA, GAP);
        let (t, regions) = monocle(AREA, 3, GAP);
        assert_eq!(t, vec![r; 3]);
        assert_eq!(regions, vec![r; 3]);
        assert!(monocle(AREA, 0, GAP).0.is_empty());
    }

    #[test]
    fn grid_two_by_two_for_four_windows() {
        let (t, _) = grid(AREA, 4, GAP);
        assert_eq!(t[0].top, t[1].top);
        assert_eq!(t[2].top, t[3].top);
        assert_eq!(t[0].left, t[2].left);
        assert_eq!(t[1].left, t[3].left);
        assert!(t[0].bottom <= t[2].top);
        assert!(t[0].right <= t[1].left);
        assert_eq!(t[3].right, inner(AREA, GAP).right);
        assert_eq!(t[3].bottom, inner(AREA, GAP).bottom);
        // Three windows on this ultrawide area use one row instead of leaving a large empty cell.
        let (t3, _) = grid(AREA, 3, GAP);
        assert_eq!(t3[2].top, t3[0].top);
        assert!(t3[2].left > t3[1].left);
    }

    #[test]
    fn grid_adapts_to_portrait_monitor_shape() {
        let portrait = RECT {
            left: 0,
            top: 0,
            right: 1200,
            bottom: 2400,
        };
        let (tiles, _) = grid(portrait, 3, GAP);
        assert_eq!(tiles[0].left, tiles[1].left);
        assert!(tiles[1].top > tiles[0].top);
        assert!(tiles[2].top > tiles[1].top);
    }

    #[test]
    fn split_for_layout_is_per_layout() {
        assert_eq!(split_for_layout(Layout::Spiral, 0, 3, true), Some((0, 1.0)));
        assert_eq!(
            split_for_layout(Layout::MasterStack, 0, 3, true),
            Some((0, 1.0))
        );
        // a stack window drives the same split, in the opposite direction
        assert_eq!(
            split_for_layout(Layout::MasterStack, 1, 3, true),
            Some((0, -1.0))
        );
        assert_eq!(split_for_layout(Layout::MasterStack, 0, 3, false), None);
        assert_eq!(split_for_layout(Layout::MasterStack, 0, 1, true), None);
        assert_eq!(split_for_layout(Layout::Grid, 0, 4, true), None);
        assert_eq!(split_for_layout(Layout::Columns, 1, 4, true), None);
    }

    #[test]
    fn master_stack_resize_changes_the_master_width() {
        let (tiles, regions) = master_stack(AREA, 3, &[], GAP);
        let mut rs: Vec<f32> = vec![];
        // dragging the master's right edge past its tile widens the master
        let new = RECT {
            right: tiles[0].right + 400,
            ..tiles[0]
        };
        assert!(apply_resize_for(
            Layout::MasterStack,
            &mut rs,
            &tiles,
            &regions,
            0,
            new,
            GAP
        ));
        assert!(rs[0] > 0.5);
        // dragging a stack window's left edge also moves the divider
        let mut rs2: Vec<f32> = vec![0.5];
        let new2 = RECT {
            left: tiles[1].left + 300,
            ..tiles[1]
        };
        assert!(apply_resize_for(
            Layout::MasterStack,
            &mut rs2,
            &tiles,
            &regions,
            1,
            new2,
            GAP
        ));
        assert!(rs2[0] > 0.5);
        // dragging a stack window's top edge does nothing (the bands are equal)
        let mut rs3: Vec<f32> = vec![0.5];
        let new3 = RECT {
            top: tiles[1].top + 50,
            ..tiles[1]
        };
        assert!(!apply_resize_for(
            Layout::MasterStack,
            &mut rs3,
            &tiles,
            &regions,
            1,
            new3,
            GAP
        ));
    }

    #[test]
    fn impossible_master_stack_minimum_is_rejected_without_sacrificing_the_master() {
        // Two columns cannot both be 2000px wide on this monitor.
        let windows = [1isize, 2, 3];
        let (t, _) = place_fitted(
            Layout::MasterStack,
            AREA,
            &windows,
            &[],
            &HashMap::new(),
            (2000, 0),
            GAP,
        );
        assert!(!layout_is_valid(
            AREA,
            &windows,
            &t,
            &HashMap::new(),
            (2000, 0),
            GAP
        ));
        assert!(layout_is_valid(
            AREA,
            &windows,
            &t,
            &HashMap::new(),
            (1, 1),
            GAP
        ));
    }

    #[test]
    fn every_fitted_layout_reserves_each_applications_minimum() {
        let windows = [10, 20, 30];
        let mins = HashMap::from([(10, (1200, 300)), (20, (700, 500)), (30, (500, 500))]);
        for layout in [
            Layout::Spiral,
            Layout::MasterStack,
            Layout::Grid,
            Layout::Columns,
        ] {
            let (tiles, regions) = place_fitted(
                layout,
                AREA,
                &windows,
                &[0.88, 0.88],
                &mins,
                (480, 320),
                GAP,
            );
            assert_eq!(regions.len(), windows.len());
            assert!(
                layout_is_valid(AREA, &windows, &tiles, &mins, (480, 320), GAP),
                "{layout:?}: {tiles:?}"
            );
        }
    }

    #[test]
    fn a_wide_application_cannot_take_its_neighbours_minimum_width() {
        let area = RECT {
            left: 0,
            top: 0,
            right: 3440,
            bottom: 1440,
        };
        let windows = [10, 20];
        let mins = HashMap::from([(10, (2750, 500))]);
        for layout in [
            Layout::Spiral,
            Layout::MasterStack,
            Layout::Grid,
            Layout::Columns,
        ] {
            let (tiles, _) = place_fitted(layout, area, &windows, &[0.88], &mins, (480, 320), GAP);
            assert!(
                layout_is_valid(area, &windows, &tiles, &mins, (480, 320), GAP),
                "{layout:?}: {tiles:?}"
            );
            assert!(tiles[1].right - tiles[1].left >= 480);
        }
    }

    #[test]
    fn impossible_constraints_are_explicit_for_every_layout() {
        let windows = [10, 20, 30];
        let mins = HashMap::from([(10, (4000, 2000))]);
        for layout in [
            Layout::Spiral,
            Layout::MasterStack,
            Layout::Grid,
            Layout::Columns,
        ] {
            let (tiles, _) = place_fitted(layout, AREA, &windows, &[], &mins, (480, 320), GAP);
            assert!(!layout_is_valid(
                AREA,
                &windows,
                &tiles,
                &mins,
                (480, 320),
                GAP
            ));
            assert!(layout_is_valid(
                AREA,
                &windows,
                &tiles,
                &HashMap::new(),
                (1, 1),
                GAP
            ));
        }
    }

    #[test]
    fn capacity_depends_on_layout_and_monitor_shape() {
        let area = RECT {
            left: -1000,
            top: 0,
            right: 0,
            bottom: 2000,
        };
        let windows = [10, 20, 30];
        let mins = HashMap::new();
        let (columns, _) =
            place_fitted(Layout::Columns, area, &windows, &[], &mins, (480, 320), GAP);
        assert!(!layout_is_valid(
            area,
            &windows,
            &columns,
            &mins,
            (480, 320),
            GAP
        ));
        let (grid, _) = place_fitted(Layout::Grid, area, &windows, &[], &mins, (480, 320), GAP);
        assert!(layout_is_valid(
            area,
            &windows,
            &grid,
            &mins,
            (480, 320),
            GAP
        ));
    }

    #[test]
    fn fitted_ratios_stay_unchanged_when_all_minimums_already_fit() {
        let windows = [10, 20, 30];
        for layout in [Layout::Spiral, Layout::MasterStack] {
            let plain = place(layout, AREA, windows.len(), &[0.4, 0.6], GAP).0;
            let fitted = place_fitted(
                layout,
                AREA,
                &windows,
                &[0.4, 0.6],
                &HashMap::new(),
                (100, 100),
                GAP,
            )
            .0;
            assert_eq!(plain, fitted);
        }
    }

    #[test]
    fn excessive_gaps_do_not_generate_inverted_or_overlapping_fallbacks() {
        let area = RECT {
            left: 0,
            top: 0,
            right: 100,
            bottom: 100,
        };
        let windows = [10, 20, 30];
        let gaps = Gaps {
            outer: 90,
            inner: 90,
        };
        for layout in [
            Layout::Spiral,
            Layout::MasterStack,
            Layout::Grid,
            Layout::Columns,
        ] {
            let (tiles, _) = place_fitted(
                layout,
                area,
                &windows,
                &[],
                &HashMap::new(),
                (480, 320),
                gaps,
            );
            assert!(!layout_is_valid(
                area,
                &windows,
                &tiles,
                &HashMap::new(),
                (480, 320),
                gaps
            ));
            assert!(layout_is_valid(
                area,
                &windows,
                &tiles,
                &HashMap::new(),
                (1, 1),
                0
            ));
        }
    }

    #[test]
    fn validator_rejects_missing_tiles_bounds_and_insufficient_separation() {
        let area = RECT {
            left: 0,
            top: 0,
            right: 1020,
            bottom: 500,
        };
        let windows = [10, 20];
        let mins = HashMap::new();
        let left = RECT {
            left: 10,
            top: 10,
            right: 500,
            bottom: 490,
        };
        let right = RECT {
            left: 510,
            top: 10,
            right: 1010,
            bottom: 490,
        };
        assert!(layout_is_valid(
            area,
            &windows,
            &[left, right],
            &mins,
            (480, 320),
            GAP
        ));
        assert!(!layout_is_valid(
            area,
            &windows,
            &[left],
            &mins,
            (480, 320),
            GAP
        ));
        assert!(!layout_is_valid(
            area,
            &windows,
            &[left, RECT { left: 505, ..right }],
            &mins,
            (480, 320),
            GAP
        ));
        assert!(!layout_is_valid(
            area,
            &windows,
            &[
                left,
                RECT {
                    right: 1021,
                    ..right
                }
            ],
            &mins,
            (480, 320),
            GAP
        ));
        assert!(!layout_is_valid(
            area,
            &windows,
            &[RECT { right: 520, ..left }, right],
            &mins,
            (480, 320),
            GAP
        ));
    }

    #[test]
    fn inner_and_outer_gaps_are_independent() {
        let gaps = Gaps {
            inner: 4,
            outer: 20,
        };
        let (t, _) = spiral(AREA, 3, &[], gaps);
        assert_eq!(t[0].left, AREA.left + 20); // outer gap at the screen edge
        assert_eq!(t[0].top, AREA.top + 20);
        assert_eq!(t[1].left - t[0].right, 4); // inner gap between tiles
        let (g, _) = grid(AREA, 4, gaps);
        assert_eq!(g[0].left, AREA.left + 20);
        assert_eq!(g[1].left - g[0].right, 4);
        // a single value keeps the old behaviour (same gap everywhere)
        let (s, _) = spiral(AREA, 3, &[], 10);
        let (a, _) = spiral(
            AREA,
            3,
            &[],
            Gaps {
                inner: 10,
                outer: 10,
            },
        );
        assert_eq!(s, a);
    }

    #[test]
    fn adjusting_an_ultrawide_split_does_not_jump_back_to_legacy_ratio_limits() {
        let area = RECT {
            left: 0,
            top: 0,
            right: 7680,
            bottom: 1440,
        };
        for layout in [Layout::Spiral, Layout::MasterStack] {
            for (start, delta) in [(0.93, -50), (0.07, 50)] {
                let mut ratios = vec![start];
                let (tiles, regions) = place_fitted(
                    layout,
                    area,
                    &[1, 2],
                    &ratios,
                    &HashMap::new(),
                    (480, 320),
                    10,
                );
                let mut requested = tiles[0];
                requested.right += delta;
                assert!(apply_resize_for(
                    layout,
                    &mut ratios,
                    &tiles,
                    &regions,
                    0,
                    requested,
                    10
                ));
                assert!((ratios[0] - start).abs() < 0.01);
                assert!(if delta < 0 {
                    ratios[0] < start
                } else {
                    ratios[0] > start
                });
            }
        }
    }
}
