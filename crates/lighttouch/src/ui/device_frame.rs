//! The drawn device around the guest screen (port of the Android DeviceFrame): outlines in millimetres, the screen
//! placed at the panel's exact aspect so the guest never letterboxes, and real Home, Sleep/Wake and volume buttons.
use eframe::egui::{Color32, CornerRadius, Mesh, Painter, Pos2, Rect, Shape, Stroke, Vec2, epaint, pos2, vec2};
use lighttouch_model::rotation::Rotation;

#[derive(Debug, Clone, Copy)]
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
        face: rgb(0x0B0B0C),
        rim: [rgb(0xC7C7CC), rgb(0x6E6E73), rgb(0xAEAEB2)],
    };
    pub const IPAD: Skin = Skin {
        width: 189.7,
        height: 242.8,
        corner: 14.0,
        screen_left: 18.85,
        screen_top: 25.2,
        screen_width: 152.0,
        screen_height: 202.7,
        home_center_y: 236.6,
        home_diameter: 11.0,
        face: rgb(0x0B0B0C),
        rim: [rgb(0xD1D1D6), rgb(0x8E8E93), rgb(0xC7C7CC)],
    };

    pub fn for_board(board: Option<&str>) -> Skin {
        match board {
            Some(b) if b.starts_with("n81") => Self::IPOD_TOUCH_4,
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
    let outer =
        Rect::from_center_size(space.center(), vec2(skin.width + 2.0 * MARGIN, skin.height + 2.0 * MARGIN) * scale);
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
    let side_len = skin.height * 0.065;
    let drawn = [
        (POWER, MARGIN + skin.width * 0.62, MARGIN - 1.4, skin.width * 0.2, 2.0),
        (VOLUME_UP, MARGIN - 1.3, MARGIN + skin.height * 0.13, 2.0, side_len),
        (VOLUME_DOWN, MARGIN - 1.3, MARGIN + skin.height * 0.21, 2.0, side_len),
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
    let face = layout.body.shrink(0.7 * s);
    painter.rect_filled(face, CornerRadius::same((layout.corner - 0.7 * s).max(0.0).min(255.0) as u8), skin.face);
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
