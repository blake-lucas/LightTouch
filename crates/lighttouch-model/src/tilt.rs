//! Tilting the device from the keyboard, Android's tilt controls without motion sensors: the arrow keys lean it
//! up to `MAX_DEGREES` toward that side, easing there and back, and each step is an accelerometer sample (and a
//! gyroscope one, for boards that have it) in the guest's portrait axes: x right, y up, z toward the viewer, m/s²
//! and rad/s. At rest the device lies flat, face up.

use std::time::Instant;

const MAX_DEGREES: f64 = 35.0;
const DEGREES_PER_SECOND: f64 = 150.0;
const GRAVITY: f64 = 9.80665;

/// The arrow keys held.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TiltKeys {
    pub left: bool,
    pub right: bool,
    pub up: bool,
    pub down: bool,
}

#[derive(Debug, Default)]
pub struct Tilt {
    /// About the device's y axis, radians: positive lowers the right edge.
    roll: f64,
    /// About the device's x axis, radians: positive raises the top edge.
    pitch: f64,
    at: Option<Instant>,
}

impl Tilt {
    /// Ease toward what `keys` ask for and return (acceleration, angular velocity). The arrow says which edge
    /// goes down, so something rolling on the screen rolls that way.
    pub fn step(&mut self, keys: TiltKeys, now: Instant) -> ([f64; 3], [f64; 3]) {
        let dt = self.at.map_or(0.0, |t| now.saturating_duration_since(t).as_secs_f64().min(0.1));
        self.at = Some(now);
        let max = MAX_DEGREES.to_radians();
        let roll_to = (keys.right as i8 - keys.left as i8) as f64 * max;
        let pitch_to = (keys.down as i8 - keys.up as i8) as f64 * max;
        let reach = DEGREES_PER_SECOND.to_radians() * dt;
        let (roll, pitch) = (approach(self.roll, roll_to, reach), approach(self.pitch, pitch_to, reach));
        let (droll, dpitch) = if dt > 0.0 { ((roll - self.roll) / dt, (pitch - self.pitch) / dt) } else { (0.0, 0.0) };
        self.roll = roll;
        self.pitch = pitch;
        // The device is the world turned by roll about y, then pitch about x; it measures gravity's reaction.
        let acceleration =
            [-GRAVITY * roll.sin(), GRAVITY * roll.cos() * pitch.sin(), GRAVITY * roll.cos() * pitch.cos()];
        let angular_velocity = [dpitch, droll * pitch.cos(), -droll * pitch.sin()];
        (acceleration, angular_velocity)
    }

    pub fn at_rest(&self) -> bool {
        self.roll == 0.0 && self.pitch == 0.0
    }
}

fn approach(from: f64, to: f64, reach: f64) -> f64 {
    if (to - from).abs() <= reach { to } else { from + reach.copysign(to - from) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn leans_and_returns() {
        let mut t = Tilt::default();
        let start = Instant::now();
        let (a, _) = t.step(TiltKeys::default(), start);
        assert_eq!(a, [0.0, 0.0, GRAVITY]);
        // Left lowers the left edge: gravity's reaction leans toward +x, and the gyro turns about -y.
        let left = TiltKeys { left: true, ..Default::default() };
        let (a, g) = t.step(left, start + Duration::from_millis(50));
        assert!(a[0] > 0.0 && a[1].abs() < 1e-9 && g[1] < 0.0);
        let mut now = start + Duration::from_millis(50);
        for _ in 0..20 {
            now += Duration::from_millis(50);
            t.step(left, now);
        }
        let (a, g) = t.step(left, now + Duration::from_millis(50));
        assert!((a[0] - GRAVITY * MAX_DEGREES.to_radians().sin()).abs() < 1e-9 && g == [0.0, 0.0, 0.0]);
        // Up lowers the top edge: -y.
        let mut t = Tilt::default();
        t.step(TiltKeys::default(), start);
        let (a, _) = t.step(TiltKeys { up: true, ..Default::default() }, start + Duration::from_millis(50));
        assert!(a[1] < 0.0 && a[0].abs() < 1e-9);
        for i in 2..40 {
            t.step(TiltKeys::default(), start + Duration::from_millis(50 * i));
        }
        assert!(t.at_rest());
    }
}
