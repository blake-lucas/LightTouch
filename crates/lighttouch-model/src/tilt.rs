//! The device's pose for its accelerometer and gyroscope, and tilting it from the keyboard (Android's tilt controls
//! without motion sensors). At rest it stands upright facing the player, turned the way the desktop shows it
//! ([`Rotation`]): turned on its side, it reads as held on its side, so a game that follows the device turns with it.
//! Leaning it rocks it side to side as shown (like a steering wheel) and tips its top toward or away from the player.
//! The arrow keys lean it up to `MAX_DEGREES`, easing there and back. Each step is an accelerometer sample (and a
//! gyroscope one, for boards that have it) in the guest's portrait axes: x right, y up, z toward the viewer, m/s² and
//! rad/s.

use crate::rotation::Rotation;
use std::time::Instant;

const MAX_DEGREES: f64 = 35.0;
const DEGREES_PER_SECOND: f64 = 150.0;
pub const GRAVITY: f64 = 9.80665;

/// The arrow keys held, as the screen shows them.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TiltKeys {
    pub left: bool,
    pub right: bool,
    pub up: bool,
    pub down: bool,
}

/// The accelerometer at rest: the device upright, turned as shown.
pub fn rest(rotation: Rotation) -> [f64; 3] {
    rotation.to_guest_axes([0.0, GRAVITY, 0.0])
}

/// The device leaning from upright, in the axes of the screen as shown (x right, y up, z toward the viewer): `roll`
/// rocks it clockwise (its right side down) and `pitch` tips its top toward the viewer, radians, changing at `droll`
/// and `dpitch` a second. Returns (acceleration, angular velocity). The device is the upright pose rocked about the
/// viewer's line of sight, then tipped about its own x axis; it measures gravity's reaction.
pub fn lean(roll: f64, pitch: f64, droll: f64, dpitch: f64) -> ([f64; 3], [f64; 3]) {
    let acceleration = [-GRAVITY * roll.sin(), GRAVITY * roll.cos() * pitch.cos(), -GRAVITY * roll.cos() * pitch.sin()];
    let angular_velocity = [dpitch, -droll * pitch.sin(), -droll * pitch.cos()];
    (acceleration, angular_velocity)
}

#[derive(Debug, Default)]
pub struct Tilt {
    /// Rocked clockwise as shown, radians: its right side down.
    roll: f64,
    /// Its top tipped toward the viewer, radians.
    pitch: f64,
    at: Option<Instant>,
}

impl Tilt {
    /// Ease toward what `keys` ask for and return (acceleration, angular velocity) in the guest's axes. Left and
    /// right rock the device that way; up tips its top away (toward lying face up), down toward the player.
    pub fn step(&mut self, keys: TiltKeys, rotation: Rotation, now: Instant) -> ([f64; 3], [f64; 3]) {
        let roll = (keys.right as i8 - keys.left as i8) as f64;
        let pitch = (keys.down as i8 - keys.up as i8) as f64;
        self.step_toward(roll, pitch, rotation, now)
    }

    /// The same toward a lean in between (a controller's stick): `roll` and `pitch` from -1 to 1 of the full lean,
    /// as shown (positive `roll` lowers the right side, positive `pitch` tips the top toward the player).
    pub fn step_toward(&mut self, roll: f64, pitch: f64, rotation: Rotation, now: Instant) -> ([f64; 3], [f64; 3]) {
        let dt = self.at.map_or(0.0, |t| now.saturating_duration_since(t).as_secs_f64().min(0.1));
        self.at = Some(now);
        let max = MAX_DEGREES.to_radians();
        let roll_to = roll.clamp(-1.0, 1.0) * max;
        let pitch_to = pitch.clamp(-1.0, 1.0) * max;
        let reach = DEGREES_PER_SECOND.to_radians() * dt;
        let (roll, pitch) = (approach(self.roll, roll_to, reach), approach(self.pitch, pitch_to, reach));
        let (droll, dpitch) = if dt > 0.0 { ((roll - self.roll) / dt, (pitch - self.pitch) / dt) } else { (0.0, 0.0) };
        self.roll = roll;
        self.pitch = pitch;
        let (acceleration, angular_velocity) = lean(roll, pitch, droll, dpitch);
        (rotation.to_guest_axes(acceleration), rotation.to_guest_axes(angular_velocity))
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

    const ROTATIONS: [Rotation; 3] = [Rotation::Portrait, Rotation::HomeRight, Rotation::HomeLeft];

    fn close(a: [f64; 3], b: [f64; 3]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-9)
    }

    #[test]
    fn rests_upright_as_shown() {
        assert_eq!(rest(Rotation::Portrait), [0.0, GRAVITY, 0.0]);
        // Home on the right: the device's right edge is up. Home on the left: its left edge.
        assert_eq!(rest(Rotation::HomeRight), [GRAVITY, 0.0, 0.0]);
        assert_eq!(rest(Rotation::HomeLeft), [-GRAVITY, 0.0, 0.0]);
        for r in ROTATIONS {
            let mut t = Tilt::default();
            let (a, g) = t.step(TiltKeys::default(), r, Instant::now());
            assert!(close(a, rest(r)) && g == [0.0; 3]);
        }
    }

    /// The gyroscope agrees with the accelerometer: gravity's reaction, fixed in the world, turns against the device,
    /// d(a)/dt = -ω × a. Otherwise iOS's sensor fusion keeps pulling the two back together (a game re-levelling).
    #[test]
    fn gyro_agrees_with_the_accelerometer() {
        let dt = 1e-6;
        for (roll, pitch, droll, dpitch) in
            [(0.0, 0.0, 1.0, 0.0), (0.0, 0.0, 0.0, 1.0), (0.4, -0.3, 0.7, -1.2), (-0.6, 0.5, -0.2, 0.9)]
        {
            let (a0, w) = lean(roll, pitch, droll, dpitch);
            let (a1, _) = lean(roll + droll * dt, pitch + dpitch * dt, 0.0, 0.0);
            let da = [(a1[0] - a0[0]) / dt, (a1[1] - a0[1]) / dt, (a1[2] - a0[2]) / dt];
            let cross = [w[1] * a0[2] - w[2] * a0[1], w[2] * a0[0] - w[0] * a0[2], w[0] * a0[1] - w[1] * a0[0]];
            assert!(close_to(da, cross.map(|c| -c)), "{da:?} vs {cross:?} at {roll} {pitch}");
            // Turned as shown, both turn alike: a rotation keeps the relation.
            for r in ROTATIONS {
                let (a, w) = (r.to_guest_axes(a0), r.to_guest_axes(w));
                let b = r.to_guest_axes(a1);
                let da = [(b[0] - a[0]) / dt, (b[1] - a[1]) / dt, (b[2] - a[2]) / dt];
                let cross = [w[1] * a[2] - w[2] * a[1], w[2] * a[0] - w[0] * a[2], w[0] * a[1] - w[1] * a[0]];
                assert!(close_to(da, cross.map(|c| -c)));
            }
        }
    }

    fn close_to(a: [f64; 3], b: [f64; 3]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-3 * GRAVITY)
    }

    #[test]
    fn rocks_as_shown_and_returns() {
        let start = Instant::now();
        for r in ROTATIONS {
            let mut t = Tilt::default();
            t.step(TiltKeys::default(), r, start);
            // Right lowers the right side as shown: gravity's reaction leans toward the left as shown, by the same
            // amount a flat device's roll gave, and the device turns clockwise as shown (about -z).
            let right = TiltKeys { right: true, ..Default::default() };
            let mut now = start;
            let mut last = ([0.0; 3], [0.0; 3]);
            for _ in 0..20 {
                now += Duration::from_millis(50);
                last = t.step(right, r, now);
            }
            let max = MAX_DEGREES.to_radians();
            let shown = lean(max, 0.0, 0.0, 0.0).0;
            assert!(close(last.0, r.to_guest_axes(shown)));
            assert!((shown[0] + GRAVITY * max.sin()).abs() < 1e-9 && shown[2].abs() < 1e-9);
            let mut t2 = Tilt::default();
            t2.step(TiltKeys::default(), r, start);
            let (_, g) = t2.step(right, r, start + Duration::from_millis(50));
            assert!(close(g, r.to_guest_axes([0.0, 0.0, g_z(g, r)])) && g_z(g, r) < 0.0);
            // Up tips the top away: the screen faces up a little (+z as shown).
            let mut t3 = Tilt::default();
            t3.step(TiltKeys::default(), r, start);
            let (a, _) = t3.step(TiltKeys { up: true, ..Default::default() }, r, start + Duration::from_millis(50));
            let z = r.to_guest_axes([0.0, 0.0, 1.0]);
            assert!(a.iter().zip(z).map(|(a, z)| a * z).sum::<f64>() > 0.0);
            for i in 2..40 {
                t.step(TiltKeys::default(), r, now + Duration::from_millis(50 * i));
            }
            assert!(t.at_rest());
        }
    }

    /// The turn about the viewer's line of sight, from a gyro sample in the guest's axes.
    fn g_z(g: [f64; 3], r: Rotation) -> f64 {
        let z = r.to_guest_axes([0.0, 0.0, 1.0]);
        g.iter().zip(z).map(|(g, z)| g * z).sum()
    }
}
