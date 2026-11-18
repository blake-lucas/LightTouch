//! iPads at their real size, for toys and pieces used on the screen. The display's density, measured once against a
//! card or ruler, turns the device's millimetres into points. It is kept in physical pixels, so the system's scaling
//! and the app's zoom don't change the result; only another display (or another resolution) needs a new measure.
use serde::{Deserialize, Serialize};

/// A bank or ID card's width (ISO/IEC 7810 ID-1), in millimetres.
pub const CARD_WIDTH: f32 = 85.60;
pub const CARD_HEIGHT: f32 = 53.98;
pub const CARD_CORNER: f32 = 3.18;

/// The densities a measure may take: about 38 to 760 pixels per inch.
pub const MIN_PX_PER_MM: f32 = 1.5;
pub const MAX_PX_PER_MM: f32 = 30.0;

const MM_PER_INCH: f32 = 25.4;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DisplayMeasure {
    /// Physical pixels per millimetre.
    pub px_per_mm: f32,
    /// The monitor it was measured on, in physical pixels, when the system said.
    #[serde(default)]
    pub monitor: Option<[u32; 2]>,
    /// Not measured: the first guess, kept when real size was turned on before saving a measure.
    #[serde(default)]
    pub estimate: bool,
}

impl DisplayMeasure {
    /// Whether `monitor` (in physical pixels) is another display than the one measured, as far as can be told.
    pub fn other_display(&self, monitor: Option<[u32; 2]>) -> bool {
        matches!((self.monitor, monitor), (Some(a), Some(b)) if a != b)
    }
}

pub fn clamp(px_per_mm: f32) -> f32 {
    if px_per_mm.is_finite() { px_per_mm.clamp(MIN_PX_PER_MM, MAX_PX_PER_MM) } else { guess(1.0) }
}

/// Points per millimetre for a density, at the window's current pixels per point (system scaling × zoom).
pub fn points_per_mm(px_per_mm: f32, pixels_per_point: f32) -> f32 {
    px_per_mm / pixels_per_point.max(0.1)
}

/// A first guess before measuring: systems scale so that a point is about 1/96 inch.
pub fn guess(native_pixels_per_point: f32) -> f32 {
    clamp(96.0 / MM_PER_INCH * native_pixels_per_point.max(0.5))
}

pub fn per_inch(px_per_mm: f32) -> f32 {
    px_per_mm * MM_PER_INCH
}

pub fn from_per_inch(px_per_inch: f32) -> f32 {
    clamp(px_per_inch / MM_PER_INCH)
}

/// The diagonal, in inches, that a density makes of a monitor's pixels.
pub fn diagonal_inches(monitor: [u32; 2], px_per_mm: f32) -> f32 {
    let (w, h) = (monitor[0] as f32, monitor[1] as f32);
    (w * w + h * h).sqrt() / px_per_mm / MM_PER_INCH
}

/// The density of a monitor of so many pixels with a diagonal of `inches`.
pub fn from_diagonal(monitor: [u32; 2], inches: f32) -> f32 {
    let (w, h) = (monitor[0] as f32, monitor[1] as f32);
    clamp((w * w + h * h).sqrt() / (inches.max(1.0) * MM_PER_INCH))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_measure_holds_through_scaling_and_zoom() {
        // 5.67 px/mm (144 ppi) at 150% scaling is 3.78 points per mm; zooming in to 120% leaves fewer points per mm.
        let at_150 = points_per_mm(5.67, 1.5);
        assert!((at_150 - 3.78).abs() < 1e-3);
        assert!((points_per_mm(5.67, 1.8) * 1.8 - 5.67).abs() < 1e-4);
    }

    #[test]
    fn diagonal_and_density_agree() {
        // A 27-inch 2560 × 1440 monitor is about 108.8 ppi.
        let density = from_diagonal([2560, 1440], 27.0);
        assert!((per_inch(density) - 108.79).abs() < 0.05);
        assert!((diagonal_inches([2560, 1440], density) - 27.0).abs() < 1e-3);
    }

    #[test]
    fn guesses_and_limits() {
        assert!((per_inch(guess(1.0)) - 96.0).abs() < 1e-3);
        assert!((per_inch(guess(2.0)) - 192.0).abs() < 1e-3);
        assert_eq!(clamp(0.0), MIN_PX_PER_MM);
        assert_eq!(clamp(f32::NAN), guess(1.0));
        let measure = DisplayMeasure { px_per_mm: 4.0, monitor: Some([1920, 1080]), estimate: false };
        assert!(!measure.other_display(None));
        assert!(!measure.other_display(Some([1920, 1080])));
        assert!(measure.other_display(Some([2560, 1440])));
    }
}
