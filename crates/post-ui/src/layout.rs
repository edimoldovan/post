//! How much of a route the window has room for: the designs' breakpoints.
//!
//! The width decides the columns and nothing else. The history stays as it
//! is, so resizing shows the same screens with more or fewer columns.

/// The sidebar with names, and the rail of icons it narrows to.
pub const SIDEBAR: f32 = 232.0;
pub const RAIL: f32 = 60.0;

/// The columns a window of one width shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    /// The sidebar's width: [`SIDEBAR`], or [`RAIL`].
    pub side: f32,
    /// The list beside the reader. `None` under 760, where the route's
    /// deepest screen is the only column.
    pub list: Option<f32>,
}

impl Layout {
    pub fn of(width: f32) -> Layout {
        if width > 1200.0 {
            Layout { side: SIDEBAR, list: Some(400.0) }
        } else if width >= 1000.0 {
            Layout { side: SIDEBAR, list: Some(340.0) }
        } else if width >= 760.0 {
            Layout { side: RAIL, list: Some(340.0) }
        } else {
            Layout { side: RAIL, list: None }
        }
    }

    /// True when the sidebar is a rail of icons.
    pub fn rail(&self) -> bool {
        self.side == RAIL
    }
}

impl Default for Layout {
    fn default() -> Layout {
        Layout::of(1280.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The designs' four widths, and the edges between them.
    #[test]
    fn the_width_decides_the_columns() {
        assert_eq!(Layout::of(1440.0), Layout { side: SIDEBAR, list: Some(400.0) });
        assert_eq!(Layout::of(1201.0).list, Some(400.0));
        assert_eq!(Layout::of(1200.0), Layout { side: SIDEBAR, list: Some(340.0) });
        assert_eq!(Layout::of(1000.0), Layout { side: SIDEBAR, list: Some(340.0) });
        assert_eq!(Layout::of(999.0), Layout { side: RAIL, list: Some(340.0) });
        assert_eq!(Layout::of(760.0), Layout { side: RAIL, list: Some(340.0) });
        assert_eq!(Layout::of(759.0), Layout { side: RAIL, list: None });
        assert_eq!(Layout::of(390.0), Layout { side: RAIL, list: None });
        assert!(Layout::of(800.0).rail());
        assert!(!Layout::of(1100.0).rail());
    }
}
