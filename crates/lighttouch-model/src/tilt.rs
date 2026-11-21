//! The device's pose for its accelerometer and gyroscope, and tilting it from the keyboard (Android's tilt controls
//! without motion sensors). At rest it stands upright facing the player, turned the way the desktop shows it
//! ([`Rotation`]): turned on its side, it reads as held on its side, so a game that follows the device turns with it.
//! Leaning it rocks it side to side as shown (like a steering wheel) and tips its top toward or away from the player.
//! The arrow keys lean it up to `MAX_DEGREES`, easing there and back. Each step is an accelerometer sample (and a
//! gyroscope one, for boards that have it) in the guest's portrait axes: x right, y up, z toward the viewer, m/s² and
//! rad/s.

use crate::rotation::Rotation;
use serde::{Deserialize, Serialize};
use std::f64::consts::{FRAC_PI_2, FRAC_PI_4, PI};
use std::time::Instant;

/// How far a full lean goes from rest.
pub const MAX_DEGREES: f64 = 35.0;
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
    pose(Rest::Upright, roll, pitch, droll, dpitch)
}

/// How the device rests when nothing leans it, as the desktop shows it (a stick's Tilt binding).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rest {
    /// Standing upright, facing the player.
    #[default]
    Upright,
    /// Tipped back halfway to flat, as held in the hands.
    TippedBack,
    /// Lying face up on a table, its top away from the player.
    Flat,
    /// Lying face down, its top away from the player.
    FaceDown,
    /// Upright, turned a quarter onto its left side as shown.
    LeftSide,
    /// Upright, turned a quarter onto its right side as shown.
    RightSide,
    /// Upright, turned over onto its top.
    UpsideDown,
}

impl Rest {
    pub const ALL: [Rest; 7] = [
        Rest::Upright,
        Rest::TippedBack,
        Rest::Flat,
        Rest::FaceDown,
        Rest::LeftSide,
        Rest::RightSide,
        Rest::UpsideDown,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Rest::Upright => "Upright, facing you",
            Rest::TippedBack => "Tipped back, as held",
            Rest::Flat => "Flat on a table",
            Rest::FaceDown => "Face down",
            Rest::LeftSide => "On its left side",
            Rest::RightSide => "On its right side",
            Rest::UpsideDown => "Upside down",
        }
    }

    pub fn about(self) -> &'static str {
        match self {
            Rest::Upright => "Standing up and facing you, turned the way the screen is shown",
            Rest::TippedBack => "Tipped back halfway to flat, the way a device is held to play",
            Rest::Flat => "Lying face up, its top away from you: for games played on a table, like rolling a ball",
            Rest::FaceDown => "Lying face down, its top away from you",
            Rest::LeftSide => "Standing up, turned a quarter onto its left side as shown",
            Rest::RightSide => "Standing up, turned a quarter onto its right side as shown",
            Rest::UpsideDown => "Standing up, turned over onto its top",
        }
    }

    /// The pose's turn from upright, in the world's axes as shown (x right, y up, z toward the player).
    fn turn(self) -> M {
        match self {
            Rest::Upright => rz(0.0),
            Rest::TippedBack => rx(-FRAC_PI_4),
            Rest::Flat => rx(-FRAC_PI_2),
            Rest::FaceDown => mul(rz(PI), rx(-FRAC_PI_2)),
            Rest::LeftSide => rz(FRAC_PI_2),
            Rest::RightSide => rz(-FRAC_PI_2),
            Rest::UpsideDown => rz(PI),
        }
    }
}

type M = [[f64; 3]; 3];

fn rx(a: f64) -> M {
    let (s, c) = a.sin_cos();
    [[1.0, 0.0, 0.0], [0.0, c, -s], [0.0, s, c]]
}

fn rz(a: f64) -> M {
    let (s, c) = a.sin_cos();
    [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]]
}

fn mul(a: M, b: M) -> M {
    std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| a[i][k] * b[k][j]).sum()))
}

/// `m` transposed, times `v`: a world vector in the device's axes.
fn into_device(m: M, v: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|j| (0..3).map(|i| m[i][j] * v[i]).sum())
}

fn apply(m: M, v: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|i| (0..3).map(|k| m[i][k] * v[k]).sum())
}

/// The device resting as `rest`, then leaning in the world as [`lean`] does from upright: `roll` turns it clockwise
/// about the player's line of sight (its right side as shown goes down) and `pitch` about the left-right line (its
/// top tips toward the player, or up from a table). Returns (acceleration, angular velocity) in the axes of the
/// screen as shown.
pub fn pose(rest: Rest, roll: f64, pitch: f64, droll: f64, dpitch: f64) -> ([f64; 3], [f64; 3]) {
    let spin = rz(-roll);
    let device = mul(mul(spin, rx(pitch)), rest.turn());
    let acceleration = into_device(device, [0.0, GRAVITY, 0.0]);
    let pitching = apply(spin, [dpitch, 0.0, 0.0]);
    let angular_velocity = into_device(device, [pitching[0], pitching[1], pitching[2] - droll]);
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
        let max = MAX_DEGREES.to_radians();
        self.step_to(roll.clamp(-1.0, 1.0) * max, pitch.clamp(-1.0, 1.0) * max, Rest::Upright, rotation, now)
    }

    /// Ease toward leaning `roll` and `pitch` radians from `rest` (as [`pose`] leans).
    pub fn step_to(
        &mut self,
        roll_to: f64,
        pitch_to: f64,
        rest: Rest,
        rotation: Rotation,
        now: Instant,
    ) -> ([f64; 3], [f64; 3]) {
        let dt = self.at.map_or(0.0, |t| now.saturating_duration_since(t).as_secs_f64().min(0.1));
        self.at = Some(now);
        let reach = DEGREES_PER_SECOND.to_radians() * dt;
        let (roll, pitch) = (approach(self.roll, roll_to, reach), approach(self.pitch, pitch_to, reach));
        let (droll, dpitch) = if dt > 0.0 { ((roll - self.roll) / dt, (pitch - self.pitch) / dt) } else { (0.0, 0.0) };
        self.roll = roll;
        self.pitch = pitch;
        let (acceleration, angular_velocity) = pose(rest, roll, pitch, droll, dpitch);
        (rotation.to_guest_axes(acceleration), rotation.to_guest_axes(angular_velocity))
    }

    pub fn at_rest(&self) -> bool {
        self.roll == 0.0 && self.pitch == 0.0
    }

    /// How far it leans now (roll, pitch), radians.
    pub fn leaning(&self) -> (f64, f64) {
        (self.roll, self.pitch)
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

    #[test]
    fn each_rest_is_its_pose_and_leans_alike() {
        let g = GRAVITY;
        let rests = [
            (Rest::Upright, [0.0, g, 0.0]),
            (Rest::Flat, [0.0, 0.0, g]),
            (Rest::FaceDown, [0.0, 0.0, -g]),
            (Rest::LeftSide, [g, 0.0, 0.0]),
            (Rest::RightSide, [-g, 0.0, 0.0]),
            (Rest::UpsideDown, [0.0, -g, 0.0]),
        ];
        for (rest, at) in rests {
            assert!(close(pose(rest, 0.0, 0.0, 0.0, 0.0).0, at), "{rest:?}");
        }
        let back = pose(Rest::TippedBack, 0.0, 0.0, 0.0, 0.0).0;
        assert!(close(back, [0.0, g * FRAC_PI_4.cos(), g * FRAC_PI_4.sin()]));
        // Upright, it's the lean it always was.
        for (roll, pitch, droll, dpitch) in [(0.4, -0.3, 0.7, -1.2), (-0.6, 0.5, -0.2, 0.9)] {
            let a = pose(Rest::Upright, roll, pitch, droll, dpitch);
            let acceleration = [-g * f64::sin(roll), g * roll.cos() * pitch.cos(), -g * roll.cos() * pitch.sin()];
            let angular_velocity = [dpitch, -droll * pitch.sin(), -droll * pitch.cos()];
            assert!(close(a.0, acceleration) && close(a.1, angular_velocity));
        }
        // Flat, rolling right lowers its right side (gravity's reaction leans to its left, as upright), and pitching
        // raises its top (the reaction leans toward its top), so a ball rolls right and down.
        let (a, _) = pose(Rest::Flat, 0.3, 0.0, 0.0, 0.0);
        assert!(a[0] < 0.0 && a[1].abs() < 1e-9);
        let (a, _) = pose(Rest::Flat, 0.0, 0.3, 0.0, 0.0);
        assert!(a[1] > 0.0 && a[2] > 0.0 && a[0].abs() < 1e-9);
        // Every rest's gyroscope agrees with its accelerometer.
        let dt = 1e-6;
        for rest in Rest::ALL {
            let (roll, pitch, droll, dpitch) = (0.3, -0.2, 0.8, -0.5);
            let (a0, w) = pose(rest, roll, pitch, droll, dpitch);
            let (a1, _) = pose(rest, roll + droll * dt, pitch + dpitch * dt, 0.0, 0.0);
            let da = [(a1[0] - a0[0]) / dt, (a1[1] - a0[1]) / dt, (a1[2] - a0[2]) / dt];
            let cross = [w[1] * a0[2] - w[2] * a0[1], w[2] * a0[0] - w[0] * a0[2], w[0] * a0[1] - w[1] * a0[0]];
            assert!(close_to(da, cross.map(|c| -c)), "{rest:?}");
        }
    }

    /// The turn about the viewer's line of sight, from a gyro sample in the guest's axes.
    fn g_z(g: [f64; 3], r: Rotation) -> f64 {
        let z = r.to_guest_axes([0.0, 0.0, 1.0]);
        g.iter().zip(z).map(|(g, z)| g * z).sum()
    }
}
