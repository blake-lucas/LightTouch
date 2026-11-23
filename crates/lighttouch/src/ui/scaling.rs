//! Drawing a guest-sized picture at the size it's shown, by the Screen scaling setting: the live screen and the
//! settings page's preview go through the same texture upload and mesh, so the preview shows what the device will.
//! Smooth and Crisp sample bilinearly, Sharp and Pixel perfect take the nearest pixel. Crisp (sharp-bilinear) first
//! repeats each pixel the whole number of times that reaches the size shown, then bilinear brings it down to that
//! size, so only about one screen pixel at each edge is blended, at any size (rounding the multiple down instead
//! would leave it as soft as Smooth below 2x).
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

/// Crisp's prescale: the least whole number of times a `size` picture reaches `shown_px` (its longer side shown, in
/// physical pixels). Other methods upload the picture as it is.
pub fn prescale_factor(scaling: Scaling, size: [usize; 2], shown_px: f32) -> usize {
    let long = size[0].max(size[1]);
    if scaling != Scaling::Crisp || long == 0 {
        return 1;
    }
    ((shown_px / long as f32 - 0.01).ceil().max(1.0) as usize).clamp(1, (MAX_SIDE / long).max(1))
}

/// `image` with each pixel repeated `k` times across and down.
pub fn prescale(image: ColorImage, k: usize) -> ColorImage {
    if k <= 1 {
        return image;
    }
    let [w, h] = image.size;
    let mut pixels = Vec::with_capacity(w * h * k * k);
    let mut row = Vec::with_capacity(w * k);
    for y in 0..h {
        row.clear();
        for &p in &image.pixels[y * w..(y + 1) * w] {
            row.extend(std::iter::repeat_n(p, k));
        }
        for _ in 0..k {
            pixels.extend_from_slice(&row);
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
        assert_eq!(prescale_factor(Scaling::Crisp, [320, 480], 1439.0), 3);
        assert_eq!(prescale_factor(Scaling::Crisp, [320, 480], 1440.0), 3);
        assert_eq!(prescale_factor(Scaling::Crisp, [320, 480], 1460.0), 4);
        assert_eq!(prescale_factor(Scaling::Crisp, [320, 480], 300.0), 1);
        assert_eq!(prescale_factor(Scaling::Crisp, [640, 960], 1300.0), 2);
        assert_eq!(prescale_factor(Scaling::Smooth, [320, 480], 2000.0), 1);
        assert_eq!(prescale_factor(Scaling::Crisp, [320, 480], 100_000.0), 8);
        let image = ColorImage::new([2, 1], vec![Color32::RED, Color32::BLUE]);
        let big = prescale(image, 2);
        assert_eq!(big.size, [4, 2]);
        assert_eq!(big.pixels, [Color32::RED, Color32::RED, Color32::BLUE, Color32::BLUE].repeat(2));
    }
}
