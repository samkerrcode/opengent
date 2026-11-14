//! Sizes the main window to the monitor it opens on. The window starts hidden
//! (`"visible": false` in `tauri.conf.json`) so it can be resized and centred before the first
//! paint, then is always shown — even if sizing fails.

use tauri::{App, LogicalSize, Manager, PhysicalPosition, Runtime, WebviewWindow};

/// Minimum outer size in logical pixels; mirrors `minWidth`/`minHeight` in `tauri.conf.json`.
const MIN_WIDTH: f64 = 900.0;
const MIN_HEIGHT: f64 = 600.0;
/// Share of the monitor's work area the window opens at, per dimension.
const WORK_AREA_FRACTION: f64 = 0.75;

/// Target outer size and minimum outer size, both in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Fit {
    outer: (f64, f64),
    min: (f64, f64),
}

/// 75% of the work area, raised to the minimum, never larger than the work area. A work area
/// smaller than the minimum lowers the minimum to fit.
fn fit(work_width: f64, work_height: f64) -> Fit {
    let axis = |avail: f64, min: f64| {
        let min = min.min(avail);
        ((avail * WORK_AREA_FRACTION).max(min).min(avail), min)
    };
    let (width, min_width) = axis(work_width, MIN_WIDTH);
    let (height, min_height) = axis(work_height, MIN_HEIGHT);
    Fit {
        outer: (width, height),
        min: (min_width, min_height),
    }
}

pub fn fit_and_show<R: Runtime>(app: &App<R>) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    if let Err(err) = fit_to_monitor(&window) {
        tracing::warn!(error = %err, "could not size the main window to its monitor");
    }
    if let Err(err) = window.show() {
        tracing::warn!(error = %err, "could not show the main window");
    }
}

fn fit_to_monitor<R: Runtime>(window: &WebviewWindow<R>) -> tauri::Result<()> {
    let monitor = match window.current_monitor()? {
        Some(monitor) => Some(monitor),
        None => window.primary_monitor()?,
    };
    let Some(monitor) = monitor else {
        return Ok(());
    };
    let scale = monitor.scale_factor();
    let area = monitor.work_area();
    let fit = fit(
        f64::from(area.size.width) / scale,
        f64::from(area.size.height) / scale,
    );

    // Sizes set below are inner sizes; subtract the frame (title bar, borders) so the outer
    // window matches the target.
    let (outer, inner) = (window.outer_size()?, window.inner_size()?);
    let frame_width = f64::from(outer.width.saturating_sub(inner.width)) / scale;
    let frame_height = f64::from(outer.height.saturating_sub(inner.height)) / scale;
    let inner_size = |(width, height): (f64, f64)| {
        LogicalSize::new(
            (width - frame_width).max(1.0),
            (height - frame_height).max(1.0),
        )
    };

    if fit.min != (MIN_WIDTH, MIN_HEIGHT) {
        window.set_min_size(Some(inner_size(fit.min)))?;
    }
    window.set_size(inner_size(fit.outer))?;

    let centre = |origin: i32, avail: u32, size: f64| {
        let size = (size * scale).round() as i64;
        let offset = ((i64::from(avail) - size) / 2).max(0);
        (i64::from(origin) + offset) as i32
    };
    window.set_position(PhysicalPosition::new(
        centre(area.position.x, area.size.width, fit.outer.0),
        centre(area.position.y, area.size.height, fit.outer.1),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Work area of a physical screen with a 48 px (at 100%) taskbar, in logical pixels.
    fn work_area(width: f64, height: f64, scale: f64) -> (f64, f64) {
        (width / scale, height / scale - 48.0)
    }

    #[test]
    fn uhd_at_100_percent_uses_three_quarters() {
        let (w, h) = work_area(3840.0, 2160.0, 1.0);
        let fit = fit(w, h);
        assert_eq!(fit.outer, (2880.0, 1584.0));
        assert_eq!(fit.min, (MIN_WIDTH, MIN_HEIGHT));
    }

    #[test]
    fn uhd_at_150_percent_uses_three_quarters() {
        let (w, h) = work_area(3840.0, 2160.0, 1.5);
        let fit = fit(w, h);
        assert_eq!(fit.outer, (1920.0, 1044.0));
        assert_eq!(fit.min, (MIN_WIDTH, MIN_HEIGHT));
    }

    #[test]
    fn uhd_at_300_percent_raises_to_minimum_within_work_area() {
        let (w, h) = work_area(3840.0, 2160.0, 3.0);
        assert_eq!((w, h), (1280.0, 672.0));
        let fit = fit(w, h);
        assert_eq!(fit.outer, (960.0, 600.0));
        assert_eq!(fit.min, (MIN_WIDTH, MIN_HEIGHT));
    }

    #[test]
    fn full_hd_uses_three_quarters() {
        let (w, h) = work_area(1920.0, 1080.0, 1.0);
        let fit = fit(w, h);
        assert_eq!(fit.outer, (1440.0, 774.0));
        assert_eq!(fit.min, (MIN_WIDTH, MIN_HEIGHT));
    }

    #[test]
    fn tiny_screen_fills_work_area_and_lowers_minimum() {
        let fit = fit(800.0, 552.0);
        assert_eq!(fit.outer, (800.0, 552.0));
        assert_eq!(fit.min, (800.0, 552.0));
    }

    #[test]
    fn never_exceeds_work_area() {
        for (w, h) in [
            (900.0, 600.0),
            (1000.0, 650.0),
            (1200.0, 700.0),
            (5120.0, 2832.0),
        ] {
            let fit = fit(w, h);
            assert!(fit.outer.0 <= w && fit.outer.1 <= h, "{fit:?} in {w}x{h}");
            assert!(fit.outer.0 >= fit.min.0 && fit.outer.1 >= fit.min.1);
        }
    }
}
