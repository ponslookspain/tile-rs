//! Export native window facts and manager state for diagnostics.

use super::*;

pub(super) fn diagnostic_rect(rect: RECT) -> serde_json::Value {
    serde_json::json!([rect.left, rect.top, rect.right, rect.bottom])
}

pub(super) fn diagnostic_report(
    include_titles: bool,
    manager_state_available: bool,
) -> serde_json::Value {
    let rules = config::rules();
    let (ignored, tracked) = STATE.with(|state| {
        let state = state.borrow();
        let ignored = state
            .manual_float
            .union(&state.capacity_float)
            .copied()
            .collect();
        let tracked: HashSet<_> = state
            .identities
            .keys()
            .chain(state.ws.keys())
            .chain(state.fs_ws.keys())
            .copied()
            .collect();
        (ignored, tracked)
    });
    let mut windows = window::diagnostic_snapshots(&rules, &ignored, include_titles);
    let visible: HashSet<_> = windows
        .iter()
        .filter_map(|window| window["hwnd"].as_i64().map(|h| h as isize))
        .collect();
    for h in tracked.difference(&visible) {
        windows.push(window::diagnostic_snapshot(
            HWND(*h as *mut _),
            &rules,
            &ignored,
            include_titles,
        ));
    }
    STATE.with(|state| {
        let state = state.borrow();
        for window in &mut windows {
            let Some(h) = window["hwnd"].as_i64().map(|h| h as isize) else {
                continue;
            };
            let mi = state
                .mon
                .get(&h)
                .copied()
                .or_else(|| state.fs_mon.get(&h).copied());
            let ws = state
                .ws
                .get(&h)
                .copied()
                .or_else(|| state.fs_ws.get(&h).copied());
            let target = mi.and_then(|mi| {
                state.last[mi]
                    .iter()
                    .position(|item| *item == h)
                    .and_then(|index| state.rects[mi].get(index))
                    .copied()
            });
            window["manager"] = serde_json::json!({
                "monitor": mi.map(|mi| mi + 1),
                "workspace": ws.map(|ws| ws + 1),
                "target": target.map(diagnostic_rect),
                "suspended": state.suspended.contains(&h),
                "manual_float": state.manual_float.contains(&h),
                "automatic_float_reason": state.float_reasons.get(&h),
                "learned_minimum": state.min_size.get(&h),
                "geometry_retries": state.geometry_retries.get(&h).map(|(_, attempts)| attempts),
                "placement": state.placements.get(&h).map(|request| serde_json::json!({
                    "generation": request.generation,
                    "before": request.before.map(diagnostic_rect),
                    "target": diagnostic_rect(request.target),
                    "age_ms": request.submitted.elapsed().as_millis() as u64,
                    "minimum_queried": request.queried,
                }))
            });
        }
    });
    let monitors: Vec<_> = monitor::list().iter().enumerate().map(|(mi, monitor)| serde_json::json!({
        "index": mi + 1, "device": monitor.device, "work_area": diagnostic_rect(monitor.work),
        "screen": diagnostic_rect(monitor.full),
    })).collect();
    serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "executable": std::env::current_exe().ok(),
        "captured_at_unix_ms": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64,
        "manager_state_available": manager_state_available,
        "titles_included": include_titles,
        "config_path": config::config_path(),
        "layout": format!("{:?}", config::layout()),
        "minimum_tile": config::min_tile(),
        "max_tiles": config::max_tiles(),
        "auto_overflow": config::auto_overflow(),
        "monitors": monitors,
        "windows": windows,
    })
}

pub(crate) fn export_diagnostics(include_titles: bool) {
    let path = log_file().with_file_name("window-diagnostics.json");
    match write_diagnostics(&path, include_titles, true) {
        Ok(()) => notify(
            "tile-rs: window diagnostics",
            &format!("Saved to {}", path.display()),
        ),
        Err(error) => notify("tile-rs: diagnostics failed", &error.to_string()),
    }
}

pub(super) fn write_diagnostics(
    path: &std::path::Path,
    include_titles: bool,
    manager: bool,
) -> std::io::Result<()> {
    let report = serde_json::to_vec_pretty(&diagnostic_report(include_titles, manager))?;
    std::fs::write(path, report)
}
