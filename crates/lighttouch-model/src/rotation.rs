//! Turning the device on the desktop's landscape screen. iOS lays its UI out in the panel's own scan: every panel
//! but the iPad's scans out portrait, whatever the orientation (a landscape app draws sideways), and the iPad's
//! scans out landscape with its portrait UI turned a quarter clockwise into it. So turning the drawn device turns
//! the picture with it and maps clicks and the tilt arrows back to the device's own axes. The device keeps lying
//! flat, face up.
use crate::tilt::TiltKeys;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Rotation {
    #[default]
    Portrait,
    /// Turned a quarter anticlockwise: the Home button on the right.
    HomeRight,
    /// Turned a quarter clockwise: the Home button on the left.
    HomeLeft,
}

impl Rotation {
    /// Portrait, then Home on the right, then Home on the left.
    pub fn next(self) -> Self {
        match self {
            Self::Portrait => Self::HomeRight,
            Self::HomeRight => Self::HomeLeft,
            Self::HomeLeft => Self::Portrait,
        }
    }

    pub fn landscape(self) -> bool {
        self != Self::Portrait
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Portrait => "Portrait",
            Self::HomeRight => "Landscape, Home on the right",
            Self::HomeLeft => "Landscape, Home on the left",
        }
    }

    /// A point on the screen as shown (`u` rightward, `v` downward, 0 to 1) in the guest's portrait coordinates
    /// (its upright axes, whatever way the device is turned).
    pub fn to_guest(self, u: f64, v: f64) -> (f64, f64) {
        match self {
            Self::Portrait => (u, v),
            Self::HomeRight => (1.0 - v, u),
            Self::HomeLeft => (v, 1.0 - u),
        }
    }

    /// The inverse of [`Self::to_guest`].
    pub fn from_guest(self, x: f64, y: f64) -> (f64, f64) {
        match self {
            Self::Portrait => (x, y),
            Self::HomeRight => (y, 1.0 - x),
            Self::HomeLeft => (1.0 - y, x),
        }
    }

    /// A point on the screen as shown, in the panel's own scan: the frame's pixels, and the space the engine takes
    /// touches in. [`Self::to_guest`] undoes the device's turn, then `turned` undoes the quarter-turn the guest
    /// gives its portrait UI on a landscape scan (the iPad's panel: UIKit turns a portrait UI a quarter clockwise
    /// into a panel wider than tall, so the scan stands a quarter-turn from upright).
    pub fn to_scan(self, u: f64, v: f64, turned: bool) -> (f64, f64) {
        let (x, y) = self.to_guest(u, v);
        if turned { (y, 1.0 - x) } else { (x, y) }
    }

    /// The inverse of [`Self::to_scan`].
    pub fn from_scan(self, x: f64, y: f64, turned: bool) -> (f64, f64) {
        let (x, y) = if turned { (1.0 - y, x) } else { (x, y) };
        self.from_guest(x, y)
    }

    /// A direction on the device in its portrait axes (x right, y down) as shown on the desktop.
    pub fn turn(self, dx: f32, dy: f32) -> (f32, f32) {
        match self {
            Self::Portrait => (dx, dy),
            Self::HomeRight => (dy, -dx),
            Self::HomeLeft => (-dy, dx),
        }
    }

    /// The arrows as the screen shows them, in the device's own: each still lowers the edge it points at.
    pub fn keys(self, keys: TiltKeys) -> TiltKeys {
        let TiltKeys { left, right, up, down } = keys;
        match self {
            Self::Portrait => keys,
            // The device's top is on the left and its right edge at the top.
            Self::HomeRight => TiltKeys { left: down, right: up, up: left, down: right },
            Self::HomeLeft => TiltKeys { left: up, right: down, up: right, down: left },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_round_trip_and_corners() {
        for r in [Rotation::Portrait, Rotation::HomeRight, Rotation::HomeLeft] {
            let (x, y) = r.to_guest(0.2, 0.7);
            let (u, v) = r.from_guest(x, y);
            assert!((u - 0.2).abs() < 1e-12 && (v - 0.7).abs() < 1e-12);
        }
        // Home on the right: the device's top-right corner shows at the top left.
        assert_eq!(Rotation::HomeRight.to_guest(0.0, 0.0), (1.0, 0.0));
        // Home on the left: its bottom-left corner does.
        assert_eq!(Rotation::HomeLeft.to_guest(0.0, 0.0), (0.0, 1.0));
        // The device's up (toward its top) points left, then right.
        assert_eq!(Rotation::HomeRight.turn(0.0, -1.0), (-1.0, -0.0));
        assert_eq!(Rotation::HomeLeft.turn(0.0, -1.0), (1.0, 0.0));
    }

    #[test]
    fn a_landscape_scan_stands_a_quarter_turn_from_upright() {
        // The iPad's panel (1024x768): its portrait UI is turned a quarter clockwise into it, so the screen's top
        // edge is the scan's left edge (bottom to top) and its top-left corner the scan's bottom-left.
        assert_eq!(Rotation::Portrait.to_scan(0.0, 0.0, true), (0.0, 1.0));
        assert_eq!(Rotation::Portrait.to_scan(1.0, 0.0, true), (0.0, 0.0));
        assert_eq!(Rotation::Portrait.to_scan(0.0, 1.0, true), (1.0, 1.0));
        // A portrait scan is the device's own axes, untouched.
        for r in [Rotation::Portrait, Rotation::HomeRight, Rotation::HomeLeft] {
            assert_eq!(r.to_scan(0.2, 0.7, false), r.to_guest(0.2, 0.7));
        }
        // Both kinds of scan round-trip, however the device is turned.
        for turned in [false, true] {
            for r in [Rotation::Portrait, Rotation::HomeRight, Rotation::HomeLeft] {
                let (x, y) = r.to_scan(0.2, 0.7, turned);
                let (u, v) = r.from_scan(x, y, turned);
                assert!((u - 0.2).abs() < 1e-12 && (v - 0.7).abs() < 1e-12);
            }
        }
    }

    #[test]
    fn arrows_lower_the_edge_they_point_at() {
        // Home on the right, the left arrow lowers the device's top: its "up" key.
        let left = TiltKeys { left: true, ..Default::default() };
        assert_eq!(Rotation::HomeRight.keys(left), TiltKeys { up: true, ..Default::default() });
        assert_eq!(Rotation::HomeLeft.keys(left), TiltKeys { down: true, ..Default::default() });
        let up = TiltKeys { up: true, ..Default::default() };
        assert_eq!(Rotation::HomeRight.keys(up), TiltKeys { right: true, ..Default::default() });
        assert_eq!(Rotation::HomeLeft.keys(up), TiltKeys { left: true, ..Default::default() });
    }
}
