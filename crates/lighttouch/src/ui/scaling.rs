//! Drawing a guest-sized picture at the size it's shown, by the Screen scaling setting: the live screen and the
//! settings page's preview go through the same texture upload and mesh, so the preview shows what the device will.
//! Smooth and Crisp sample bilinearly, Sharp and Pixel perfect take the nearest pixel. Crisp (sharp-bilinear) first
//! repeats each pixel a whole number of times, the largest that fits but at least 2 once the picture is shown larger
//! than it is (1 would be as soft as Smooth), then bilinear brings it to the size shown, so only about a screen pixel
//! at each edge is blended. The Android presenter keeps 1 below 2x: it draws the multiple on the CPU, and a Retina
//! screen at 2x would cost it 4 times the pixels.
use eframe::egui::{self, Color32, ColorImage, Rect, TextureHandle, TextureOptions};
use lighttouch_model::rotation::Rotation;
use lighttouch_model::settings::Scaling;

/// The largest prescaled texture side Crisp makes (a 320-pixel-wide screen at 12x).
const MAX_SIDE: usize = 4096;

pub fn texture_options(scaling: Scaling) -> TextureOptions {
    match scaling {
        Scaling::Smooth | Scaling::Crisp => TextureOptions::LINEAR,
        Scaling::Sharp | Scaling::PixelPerfect => TextureOptions::NEAREST,
    }
}

/// Crisp's prescale for a `size` picture whose longer side shows `shown_px` physical pixels: the largest whole
/// multiple that fits, but 2 between 1x and 2x. Other methods upload the picture as it is.
pub fn prescale_factor(scaling: Scaling, size: [usize; 2], shown_px: f32) -> usize {
    let long = size[0].max(size[1]);
    if scaling != Scaling::Crisp || long == 0 {
        return 1;
    }
    let scale = shown_px / long as f32;
    let whole = (scale + 0.01).floor() as usize;
    let k = if whole >= 2 {
        whole
    } else if scale > 1.01 {
        2
    } else {
        1
    };
    k.clamp(1, (MAX_SIDE / long).max(1))
}

/// `image` with each pixel repeated `k` times across and down.
pub fn prescale(image: ColorImage, k: usize) -> ColorImage {
    if k <= 1 {
        return image;
    }
    let [w, h] = image.size;
    let mut pixels = vec![Color32::TRANSPARENT; w * h * k * k];
    let stride = w * k;
    for (y, rows) in pixels.chunks_exact_mut(stride * k).enumerate() {
        let (first, rest) = rows.split_at_mut(stride);
        for (out, &p) in first.chunks_exact_mut(k).zip(&image.pixels[y * w..(y + 1) * w]) {
            out.fill(p);
        }
        for row in rest.chunks_exact_mut(stride) {
            row.copy_from_slice(first);
        }
    }
    ColorImage::new([w * k, h * k], pixels)
}

/// A texture of one picture, uploaded again when the method or Crisp's factor changes.
#[derive(Default)]
pub struct Scaled {
    texture: Option<TextureHandle>,
    uploaded: Option<(Scaling, usize)>,
}

impl Scaled {
    /// Upload `image` (when it's new) or re-upload it for `scaling` at `shown_px` (its longer side as shown, in
    /// physical pixels) when either changed. `image` is only called when an upload is needed.
    pub fn update(
        &mut self,
        ctx: &egui::Context,
        name: &str,
        fresh: bool,
        scaling: Scaling,
        size: [usize; 2],
        shown_px: f32,
        image: impl FnOnce() -> ColorImage,
    ) {
        let k = prescale_factor(scaling, size, shown_px);
        if !fresh && self.texture.is_some() && self.uploaded == Some((scaling, k)) {
            return;
        }
        let image = prescale(image(), k);
        let options = texture_options(scaling);
        match &mut self.texture {
            Some(texture) => texture.set(image, options),
            None => self.texture = Some(ctx.load_texture(name, image, options)),
        }
        self.uploaded = Some((scaling, k));
    }

    pub fn clear(&mut self) {
        self.texture = None;
        self.uploaded = None;
    }

    pub fn ready(&self) -> bool {
        self.texture.is_some()
    }

    /// Draw into `rect`, each corner showing the picture's pixel `rotation` puts there (`turned`: a landscape scan
    /// shown upright, as the iPad's).
    pub fn paint(&self, painter: &egui::Painter, rect: Rect, rotation: Rotation, turned: bool) -> bool {
        let Some(texture) = &self.texture else { return false };
        let mut mesh = egui::Mesh::with_texture(texture.id());
        for corner in [rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom()] {
            let u = ((corner.x - rect.left()) / rect.width()) as f64;
            let v = ((corner.y - rect.top()) / rect.height()) as f64;
            let (x, y) = rotation.to_scan(u, v, turned);
            mesh.vertices.push(egui::epaint::Vertex {
                pos: corner,
                uv: egui::pos2(x as f32, y as f32),
                color: Color32::WHITE,
            });
        }
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(0, 2, 3);
        painter.add(egui::Shape::mesh(mesh));
        true
    }
}

/// The longer side of `rect` in physical pixels.
pub fn shown_px(rect: Rect, pixels_per_point: f32) -> f32 {
    rect.width().max(rect.height()) * pixels_per_point
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crisp_prescales_by_whole_numbers() {
        assert_eq!(prescale_factor(Scaling::Crisp, [320, 480], 1400.0), 2);
        assert_eq!(prescale_factor(Scaling::Crisp, [320, 480], 1440.0), 3);
        assert_eq!(prescale_factor(Scaling::Crisp, [320, 480], 1900.0), 3);
        assert_eq!(prescale_factor(Scaling::Crisp, [320, 480], 300.0), 1);
        assert_eq!(prescale_factor(Scaling::Crisp, [640, 960], 1300.0), 2);
        assert_eq!(prescale_factor(Scaling::Crisp, [640, 960], 961.0), 1);
        assert_eq!(prescale_factor(Scaling::Smooth, [320, 480], 2000.0), 1);
        assert_eq!(prescale_factor(Scaling::Crisp, [320, 480], 100_000.0), 8);
        let image = ColorImage::new([2, 1], vec![Color32::RED, Color32::BLUE]);
        let big = prescale(image, 2);
        assert_eq!(big.size, [4, 2]);
        assert_eq!(big.pixels, [Color32::RED, Color32::RED, Color32::BLUE, Color32::BLUE].repeat(2));
    }
}

#[cfg(test)]
mod timing {
    use super::*;

    /// `cargo test --release -p lighttouch prescale_cost -- --ignored --nocapture`: Crisp's CPU cost per frame.
    #[test]
    #[ignore]
    fn prescale_cost() {
        for (w, h, k) in [(320, 480, 1), (640, 960, 1), (320, 480, 2), (320, 480, 4), (640, 960, 2), (1024, 768, 2)] {
            let rgba = vec![200u8; w * h * 4];
            let start = std::time::Instant::now();
            for _ in 0..50 {
                let image = prescale(ColorImage::from_rgba_unmultiplied([w, h], &rgba), k);
                std::hint::black_box(image);
            }
            println!("{w}x{h} at {k}x: {:.2} ms", start.elapsed().as_secs_f64() * 1000.0 / 50.0);
        }
    }
}
