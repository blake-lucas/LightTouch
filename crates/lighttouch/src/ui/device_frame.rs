//! The drawn device around the guest screen (port of the Android DeviceFrame): outlines in millimetres, the screen
//! placed at the panel's exact aspect so the guest never letterboxes, and real Home, Sleep/Wake and volume buttons.
use eframe::egui::{self, Color32, CornerRadius, Mesh, Painter, Pos2, Rect, Shape, Stroke, Vec2, epaint, pos2, vec2};
use lighttouch_model::rotation::Rotation;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Skin {
    pub width: f32,
    pub height: f32,
    pub corner: f32,
    pub screen_left: f32,
    pub screen_top: f32,
    pub screen_width: f32,
    pub screen_height: f32,
    pub home_center_y: f32,
    pub home_diameter: f32,
    /// The metal edge seen around the glass from the front.
    pub rim_width: f32,
    /// Sleep/Wake on the top edge: its left end and length.
    pub power: (f32, f32),
    /// The volume buttons' side (left on the phones, right on the iPad), and each one's top and length.
    pub volume_right: bool,
    pub volume_up: (f32, f32),
    pub volume_down: (f32, f32),
    pub face: Color32,
    pub rim: [Color32; 3],
}

const fn rgb(v: u32) -> Color32 {
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

impl Skin {
    /// iPod touch 2nd generation: 110 x 61.8 mm, 3.5" 320x480 panel, black glass, chrome back.
    pub const IPOD_TOUCH_2: Skin = Skin {
        width: 61.8,
        height: 110.0,
        corner: 8.5,
        screen_left: 5.95,
        screen_top: 17.6,
        screen_width: 49.9,
        screen_height: 74.8,
        home_center_y: 101.4,
        home_diameter: 10.5,
        rim_width: 0.7,
        power: (38.32, 12.36),
        volume_right: false,
        volume_up: (14.3, 7.15),
        volume_down: (23.1, 7.15),
        face: rgb(0x0B0B0C),
        rim: [rgb(0xE8E8EA), rgb(0x8E8E93), rgb(0xD1D1D6)],
    };
    /// iPod touch 4th generation: 111 x 58.9 mm, flatter rim.
    pub const IPOD_TOUCH_4: Skin = Skin {
        width: 58.9,
        height: 111.0,
        corner: 7.5,
        screen_left: 4.5,
        screen_top: 18.1,
        screen_width: 49.9,
        screen_height: 74.8,
        home_center_y: 102.3,
        home_diameter: 9.6,
        rim_width: 0.7,
        power: (36.52, 11.78),
        volume_right: false,
        volume_up: (14.43, 7.21),
        volume_down: (23.31, 7.21),
        face: rgb(0x0B0B0C),
        rim: [rgb(0xD9D9DC), rgb(0x9A9AA0), rgb(0xC7C7CC)],
    };
    pub const IPHONE: Skin = Skin {
        width: 62.1,
        height: 115.5,
        corner: 9.0,
        screen_left: 6.1,
        screen_top: 20.3,
        screen_width: 49.9,
        screen_height: 74.8,
        home_center_y: 105.6,
        home_diameter: 10.5,
        rim_width: 0.7,
        power: (38.5, 12.42),
        volume_right: false,
        volume_up: (15.02, 7.51),
        volume_down: (24.25, 7.51),
        face: rgb(0x0B0B0C),
        rim: [rgb(0xC7C7CC), rgb(0x6E6E73), rgb(0xAEAEB2)],
    };
    /// iPhone 5: 123.8 x 58.6 mm, a 4" 640x1136 panel at 326 ppi (49.9 x 88.5 mm), black glass, slate rim.
    pub const IPHONE_5: Skin = Skin {
        width: 58.6,
        height: 123.8,
        corner: 8.5,
        screen_left: 4.35,
        screen_top: 17.65,
        screen_width: 49.9,
        screen_height: 88.5,
        home_center_y: 115.0,
        home_diameter: 10.9,
        rim_width: 0.7,
        power: (38.0, 11.5),
        volume_right: false,
        volume_up: (22.0, 7.5),
        volume_down: (32.0, 7.5),
        face: rgb(0x0B0B0C),
        rim: [rgb(0x5A5A5E), rgb(0x2C2C2E), rgb(0x48484A)],
    };
    /// iPad (first generation): 242.8 x 189.7 mm, 9.7" 4:3 panel (197.1 x 147.8 mm) centred in the glass, a wide
    /// aluminium rim, Sleep/Wake at the top right and the volume rocker on the right edge below the side switch.
    pub const IPAD: Skin = Skin {
        width: 189.7,
        height: 242.8,
        corner: 13.0,
        screen_left: 20.95,
        screen_top: 22.85,
        screen_width: 147.8,
        screen_height: 197.1,
        home_center_y: 231.4,
        home_diameter: 11.5,
        rim_width: 2.4,
        power: (156.0, 12.0),
        volume_right: true,
        volume_up: (45.0, 12.0),
        volume_down: (57.5, 12.0),
        face: rgb(0x0B0B0C),
        rim: [rgb(0xD1D1D6), rgb(0x8E8E93), rgb(0xC7C7CC)],
    };

    pub fn for_board(board: Option<&str>) -> Skin {
        match board {
            Some(b) if b.starts_with("n81") => Self::IPOD_TOUCH_4,
            Some(b) if b.starts_with("n42") => Self::IPHONE_5,
            Some(b) if b.starts_with("k48") => Self::IPAD,
            Some(b) if ["m68", "n82", "n88", "n90"].iter().any(|p| b.starts_with(p)) => Self::IPHONE,
            _ => Self::IPOD_TOUCH_2,
        }
    }

    /// Screen aspect (width / height).
    pub fn screen_aspect(&self) -> f32 {
        self.screen_width / self.screen_height
    }

    /// The screen's aspect as shown, turned to `rotation`.
    pub fn shown_aspect(&self, rotation: Rotation) -> f32 {
        if rotation.landscape() { 1.0 / self.screen_aspect() } else { self.screen_aspect() }
    }
}

/// Hardware button ids shared with the runtime.
pub const HOME: u32 = 0;
pub const POWER: u32 = 1;
pub const VOLUME_UP: u32 = 2;
pub const VOLUME_DOWN: u32 = 3;

/// Where everything lands for a given space.
#[derive(Debug, Clone, Copy)]
pub struct Layout {
    pub body: Rect,
    pub screen: Rect,
    pub home: Rect,
    /// Side buttons as drawn, and their (larger) hit areas: [Power, Volume up, Volume down].
    pub side: [(u32, Rect, Rect); 3],
    pub scale: f32,
    pub corner: f32,
}

const MARGIN: f32 = 3.0;

pub fn layout(skin: &Skin, space: Rect) -> Layout {
    let scale = (space.width() / (skin.width + 2.0 * MARGIN)).min(space.height() / (skin.height + 2.0 * MARGIN));
    layout_at(skin, space.center(), scale)
}

/// The device drawn at `scale` points per millimetre, centred on `center`.
pub fn layout_at(skin: &Skin, center: Pos2, scale: f32) -> Layout {
    let outer = Rect::from_center_size(center, vec2(skin.width + 2.0 * MARGIN, skin.height + 2.0 * MARGIN) * scale);
    let mm = |x: f32, y: f32| outer.min + vec2(x, y) * scale;
    let body = Rect::from_min_size(mm(MARGIN, MARGIN), vec2(skin.width, skin.height) * scale);
    let screen = Rect::from_min_size(
        mm(MARGIN + skin.screen_left, MARGIN + skin.screen_top),
        vec2(skin.screen_width, skin.screen_height) * scale,
    );
    let home = Rect::from_center_size(
        mm(MARGIN + skin.width / 2.0, MARGIN + skin.home_center_y),
        Vec2::splat(skin.home_diameter * scale),
    );
    // Each drawn 2 mm deep, tucked under the body so ~1.3 mm shows.
    let volume_x = if skin.volume_right { MARGIN + skin.width - 0.7 } else { MARGIN - 1.3 };
    let drawn = [
        (POWER, MARGIN + skin.power.0, MARGIN - 1.4, skin.power.1, 2.0),
        (VOLUME_UP, volume_x, MARGIN + skin.volume_up.0, 2.0, skin.volume_up.1),
        (VOLUME_DOWN, volume_x, MARGIN + skin.volume_down.0, 2.0, skin.volume_down.1),
    ];
    let side = drawn.map(|(id, x, y, w, h)| {
        let rect = Rect::from_min_size(mm(x, y), vec2(w, h) * scale);
        let hit = Rect::from_center_size(rect.center(), vec2(rect.width().max(28.0), rect.height().max(28.0)));
        (id, rect, hit)
    });
    Layout { body, screen, home, side, scale, corner: skin.corner * scale }
}

/// `layout` for the device turned to `rotation`: laid out upright in the turned space, then every part turned a
/// quarter about the centre (quarter turns keep rectangles upright, so the painting is unchanged).
pub fn layout_turned(skin: &Skin, space: Rect, rotation: Rotation) -> Layout {
    if !rotation.landscape() {
        return layout(skin, space);
    }
    let center = space.center();
    let upright = layout(skin, Rect::from_center_size(center, vec2(space.height(), space.width())));
    turn(upright, center, rotation)
}

/// `layout_at` for the device turned to `rotation`.
pub fn layout_turned_at(skin: &Skin, center: Pos2, scale: f32, rotation: Rotation) -> Layout {
    let upright = layout_at(skin, center, scale);
    if rotation.landscape() { turn(upright, center, rotation) } else { upright }
}

fn turn(upright: Layout, center: Pos2, rotation: Rotation) -> Layout {
    let turn = |r: Rect| {
        let corner = |p: Pos2| {
            let (dx, dy) = rotation.turn(p.x - center.x, p.y - center.y);
            center + vec2(dx, dy)
        };
        Rect::from_two_pos(corner(r.min), corner(r.max))
    };
    Layout {
        body: turn(upright.body),
        screen: turn(upright.screen),
        home: turn(upright.home),
        side: upright.side.map(|(id, rect, hit)| (id, turn(rect), turn(hit))),
        ..upright
    }
}

/// Where the screen goes in `space`, and the drawn device around it when there is one.
pub struct Placed {
    pub screen: Rect,
    pub device: Option<Layout>,
}

/// Fit the device (or only its screen) into `space`, or at `real` points per millimetre when the device shows at its
/// real size. At real size the screen is centred in `space` and may be larger than it, and the drawn device around it
/// usually is: the caller crops both to `space`, so the screen is the last part cut off.
pub fn place(skin: &Skin, space: Rect, rotation: Rotation, frame: bool, real: Option<f32>) -> Placed {
    match real {
        None if frame => {
            let layout = layout_turned(skin, space, rotation);
            Placed { screen: layout.screen, device: Some(layout) }
        }
        None => {
            let aspect = skin.shown_aspect(rotation);
            let (w, h) = if space.width() / space.height() > aspect {
                (space.height() * aspect, space.height())
            } else {
                (space.width(), space.width() / aspect)
            };
            Placed { screen: Rect::from_center_size(space.center(), vec2(w, h)), device: None }
        }
        Some(scale) => {
            // The whole device centred when it fits, else its screen.
            let layout = layout_turned_at(skin, space.center(), scale, rotation);
            let fits = frame && space.expand(0.5).contains_rect(layout.body);
            let layout = if fits { layout } else { shifted(layout, space.center() - layout.screen.center()) };
            Placed { screen: layout.screen, device: frame.then_some(layout) }
        }
    }
}

/// Every part of a layout moved by `by`.
fn shifted(layout: Layout, by: Vec2) -> Layout {
    Layout {
        body: layout.body.translate(by),
        screen: layout.screen.translate(by),
        home: layout.home.translate(by),
        side: layout.side.map(|(id, rect, hit)| (id, rect.translate(by), hit.translate(by))),
        ..layout
    }
}

/// Points per millimetre for showing this device at its real size, when that's on and applies to it.
pub fn real_scale(
    skin: &Skin,
    settings: &lighttouch_model::settings::Settings,
    ctx: &eframe::egui::Context,
) -> Option<f32> {
    settings
        .real_px_per_mm(*skin == Skin::IPAD)
        .map(|d| lighttouch_model::real_size::points_per_mm(d, ctx.pixels_per_point()))
}

/// The screen's pixels standing upright, by board, until a frame says: 320 x 480, Retina 640 x 960, the iPhone 5's
/// 640 x 1136 and the iPad's 768 x 1024.
pub fn screen_pixels(board: Option<&str>) -> [u32; 2] {
    match board {
        Some(b) if b.starts_with("n81") || b.starts_with("n90") => [640, 960],
        Some(b) if b.starts_with("n42") => [640, 1136],
        Some(b) if b.starts_with("k48") => [768, 1024],
        _ => [320, 480],
    }
}

/// `place` by the Screen scaling settings: fitted, at the real size, or (Pixel perfect) at a whole multiple of the
/// screen's `pixels`, on the physical pixel grid, never a fraction: below 1x the screen is cropped.
pub fn place_for(
    skin: &Skin,
    space: Rect,
    rotation: Rotation,
    frame: bool,
    settings: &lighttouch_model::settings::Settings,
    ctx: &eframe::egui::Context,
    pixels: [u32; 2],
) -> Placed {
    let ppp = ctx.pixels_per_point();
    if settings.scaling == lighttouch_model::settings::Scaling::PixelPerfect {
        return place_whole(skin, space, rotation, frame, settings.integer_scale, pixels, ppp);
    }
    place(skin, space, rotation, frame, real_scale(skin, settings, ctx))
}

/// The multiple Pixel perfect shows: the largest the whole device fits around (with the device drawn), else the
/// largest the screen alone fits (the device cropped around it), else 1x (the screen cropped). A `fixed` multiple is a cap on
/// that, so a window made smaller steps down a whole multiple at a time and back up as it grows.
pub fn whole_multiple(
    skin: &Skin,
    space: Rect,
    rotation: Rotation,
    frame: bool,
    fixed: u8,
    pixels: [u32; 2],
    ppp: f32,
) -> u32 {
    let fits = |k: u32, whole: bool| {
        let layout = layout_turned_at(skin, space.center(), mm_scale(skin, k, pixels, ppp), rotation);
        let r = if whole { layout.body } else { layout.screen };
        r.width() <= space.width() + 0.5 && r.height() <= space.height() + 0.5
    };
    let largest = |whole: bool| (1..=12).rev().find(|&k| fits(k, whole));
    let auto = frame.then(|| largest(true)).flatten().or_else(|| largest(false)).unwrap_or(1);
    if fixed > 0 { auto.min(fixed as u32) } else { auto }
}

/// Points per millimetre that make the screen `k` times its pixels wide.
fn mm_scale(skin: &Skin, k: u32, pixels: [u32; 2], ppp: f32) -> f32 {
    k as f32 * pixels[0] as f32 / ppp / skin.screen_width
}

fn place_whole(
    skin: &Skin,
    space: Rect,
    rotation: Rotation,
    frame: bool,
    fixed: u8,
    pixels: [u32; 2],
    ppp: f32,
) -> Placed {
    let k = whole_multiple(skin, space, rotation, frame, fixed, pixels, ppp);
    // A device that doesn't fit around the screen at this multiple is cropped with it, as at real size.
    let mut placed = place(skin, space, rotation, frame, Some(mm_scale(skin, k, pixels, ppp)));
    // The screen exactly k x its pixels (the drawn mm are a hair off the panel's pixel aspect), starting on a
    // physical pixel, and the device moved with it.
    let [w, h] = if rotation.landscape() { [pixels[1], pixels[0]] } else { pixels };
    let size = vec2((k * w) as f32, (k * h) as f32) / ppp;
    let center = placed.screen.center();
    let corner = center - size / 2.0;
    let min = egui::pos2((corner.x * ppp).round() / ppp, (corner.y * ppp).round() / ppp);
    let by = min - placed.screen.min;
    placed.device = placed.device.map(|layout| shifted(layout, by));
    placed.screen = Rect::from_min_size(min, size);
    placed
}

/// Draw the device (everything except the screen contents). `pressed` darkens held buttons.
pub fn paint(painter: &Painter, skin: &Skin, layout: &Layout, pressed: impl Fn(u32) -> bool) {
    let s = layout.scale;
    for (id, rect, _) in layout.side {
        let rim = if pressed(id) { skin.rim.map(|c| c.gamma_multiply(0.75)) } else { skin.rim };
        painter.add(gradient_rounded_rect(rect, 0.8 * s, rim));
    }
    let shadow = epaint::Shadow {
        offset: [0, (2.0 * s) as i8],
        blur: (4.0 * s).min(255.0) as u8,
        spread: 0,
        color: Color32::from_black_alpha(110),
    };
    painter.add(shadow.as_shape(layout.body, CornerRadius::same(layout.corner.min(255.0) as u8)));
    painter.add(gradient_rounded_rect(layout.body, layout.corner, skin.rim));
    let rim = skin.rim_width * s;
    let face = layout.body.shrink(rim);
    painter.rect_filled(face, CornerRadius::same((layout.corner - rim).max(0.0).min(255.0) as u8), skin.face);
    // A faint glass edge around the panel.
    painter.rect_stroke(
        layout.screen.expand(0.6 * s),
        CornerRadius::same((0.6 * s).min(255.0) as u8),
        Stroke::new(1.0, Color32::from_white_alpha(10)),
        epaint::StrokeKind::Outside,
    );
    // Home button: radial gradient, a thin ring and the rounded-square glyph.
    let home = layout.home;
    let (inner, outer) = if pressed(HOME) { (rgb(0x101012), rgb(0x020202)) } else { (rgb(0x1C1C1E), rgb(0x050505)) };
    painter.add(radial_circle(home.center(), home.width() / 2.0, inner, outer));
    painter.circle_stroke(home.center(), home.width() / 2.0, Stroke::new(1.0, rgb(0x3A3A3C)));
    let glyph = Rect::from_center_size(home.center(), Vec2::splat(home.width() * 0.34));
    painter.rect_stroke(
        glyph,
        CornerRadius::same((home.width() * 0.08).min(255.0) as u8),
        Stroke::new(1.0, rgb(0x8E8E93)),
        epaint::StrokeKind::Inside,
    );
}

/// Outline of a rounded rectangle, clockwise from the top-left corner.
fn rounded_outline(rect: Rect, radius: f32) -> Vec<Pos2> {
    let r = radius.min(rect.width() / 2.0).min(rect.height() / 2.0).max(0.0);
    let steps = ((r * 0.6).ceil() as usize).clamp(3, 16);
    let corners = [
        (pos2(rect.right() - r, rect.top() + r), -90.0f32),
        (pos2(rect.right() - r, rect.bottom() - r), 0.0),
        (pos2(rect.left() + r, rect.bottom() - r), 90.0),
        (pos2(rect.left() + r, rect.top() + r), 180.0),
    ];
    let mut points = Vec::with_capacity(4 * (steps + 1));
    for (center, start) in corners {
        for i in 0..=steps {
            let angle = (start + 90.0 * i as f32 / steps as f32).to_radians();
            points.push(center + vec2(angle.cos(), angle.sin()) * r);
        }
    }
    points
}

/// Compose's default linear gradient: top-left to bottom-right, colours evenly spaced.
fn gradient_rounded_rect(rect: Rect, radius: f32, colors: [Color32; 3]) -> Shape {
    let points = rounded_outline(rect, radius);
    let diagonal = rect.max - rect.min;
    let length = diagonal.length_sq().max(1.0);
    let color_at = |p: Pos2| {
        let t = ((p - rect.min).dot(diagonal) / length).clamp(0.0, 1.0);
        if t < 0.5 { lerp(colors[0], colors[1], t * 2.0) } else { lerp(colors[1], colors[2], (t - 0.5) * 2.0) }
    };
    fan(rect.center(), color_at(rect.center()), &points, color_at)
}

fn radial_circle(center: Pos2, radius: f32, inner: Color32, outer: Color32) -> Shape {
    let steps = 48;
    let points: Vec<Pos2> = (0..steps)
        .map(|i| {
            let a = std::f32::consts::TAU * i as f32 / steps as f32;
            center + vec2(a.cos(), a.sin()) * radius
        })
        .collect();
    fan(center, inner, &points, |_| outer)
}

fn fan(center: Pos2, center_color: Color32, outline: &[Pos2], color: impl Fn(Pos2) -> Color32) -> Shape {
    let mut mesh = Mesh::default();
    mesh.colored_vertex(center, center_color);
    for p in outline {
        mesh.colored_vertex(*p, color(*p));
    }
    let n = outline.len() as u32;
    for i in 0..n {
        mesh.add_triangle(0, 1 + i, 1 + (i + 1) % n);
    }
    Shape::mesh(mesh)
}

fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_premultiplied(mix(a.r(), b.r()), mix(a.g(), b.g()), mix(a.b(), b.b()), mix(a.a(), b.a()))
}
