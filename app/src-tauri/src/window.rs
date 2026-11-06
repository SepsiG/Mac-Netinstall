//! The main window's size and position, remembered across launches
//! (`settings.json`, [`crate::settings::WindowGeometry`]).
//!
//! The geometry is tracked from the window's move and resize events (only
//! while it is a normal window: not minimized, maximized or full screen)
//! and stored when the app quits. At start the size is restored (at least
//! the minimum, at most the display), the position only if it is on a
//! display that is connected now; otherwise macOS places the window.

use crate::settings::{WindowGeometry, MIN_WINDOW_HEIGHT, MIN_WINDOW_WIDTH};
use std::sync::Mutex;

/// A display's area in points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Display {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Display {
    fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
}

/// How much of the title bar must be on a display for the saved position
/// to be used: this point, relative to the window's top-left corner.
const GRIP: (f64, f64) = (60.0, 12.0);

/// What to apply at start for `saved` with these `displays`: the content
/// size (points) and, if the saved title bar is on one of them, the
/// position. The size is at least the window's minimum and at most the
/// display it goes on (the largest one if the position is not used).
pub fn restore(saved: &WindowGeometry, displays: &[Display]) -> ((f64, f64), Option<(f64, f64)>) {
    let (x, y) = (f64::from(saved.x), f64::from(saved.y));
    let home = displays.iter().find(|d| d.contains(x + GRIP.0, y + GRIP.1));
    let bound = home.or_else(|| {
        displays
            .iter()
            .max_by(|a, b| (a.width * a.height).total_cmp(&(b.width * b.height)))
    });
    let clamp = |v: u32, min: u32, max: Option<f64>| {
        let v = f64::from(v).max(f64::from(min));
        max.map_or(v, |m| v.min(m.max(f64::from(min))))
    };
    let size = (
        clamp(saved.width, MIN_WINDOW_WIDTH, bound.map(|d| d.width)),
        clamp(saved.height, MIN_WINDOW_HEIGHT, bound.map(|d| d.height)),
    );
    (size, home.map(|_| (x, y)))
}

/// The geometry from the window's outer position and inner size in
/// physical pixels and its scale factor, rounded to points; `None` if it
/// is not storable (e.g. smaller than the minimum).
pub fn geometry(pos: (i32, i32), size: (u32, u32), scale: f64) -> Option<WindowGeometry> {
    if !(scale.is_finite() && scale > 0.0) {
        return None;
    }
    let pt = |v: f64| (v / scale).round();
    let g = WindowGeometry {
        x: pt(f64::from(pos.0)) as i32,
        y: pt(f64::from(pos.1)) as i32,
        width: pt(f64::from(size.0)) as u32,
        height: pt(f64::from(size.1)) as u32,
    };
    g.is_valid().then_some(g)
}

/// The main window's last normal geometry (managed state).
#[derive(Debug, Default)]
pub struct WindowTracker(Mutex<Option<WindowGeometry>>);

impl WindowTracker {
    pub fn set(&self, g: WindowGeometry) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(g);
    }

    pub fn get(&self) -> Option<WindowGeometry> {
        *self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIN: Display = Display {
        x: 0.0,
        y: 0.0,
        width: 1512.0,
        height: 982.0,
    };
    const LEFT: Display = Display {
        x: -1920.0,
        y: 0.0,
        width: 1920.0,
        height: 1080.0,
    };

    fn g(x: i32, y: i32, width: u32, height: u32) -> WindowGeometry {
        WindowGeometry {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn a_position_on_a_connected_display_is_restored() {
        assert_eq!(
            restore(&g(100, 80, 1200, 760), &[MAIN]),
            ((1200.0, 760.0), Some((100.0, 80.0)))
        );
        assert_eq!(
            restore(&g(-1800, 50, 1000, 700), &[MAIN, LEFT]),
            ((1000.0, 700.0), Some((-1800.0, 50.0)))
        );
    }

    #[test]
    fn a_position_on_a_display_that_is_gone_is_not() {
        // Saved on the left display, which is unplugged now.
        assert_eq!(
            restore(&g(-1800, 50, 1000, 700), &[MAIN]),
            ((1000.0, 700.0), None)
        );
        // Only the right edge was on the display: the title bar is not.
        assert_eq!(restore(&g(1500, 50, 1000, 700), &[MAIN]).1, None);
        assert_eq!(
            restore(&g(100, 80, 1000, 700), &[]),
            ((1000.0, 700.0), None)
        );
    }

    #[test]
    fn the_size_stays_within_the_minimum_and_the_display() {
        assert_eq!(restore(&g(0, 0, 4000, 3000), &[MAIN]).0, (1512.0, 982.0));
        assert_eq!(
            restore(&g(-1800, 0, 4000, 3000), &[MAIN, LEFT]).0,
            (1920.0, 1080.0)
        );
        let tiny = Display {
            width: 640.0,
            height: 480.0,
            ..MAIN
        };
        assert_eq!(
            restore(&g(0, 0, 1000, 700), &[tiny]).0,
            (f64::from(MIN_WINDOW_WIDTH), f64::from(MIN_WINDOW_HEIGHT))
        );
    }

    #[test]
    fn geometry_is_in_points() {
        assert_eq!(
            geometry((200, 160), (2080, 1360), 2.0),
            Some(g(100, 80, 1040, 680))
        );
        assert_eq!(geometry((0, 0), (100, 100), 1.0), None, "too small");
        assert_eq!(geometry((0, 0), (2080, 1360), 0.0), None);
    }

    #[test]
    fn the_tracker_keeps_the_last_geometry() {
        let t = WindowTracker::default();
        assert_eq!(t.get(), None);
        t.set(g(1, 2, 900, 600));
        t.set(g(3, 4, 1000, 700));
        assert_eq!(t.get(), Some(g(3, 4, 1000, 700)));
    }
}
