//! Settings → Screen scaling: how the guest's pixels are drawn at the size they're shown, devices at their real size
//! (for toys and pieces used on the screen) with the display measured against a card or a ruler, and a preview beside
//! them: sample pictures at each screen's own resolution drawn the way the device will be, or a screen at its real
//! size over 1 cm squares. The measure is kept in physical pixels, so it holds through display scaling and zoom.
use super::device_frame::{self, Skin};
use super::scaling::{self, Scaled};
use super::{App, Page, icons, theme, widgets};
use eframe::egui::{
    self, Align2, Color32, ColorImage, CornerRadius, FontId, Frame, Margin, Rect, RichText, Sense, Stroke, Ui, vec2,
};
use lighttouch_model::real_size::{self, CARD_CORNER, CARD_HEIGHT, CARD_WIDTH, DisplayMeasure};
use lighttouch_model::rotation::Rotation;
use lighttouch_model::settings::{RealSize, Scaling};
use std::collections::HashMap;

pub const TABLET: &str = "\u{E1E6}";
const MINUS: &str = "\u{E32A}";

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Reference {
    #[default]
    Card,
    Ruler,
}

/// The screens the preview can show, by resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Screen {
    Classic,
    Retina,
    Tall,
    Ipad,
}

impl Screen {
    const ALL: [Screen; 4] = [Screen::Classic, Screen::Retina, Screen::Tall, Screen::Ipad];

    /// The devices with this screen, for the device menu.
    fn label(self) -> &'static str {
        match self {
            Self::Classic => "iPhone, iPod touch",
            Self::Retina => "iPhone 4, iPod touch 4G",
            Self::Tall => "iPhone 5",
            Self::Ipad => "iPad",
        }
    }

    /// Its resolution, under its name in the device picker.
    fn resolution(self) -> &'static str {
        match self {
            Self::Classic => "320 × 480",
            Self::Retina => "640 × 960",
            Self::Tall => "640 × 1136",
            Self::Ipad => "1024 × 768",
        }
    }

    fn pixels(self) -> [u32; 2] {
        match self {
            Self::Classic => [320, 480],
            Self::Retina => [640, 960],
            Self::Tall => [640, 1136],
            Self::Ipad => [768, 1024],
        }
    }

    fn skin(self) -> Skin {
        match self {
            Self::Classic => Skin::IPOD_TOUCH_2,
            Self::Retina => Skin::IPOD_TOUCH_4,
            Self::Tall => Skin::IPHONE_5,
            Self::Ipad => Skin::IPAD,
        }
    }

    fn for_board(board: Option<&str>) -> Screen {
        match device_frame::screen_pixels(board) {
            [640, 960] => Self::Retina,
            [640, 1136] => Self::Tall,
            [768, 1024] => Self::Ipad,
            _ => Self::Classic,
        }
    }
}

/// The preview's sample pictures (art/preview/scaling, drawn by tools/scaling-samples/make.py).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Sample {
    #[default]
    PixelArt,
    Game,
    Home,
}

impl Sample {
    fn label(self) -> &'static str {
        match self {
            Self::PixelArt => "Pixel art",
            Self::Game => "3D game",
            Self::Home => "Home screen",
        }
    }

    fn png(self, screen: Screen) -> &'static [u8] {
        macro_rules! art {
            ($name:literal) => {
                include_bytes!(concat!("../../../../art/preview/scaling/", $name, ".png")).as_slice()
            };
        }
        match (self, screen) {
            (Self::PixelArt, Screen::Classic) => art!("pixel-320x480"),
            (Self::PixelArt, Screen::Retina) => art!("pixel-640x960"),
            (Self::PixelArt, Screen::Tall) => art!("pixel-640x1136"),
            (Self::PixelArt, Screen::Ipad) => art!("pixel-768x1024"),
            (Self::Game, Screen::Classic) => art!("game-320x480"),
            (Self::Game, Screen::Retina) => art!("game-640x960"),
            (Self::Game, Screen::Tall) => art!("game-640x1136"),
            (Self::Game, Screen::Ipad) => art!("game-768x1024"),
            (Self::Home, Screen::Classic) => art!("home-320x480"),
            (Self::Home, Screen::Retina) => art!("home-640x960"),
            (Self::Home, Screen::Tall) => art!("home-640x1136"),
            (Self::Home, Screen::Ipad) => art!("home-768x1024"),
        }
    }
}

/// The page's unsaved state: the density being adjusted and what it's matched against, and the preview's choices.
#[derive(Default)]
pub struct ScreenScalingState {
    draft: Option<f32>,
    reference: Reference,
    measuring: bool,
    /// The preview's screen; until one is picked, the selected device's.
    screen: Option<Screen>,
    squares: bool,
    sample: Sample,
    pictures: HashMap<(Sample, Screen), (ColorImage, Scaled)>,
}

/// Open the page from Settings, starting from the saved measure.
pub fn open(app: &mut App) {
    app.screen_scaling.draft = None;
    app.screen_scaling.screen = None;
    app.screen_scaling.measuring = false;
    app.page = Page::ScreenScaling;
}

/// The Settings row's subtitle.
pub fn summary(app: &App) -> String {
    let s = &app.model.settings;
    let method = match (s.scaling, s.integer_scale) {
        (Scaling::PixelPerfect, 0) => "Pixel perfect".to_string(),
        (Scaling::PixelPerfect, k) => format!("Pixel perfect, {k}×"),
        (method, _) => method.label().to_string(),
    };
    match (s.scaling, s.real_size) {
        (Scaling::PixelPerfect, _) | (_, RealSize::Off) => method,
        (_, RealSize::Ipads) => format!("{method} · iPads at real size"),
        (_, RealSize::All) => format!("{method} · real size"),
    }
}

/// What each method does, in a line.
fn about(scaling: Scaling) -> &'static str {
    match scaling {
        Scaling::Smooth => "Blends pixels: soft, with no jagged edges",
        Scaling::Sharp => "Hard-edged pixels, some a little wider than others",
        Scaling::Crisp => "Sharp pixels, evened out with a hint of blending",
        Scaling::PixelPerfect => "Whole-number sizes only, with a border",
    }
}

/// The monitor the window is on, in physical pixels, when the system says.
fn monitor_pixels(ctx: &egui::Context) -> Option<[u32; 2]> {
    let ppp = ctx.pixels_per_point();
    ctx.input(|i| i.viewport().monitor_size)
        .filter(|s| s.x >= 1.0 && s.y >= 1.0)
        .map(|s| [(s.x * ppp).round() as u32, (s.y * ppp).round() as u32])
}

pub fn show(app: &mut App, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let native = ctx.input(|i| i.viewport().native_pixels_per_point).unwrap_or(ctx.pixels_per_point());
    let saved = app.model.settings.display;
    let mut draft = *app
        .screen_scaling
        .draft
        .get_or_insert_with(|| saved.map_or_else(|| real_size::guess(native), |d| d.px_per_mm));
    let s = &app.model.settings;
    let before = (s.scaling, s.integer_scale, s.real_size, s.display);

    let area = ui.available_rect_before_wrap();
    // The settings column holds the card (upright, so only its 54 mm side runs across) with its padding and the
    // scroll bar when measuring: on a much denser display than the first guess it widens, leaving the preview at
    // least 344 points, and when that leaves the preview too little, the preview goes below the settings instead.
    let ppm = real_size::points_per_mm(draft, ctx.pixels_per_point());
    let card_room = if app.screen_scaling.measuring { CARD_HEIGHT * ppm + 110.0 } else { 0.0 };
    // The preview needs about 344 points for a 320-pixel screen at 1x inside its card and panel.
    let cap = area.width() - 48.0 - 20.0 - 344.0;
    let wide = area.width() >= 820.0 && card_room.max(420.0) <= cap.max(420.0);
    let column = if wide { card_room.clamp(420.0, cap.max(420.0)) } else { (area.width() - 48.0).max(200.0) };
    let left = Rect::from_min_size(area.min + vec2(24.0, 0.0), vec2(column, area.height()));
    let mut left_ui = ui.new_child(egui::UiBuilder::new().id_salt("screen-scaling-settings").max_rect(left));
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(&mut left_ui, |ui| {
        // Room for the scroll bar beside the cards.
        ui.set_max_width(column - 14.0);
        ui.add_space(18.0);
        ui.horizontal(|ui| {
            if widgets::icon_button(ui, &app.palette, icons::BACK, "Back to settings").clicked() {
                app.page = Page::Settings;
            }
            ui.label(RichText::new("Screen scaling").font(theme::headline()));
        });
        methods(app, ui);
        real_size_settings(app, ui, &mut draft, native);
        if !wide {
            ui.add_space(16.0);
            let top = ui.cursor().min;
            let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 640.0), Sense::hover());
            preview(app, ui, Rect::from_min_max(top, rect.max), draft, area);
        }
        ui.add_space(28.0);
    });
    if wide {
        // Level with the first section's label, so the page title stands alone.
        let right = Rect::from_min_max(egui::pos2(left.right() + 20.0, area.top() + 80.0), area.max - vec2(24.0, 24.0));
        preview(app, ui, right, draft, area);
    }
    ui.allocate_rect(area, Sense::hover());

    app.screen_scaling.draft = Some(draft);
    let s = &app.model.settings;
    if (s.scaling, s.integer_scale, s.real_size, s.display) != before {
        app.model.save_settings();
    }
}

/// The methods as a list to pick from, with Pixel perfect's multiple under it when it's picked.
fn methods(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    widgets::section(ui, &p, "Method");
    let settings = &mut app.model.settings;
    widgets::card(ui, &p, |ui| {
        for method in Scaling::ALL {
            if choice_row(ui, &p, settings.scaling == method, method.label(), about(method)) {
                settings.scaling = method;
            }
            if method == Scaling::PixelPerfect && settings.scaling == method {
                Frame::new().inner_margin(Margin { left: 34, right: 0, top: 0, bottom: 6 }).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Size").size(14.0));
                        ui.add_space(8.0);
                        widgets::segmented_tips(
                            ui,
                            &p,
                            &mut settings.integer_scale,
                            &[
                                (0, "Auto", "The largest that fits"),
                                (1, "1×", ""),
                                (2, "2×", ""),
                                (3, "3×", ""),
                                (4, "4×", ""),
                            ],
                        );
                    });
                });
            }
        }
    });
}

/// A row picked like a radio button, anywhere along it.
fn choice_row(ui: &mut Ui, p: &theme::Palette, selected: bool, title: &str, subtitle: &str) -> bool {
    let row = ui.horizontal(|ui| {
        ui.set_min_height(50.0);
        let (dot, _) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::hover());
        let color = if selected { p.primary } else { p.outline };
        ui.painter().circle_stroke(dot.center(), 9.0, Stroke::new(2.0, color));
        if selected {
            ui.painter().circle_filled(dot.center(), 5.0, color);
        }
        ui.add_space(8.0);
        ui.vertical(|ui| {
            ui.add_space(4.0);
            ui.label(RichText::new(title).size(14.5));
            ui.label(RichText::new(subtitle).size(12.5).color(p.on_surface_variant));
        });
        ui.allocate_space(vec2(ui.available_width(), 1.0));
    });
    let response = ui.interact(row.response.rect, ui.scope_id().with(("choice", title)), Sense::click());
    if response.hovered() && !selected {
        ui.painter().rect_filled(
            row.response.rect.expand(2.0),
            CornerRadius::same(8),
            p.on_surface.gamma_multiply(0.04),
        );
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() && !selected
}

fn real_size_settings(app: &mut App, ui: &mut Ui, draft: &mut f32, native: f32) {
    let p = app.palette;
    let ctx = ui.ctx().clone();
    let ppp = ctx.pixels_per_point();
    let monitor = monitor_pixels(&ctx);
    let saved = app.model.settings.display;
    let measure = |px_per_mm: f32| DisplayMeasure { px_per_mm, monitor, estimate: false };

    widgets::section(ui, &p, "Real size");
    widgets::card(ui, &p, |ui| {
        let integer = app.model.settings.scaling == Scaling::PixelPerfect;
        let mut real = app.model.settings.real_size;
        let subtitle = if integer {
            "Pixel perfect sets its own size"
        } else {
            "The real device's size, for toys and pieces used on the screen"
        };
        widgets::setting_row(ui, &p, TABLET, "Show at real size", subtitle, |ui| {
            ui.add_enabled_ui(!integer, |ui| {
                widgets::segmented(
                    ui,
                    &p,
                    &mut real,
                    &[(RealSize::Off, "Off"), (RealSize::Ipads, "iPads"), (RealSize::All, "All")],
                );
            });
        });
        if real != app.model.settings.real_size {
            app.model.settings.real_size = real;
            // Turned on before any measure: the size shown here is the one to use, until one is saved.
            if real != RealSize::Off && saved.is_none() {
                app.model.settings.display = Some(DisplayMeasure { estimate: true, ..measure(*draft) });
                app.screen_scaling.measuring = true;
            }
        }

        // The measure, folded away until wanted.
        let status = match saved {
            Some(s) if s.other_display(monitor) => "Measured on another display".to_string(),
            Some(s) if !s.estimate => format!("{:.1} pixels per inch", real_size::per_inch(s.px_per_mm)),
            _ => "Not measured yet".to_string(),
        };
        let measuring = app.screen_scaling.measuring;
        let caret = if measuring { icons::CARET_DOWN } else { icons::CARET_RIGHT };
        let row = ui.scope(|ui| {
            widgets::setting_row(ui, &p, icons::MONITOR, "Measure this display", &status, |ui| {
                ui.label(widgets::icon_text(caret, 18.0).color(p.on_surface_variant));
            })
        });
        let fold = ui
            .interact(row.response.rect, ui.scope_id().with("measure-fold"), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if fold.clicked() {
            app.screen_scaling.measuring = !measuring;
        }
        if !app.screen_scaling.measuring {
            return;
        }
        Frame::new().inner_margin(Margin { left: 34, right: 4, top: 2, bottom: 6 }).show(ui, |ui| {
            measure_display(app, ui, draft, native, ppp, monitor, saved);
        });
    });
    Frame::new().inner_margin(Margin { left: 18, right: 18, top: 8, bottom: 0 }).show(ui, |ui| {
        ui.label(
            widgets::muted(&p, "Measure again after moving LightTouch to another display or changing its resolution.")
                .size(12.5),
        );
    });
}

fn measure_display(
    app: &mut App,
    ui: &mut Ui,
    draft: &mut f32,
    native: f32,
    ppp: f32,
    monitor: Option<[u32; 2]>,
    saved: Option<DisplayMeasure>,
) {
    let p = app.palette;
    let measure = |px_per_mm: f32| DisplayMeasure { px_per_mm, monitor, estimate: false };
    let reference = &mut app.screen_scaling.reference;
    widgets::segmented(ui, &p, reference, &[(Reference::Card, "Bank card"), (Reference::Ruler, "Ruler")]);
    ui.add_space(8.0);
    let text = match reference {
        Reference::Card => {
            "Hold a bank or ID card upright against the screen, its corner in the outline's top-left corner. \
             Drag the handle until the outline's bottom edge meets the card's."
        }
        Reference::Ruler => {
            "Hold a ruler upright along the marks, its 0 on the top one. Drag the handle until the bottom mark \
             meets the same number on the ruler."
        }
    };
    ui.label(widgets::muted(&p, text).size(12.5));
    ui.add_space(10.0);
    let length = match reference {
        Reference::Card => card(ui, &p, draft, ppp),
        Reference::Ruler => ruler(ui, &p, draft, ppp),
    };
    ui.add_space(6.0);

    // Fine steps of one pixel across the reference, and the density as numbers.
    let step = 1.0 / length;
    ui.horizontal(|ui| {
        ui.label(RichText::new("Density").size(14.0));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if widgets::icon_button_sized(ui, &p, icons::PLUS, "Larger", 32.0, true).clicked() {
                *draft = real_size::clamp(*draft + step);
            }
            let mut ppi = real_size::per_inch(*draft);
            let range = real_size::per_inch(real_size::MIN_PX_PER_MM)..=real_size::per_inch(real_size::MAX_PX_PER_MM);
            let edit = egui::DragValue::new(&mut ppi).speed(0.1).range(range).fixed_decimals(1).suffix(" pixels/inch");
            if ui.add(edit).changed() {
                *draft = real_size::from_per_inch(ppi);
            }
            if widgets::icon_button_sized(ui, &p, MINUS, "Smaller", 32.0, true).clicked() {
                *draft = real_size::clamp(*draft - step);
            }
        });
    });
    if let Some(monitor) = monitor {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Screen size").size(14.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut inches = real_size::diagonal_inches(monitor, *draft);
                let edit =
                    egui::DragValue::new(&mut inches).speed(0.05).range(3.0..=120.0).fixed_decimals(1).suffix(" in");
                if ui.add(edit).on_hover_text("The display's diagonal, if you know it").changed() {
                    *draft = real_size::from_diagonal(monitor, inches);
                }
                ui.label(widgets::muted(&p, format!("{} × {} display", monitor[0], monitor[1])).size(12.5));
            });
        });
    }
    ui.add_space(10.0);

    let unsaved = saved.is_none_or(|s| s.estimate || (s.px_per_mm - *draft).abs() > 1e-4 || s.monitor != monitor);
    ui.horizontal(|ui| {
        if widgets::primary_enabled(ui, &p, "Save", unsaved).clicked() {
            app.model.settings.display = Some(measure(*draft));
            if app.model.settings.real_size == RealSize::Off {
                app.model.settings.real_size = RealSize::All;
            }
        }
        if let Some(s) = saved.filter(|s| !s.estimate && (s.px_per_mm - *draft).abs() > 1e-4) {
            if widgets::text_button(ui, &p, "Back to saved").clicked() {
                *draft = s.px_per_mm;
            }
        } else if saved.is_some() && widgets::text_button(ui, &p, "Start over").clicked() {
            *draft = real_size::guess(native);
        }
    });
    if let Some(s) = saved {
        ui.add_space(4.0);
        if s.other_display(monitor) {
            let [w, h] = s.monitor.unwrap_or_default();
            ui.label(
                RichText::new(format!(
                    "Measured on a {w} × {h} display, not this one. Measure here and save to use this display."
                ))
                .size(12.5)
                .color(theme::WARNING),
            );
        } else if s.estimate {
            ui.label(widgets::muted(&p, "Using an estimate until you measure and save").size(12.5));
        }
    }
}

/// A card's outline at the draft density, its bottom edge dragged to fit. Returns the reference's length in mm.
fn card(ui: &mut Ui, p: &theme::Palette, draft: &mut f32, ppp: f32) -> f32 {
    let ppm = real_size::points_per_mm(*draft, ppp);
    let size = vec2(CARD_HEIGHT, CARD_WIDTH) * ppm;
    let (area, _) = ui.allocate_exact_size(vec2(ui.available_width(), size.y + 16.0), Sense::hover());
    let outline = Rect::from_min_size(area.min + vec2(2.0, 2.0), size);
    let painter = ui.painter().with_clip_rect(area.intersect(ui.clip_rect()));
    let corner = CornerRadius::same((CARD_CORNER * ppm).min(255.0) as u8);
    painter.rect(
        outline,
        corner,
        p.primary.gamma_multiply(0.12),
        Stroke::new(2.0, p.primary),
        egui::StrokeKind::Inside,
    );
    painter.text(
        outline.center(),
        Align2::CENTER_CENTER,
        "Card",
        FontId::new(13.0, theme::medium()),
        p.primary.gamma_multiply(0.7),
    );
    grip(&painter, p, outline.center_bottom(), true);
    let handle = Rect::from_center_size(outline.center_bottom(), vec2(outline.width(), 36.0));
    drag(ui, handle, true, draft, ppp, CARD_WIDTH);
    CARD_WIDTH
}

/// Ruler marks down the page at the draft density: millimetres, half centimetres and numbered centimetres, 10 cm
/// unless that would run far down the page.
fn ruler(ui: &mut Ui, p: &theme::Palette, draft: &mut f32, ppp: f32) -> f32 {
    let ppm = real_size::points_per_mm(*draft, ppp);
    let length = [100.0, 80.0, 50.0].into_iter().find(|mm| mm * ppm <= 480.0).unwrap_or(50.0);
    let (area, _) = ui.allocate_exact_size(vec2(ui.available_width(), length * ppm + 26.0), Sense::hover());
    let painter = ui.painter().with_clip_rect(area.intersect(ui.clip_rect()));
    let x0 = area.left() + 2.0;
    let y0 = area.top() + 8.0;
    let line = Stroke::new(1.0, p.on_surface);
    painter.line_segment([egui::pos2(x0, y0), egui::pos2(x0, y0 + length * ppm)], line);
    for mm in 0..=length as u32 {
        let y = y0 + mm as f32 * ppm;
        let (long, stroke) = if mm % 10 == 0 {
            (16.0, Stroke::new(1.5, p.primary))
        } else if mm % 5 == 0 {
            (10.0, line)
        } else {
            (5.0, Stroke::new(1.0, p.on_surface_variant))
        };
        painter.line_segment([egui::pos2(x0, y), egui::pos2(x0 + long, y)], stroke);
        if mm % 10 == 0 {
            painter.text(
                egui::pos2(x0 + 22.0, y),
                Align2::LEFT_CENTER,
                format!("{}", mm / 10),
                FontId::new(11.0, egui::FontFamily::Proportional),
                p.on_surface_variant,
            );
        }
    }
    let end = egui::pos2(x0 + 8.0, y0 + length * ppm);
    grip(&painter, p, end, true);
    let handle = Rect::from_center_size(egui::pos2(area.left() + 40.0, end.y), vec2(80.0, 36.0));
    drag(ui, handle, true, draft, ppp, length);
    length
}

fn grip(painter: &egui::Painter, p: &theme::Palette, at: egui::Pos2, across: bool) {
    let rect = Rect::from_center_size(at, if across { vec2(22.0, 8.0) } else { vec2(8.0, 22.0) });
    painter.rect(rect, CornerRadius::same(4), p.primary, Stroke::new(1.5, p.container), egui::StrokeKind::Outside);
}

/// Dragging the reference's far end (its grip, or near it) moves it with the pointer, across or `down`. Only
/// there: elsewhere the hand holding a card to the screen would move it too.
fn drag(ui: &Ui, handle: Rect, down: bool, draft: &mut f32, ppp: f32, length: f32) {
    let cursor = if down { egui::CursorIcon::ResizeVertical } else { egui::CursorIcon::ResizeHorizontal };
    let response = ui.interact(handle, ui.scope_id().with("reference-end"), Sense::drag()).on_hover_cursor(cursor);
    if response.dragged() {
        let delta = response.drag_delta();
        let moved = if down { delta.y } else { delta.x };
        if moved != 0.0 {
            // The far end sits `length` mm out: move it by the pointer's travel, in physical pixels.
            *draft = real_size::clamp(*draft + moved * ppp / length);
            ui.ctx().request_repaint();
        }
    }
}

/// What the preview's footer says about the picture: the method, how large it's drawn and why, and the factor.
struct Shown {
    title: String,
    detail: String,
    factor: String,
}

/// The preview card, laid out like the Android page: a title, the device picker (its screen's resolution under the
/// name), the pictures as tabs, the picture in an inset panel, and a footer saying how it's drawn.
fn preview(app: &mut App, ui: &mut Ui, rect: Rect, draft: f32, page: Rect) {
    let p = app.palette;
    let selected = Screen::for_board(app.model.current().map(|d| d.board.as_str()));
    if rect.height() < 260.0 {
        return;
    }
    let clip = rect.intersect(ui.clip_rect());
    ui.painter().with_clip_rect(clip).rect_filled(rect, CornerRadius::same(theme::radius::CARD), p.container);
    let inner = rect.shrink2(vec2(12.0, 14.0));
    let mut ui = ui.new_child(egui::UiBuilder::new().id_salt("screen-scaling-preview").max_rect(inner));
    let ui = &mut ui;
    ui.label(RichText::new("Preview").font(theme::title()));
    ui.add_space(10.0);

    let state = &mut app.screen_scaling;
    let mut screen = state.screen.unwrap_or(selected);
    let picker = device_picker(ui, &p, screen);
    // As wide as the picker it opens from: the menu otherwise sizes itself to its longest entry.
    let width = picker.rect.width();
    egui::Popup::menu(&picker).width(width).show(|ui| {
        ui.set_min_width(width - ui.style().spacing.menu_margin.sum().x);
        for option in Screen::ALL {
            let text = format!("{}  ·  {}", option.label(), option.resolution());
            if ui.selectable_label(option == screen, text).clicked() {
                screen = option;
            }
        }
    });
    if Some(screen) != state.screen && (state.screen.is_some() || screen != selected) {
        state.screen = Some(screen);
    }
    ui.add_space(10.0);
    let mut picture = if state.squares { Picture::Squares } else { Picture::Sample(state.sample) };
    tabs(
        ui,
        &p,
        &mut picture,
        &[
            (Picture::Sample(Sample::PixelArt), Sample::PixelArt.label()),
            (Picture::Sample(Sample::Game), Sample::Game.label()),
            (Picture::Sample(Sample::Home), "Home"),
            (Picture::Squares, "1 cm"),
        ],
    );
    match picture {
        Picture::Sample(sample) => {
            state.sample = sample;
            state.squares = false;
        }
        Picture::Squares => state.squares = true,
    }
    ui.add_space(10.0);

    // The footer's two lines take the bottom; the inset panel holding the picture, the rest.
    let footer_height = 52.0;
    let panel = Rect::from_min_max(ui.cursor().min, egui::pos2(inner.right(), inner.bottom() - footer_height));
    if panel.height() < 80.0 {
        return;
    }
    ui.painter().with_clip_rect(clip).rect_filled(panel, CornerRadius::same(12), p.container_lowest);
    let space = panel.shrink(8.0);
    let shown = if app.screen_scaling.squares {
        squares(app, ui, space, screen, draft)
    } else {
        sample(app, ui, space, screen, draft, page)
    };

    let painter = ui.painter().with_clip_rect(clip);
    let rule = panel.bottom() + 12.0;
    painter.hline(inner.x_range(), rule, Stroke::new(1.0, p.outline_variant));
    let row = rule + 10.0;
    let factor = painter.layout_no_wrap(shown.factor, FontId::new(14.0, theme::medium()), p.on_surface);
    let text_left = inner.left() + 16.0;
    let text_width = (inner.right() - factor.size().x - 12.0 - text_left).max(40.0);
    let line = |text: String, size: f32, color: Color32| {
        let font = FontId::new(size, egui::FontFamily::Proportional);
        let mut job = egui::text::LayoutJob::simple(text, font, color, text_width);
        job.wrap.max_rows = 1;
        painter.layout_job(job)
    };
    let title = line(shown.title, 14.0, p.on_surface);
    let detail = line(shown.detail, 12.0, p.on_surface_variant);
    // The dot and the factor sit level with the middle of the two lines.
    let middle = row + (19.0 + detail.size().y) / 2.0;
    painter.galley(egui::pos2(text_left, row), title, p.on_surface);
    painter.galley(egui::pos2(text_left, row + 19.0), detail, p.on_surface_variant);
    painter.circle_filled(egui::pos2(inner.left() + 4.0, middle), 3.5, p.primary);
    painter.galley(egui::pos2(inner.right() - factor.size().x, middle - factor.size().y / 2.0), factor, p.on_surface);
}

/// The device picker: a full-width row with the device's icon, its name over its screen's resolution, and a caret.
fn device_picker(ui: &mut Ui, p: &theme::Palette, screen: Screen) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 56.0), Sense::click());
    let fill = if response.hovered() { p.container_highest } else { p.container_high };
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(12), fill);
    let icon = if screen == Screen::Ipad { TABLET } else { icons::PHONE };
    let at = rect.left_center();
    painter.text(
        at + vec2(16.0, 0.0),
        Align2::LEFT_CENTER,
        icon,
        FontId::new(24.0, theme::icons()),
        p.on_surface_variant,
    );
    painter.text(
        at + vec2(52.0, -9.0),
        Align2::LEFT_CENTER,
        screen.label(),
        FontId::new(14.5, theme::medium()),
        p.on_surface,
    );
    let small = FontId::new(12.0, egui::FontFamily::Proportional);
    let pixels = format!("{} pixels", screen.resolution());
    painter.text(at + vec2(52.0, 10.0), Align2::LEFT_CENTER, pixels, small, p.on_surface_variant);
    let caret = FontId::new(16.0, theme::icons());
    painter.text(
        rect.right_center() - vec2(16.0, 0.0),
        Align2::RIGHT_CENTER,
        icons::CARET_DOWN,
        caret,
        p.on_surface_variant,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Tabs of equal width across the row: the picked one raised, the others flat on a darker track.
fn tabs<T: PartialEq + Copy>(ui: &mut Ui, p: &theme::Palette, value: &mut T, options: &[(T, &str)]) {
    let (track, _) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::hover());
    ui.painter().rect_filled(track, CornerRadius::same(12), p.container_lowest);
    let cell = (track.width() - 8.0) / options.len() as f32;
    for (i, (option, text)) in options.iter().enumerate() {
        let r = Rect::from_min_size(track.min + vec2(4.0 + i as f32 * cell, 4.0), vec2(cell, track.height() - 8.0));
        let response = ui.interact(r, ui.make_persistent_id(("scaling-tab", i)), Sense::click());
        let picked = *value == *option;
        if picked {
            ui.painter().rect_filled(r, CornerRadius::same(9), p.container_highest);
        } else if response.hovered() {
            ui.painter().rect_filled(r, CornerRadius::same(9), p.container_high);
        }
        let color = if picked { p.primary } else { p.on_surface_variant };
        ui.painter().text(r.center(), Align2::CENTER_CENTER, *text, FontId::new(13.0, theme::medium()), color);
        if response.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            *value = *option;
        }
    }
}

/// What the preview shows on the screen.
#[derive(Clone, Copy, PartialEq)]
enum Picture {
    Sample(Sample),
    Squares,
}

/// "2×", "2.37×".
fn times(x: f32) -> String {
    if (x - x.round()).abs() < 0.01 { format!("{}×", x.round()) } else { format!("{x:.2}×") }
}

/// A sample picture as the device view will show it: the size the device page gives the screen (Pixel perfect's
/// multiple, real size with the draft measure, or fitted), drawn by the same method on whole physical pixels. When
/// that's larger than the preview, Pixel perfect shows the largest multiple that fits here and the other methods are
/// fitted; the footer says what's shown.
fn sample(app: &mut App, ui: &mut Ui, space: Rect, screen: Screen, draft: f32, page: Rect) -> Shown {
    let ctx = ui.ctx().clone();
    let ppp = ctx.pixels_per_point();
    let mut settings = app.model.settings.clone();
    if settings.real_size != RealSize::Off {
        settings.display = Some(DisplayMeasure { px_per_mm: draft, monitor: None, estimate: true });
    }
    let skin = screen.skin();
    let pixels = screen.pixels();
    // The device page's room for the device, as it was last laid out (else its usual bars and margins).
    let [l, t, r, b] = app.device_view_insets.unwrap_or([24.0, 80.0, 24.0, 108.0]);
    let view = Rect::from_min_max(page.min + vec2(l, t), page.max - vec2(r, b));
    let frame = settings.device_frame;
    let in_view = device_frame::place_for(&skin, view, Rotation::Portrait, frame, &settings, &ctx, pixels).screen;
    let view_scale = in_view.width() * ppp / pixels[0] as f32;
    let fits = |k: f32| {
        pixels[0] as f32 * k <= space.width() * ppp + 0.5 && pixels[1] as f32 * k <= space.height() * ppp + 0.5
    };
    let fit = (space.width() * ppp / pixels[0] as f32).min(space.height() * ppp / pixels[1] as f32);
    // The size the device view draws the screen at, in physical pixels.
    let rendered = |k: f32| format!("{} × {} rendered", (pixels[0] as f32 * k).round(), (pixels[1] as f32 * k).round());
    let (scale, shown) = if settings.scaling == Scaling::PixelPerfect {
        let k = view_scale.round().max(1.0);
        let here = (1..=12).rev().map(|k| k as f32).find(|&k| fits(k));
        let cropped = here.is_none();
        let here = here.unwrap_or(1.0);
        let on_show = k.min(here);
        let fixed = settings.integer_scale as f32;
        let mut detail = rendered(k);
        if fixed > k {
            detail += &format!(" · {} doesn't fit", times(fixed));
        }
        match (on_show < k, cropped) {
            (_, true) => detail += &format!(" · {} here, cropped", times(on_show)),
            (true, false) => detail += &format!(" · {} here", times(on_show)),
            (false, false) => {}
        }
        // A multiple counts physical pixels: at 200% display scaling, 1x looks half the size of other windows' 1x.
        if ppp > 1.01 {
            detail += &format!(" · smaller at {:.0}% display scaling", ppp * 100.0);
        }
        (on_show, Shown { title: "Pixel perfect".into(), detail, factor: times(k) })
    } else {
        // At real size the preview shows the screen at exactly that size on this display (the draft measure), never
        // shrunk to fit: the panel crops it instead. Otherwise it's the device view's size, fitted when that's larger.
        let real = device_frame::real_scale(&skin, &settings, &ctx)
            .map(|ppm| skin.screen_width * ppm * ppp / pixels[0] as f32);
        let wanted = real.unwrap_or(view_scale);
        let scale = if real.is_some() || fits(wanted) { wanted } else { fit };
        let k = scaling::prescale_factor(Scaling::Crisp, [1, 1], wanted);
        let mut detail = rendered(wanted);
        if real.is_some() {
            // Why the factor isn't 1x: the device's own pixel density against this display's.
            let device_ppi = pixels[0] as f32 / (skin.screen_width / 25.4);
            detail += &format!(" · real size ({device_ppi:.0} → {:.0} ppi)", draft * 25.4);
        }
        match settings.scaling {
            Scaling::Sharp if (wanted - wanted.round()).abs() >= 0.01 => detail += " · uneven pixels",
            Scaling::Crisp if (wanted - k as f32).abs() >= 0.01 => detail += &format!(" · {k}× then smoothed"),
            _ => {}
        }
        // Real size too large for the panel is cropped, which the picture says itself (see below).
        if real.is_none() && !fits(wanted) {
            detail += " · smaller here";
        }
        let title = format!("{} scaling", settings.scaling.label());
        (scale, Shown { title, detail, factor: times(wanted) })
    };
    // Centred on whole physical pixels.
    let size = vec2(pixels[0] as f32, pixels[1] as f32) * scale / ppp;
    let corner = space.center() - size / 2.0;
    let rect = Rect::from_min_size(egui::pos2((corner.x * ppp).round() / ppp, (corner.y * ppp).round() / ppp), size);
    let key = (app.screen_scaling.sample, screen);
    let (image, texture) = app.screen_scaling.pictures.entry(key).or_insert_with(|| {
        let decoded = image::load_from_memory(key.0.png(screen)).map(|i| i.to_rgba8());
        let image = match decoded {
            Ok(rgba) => ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], &rgba),
            Err(_) => ColorImage::filled([pixels[0] as usize, pixels[1] as usize], Color32::DARK_GRAY),
        };
        (image, Scaled::default())
    });
    texture.update(&ctx, "scaling-preview", false, settings.scaling, image.size, scaling::shown_px(rect, ppp), || {
        image.clone()
    });
    let painter = ui.painter().with_clip_rect(space.intersect(ui.clip_rect()));
    texture.paint(&painter, rect, Rotation::Portrait, false);
    if !space.expand(0.5).contains_rect(rect) {
        crop_note(ui, space, "Cropped to fit here");
    }
    shown
}

/// The screen at its real size (the draft measure) over 1 cm squares, in the drawn device.
fn squares(app: &mut App, ui: &mut Ui, space: Rect, screen: Screen, draft: f32) -> Shown {
    let ctx = ui.ctx().clone();
    let ppm = real_size::points_per_mm(draft, ctx.pixels_per_point());
    let skin = screen.skin();
    let placed = device_frame::place(&skin, space, Rotation::Portrait, app.model.settings.device_frame, Some(ppm));
    let painter = ui.painter().with_clip_rect(space.intersect(ui.clip_rect()));
    if let Some(layout) = &placed.device {
        device_frame::paint(&painter, &skin, layout, |_| false);
    }
    let screen_rect = placed.screen;
    painter.rect_filled(screen_rect, 0.0, Color32::from_rgb(0x14, 0x16, 0x1A));
    let grid = Stroke::new(1.0, Color32::from_white_alpha(40));
    let cm = 10.0 * ppm;
    let mut x = screen_rect.left() + cm;
    while x < screen_rect.right() - 0.5 {
        painter.line_segment([egui::pos2(x, screen_rect.top()), egui::pos2(x, screen_rect.bottom())], grid);
        x += cm;
    }
    let mut y = screen_rect.top() + cm;
    while y < screen_rect.bottom() - 0.5 {
        painter.line_segment([egui::pos2(screen_rect.left(), y), egui::pos2(screen_rect.right(), y)], grid);
        y += cm;
    }
    let visible = screen_rect.intersect(space);
    painter.text(
        visible.center(),
        Align2::CENTER_CENTER,
        "Squares are 1 cm",
        FontId::new(12.5, egui::FontFamily::Proportional),
        Color32::from_white_alpha(150),
    );
    let whole = placed.device.as_ref().map_or(screen_rect, |d| d.body);
    if !space.expand(0.5).contains_rect(whole) {
        crop_note(ui, space, "Cropped to fit here");
    }
    let size = format!("Screen {:.1} × {:.1} cm", skin.screen_width / 10.0, skin.screen_height / 10.0);
    let detail = format!("{size} · {}", fullscreen_fit(&skin, monitor_pixels(&ctx), draft));
    Shown { title: "Real size".into(), detail, factor: format!("{:.0} ppi", draft * 25.4) }
}

/// A note along the bottom of the preview: why part of the picture is missing.
fn crop_note(ui: &Ui, space: Rect, note: &str) {
    let painter = ui.painter();
    let galley = painter.layout_no_wrap(note.into(), FontId::new(12.0, theme::medium()), Color32::WHITE);
    let chip =
        Rect::from_center_size(egui::pos2(space.center().x, space.bottom() - 18.0), galley.size() + vec2(20.0, 10.0));
    painter.rect_filled(chip, CornerRadius::same(theme::radius::PILL), Color32::from_black_alpha(170));
    painter.galley(chip.center() - galley.size() / 2.0, galley, Color32::WHITE);
}

/// Whether full screen shows the whole screen at real size on this display.
fn fullscreen_fit(skin: &Skin, monitor: Option<[u32; 2]>, px_per_mm: f32) -> &'static str {
    let Some([w, h]) = monitor else { return "full screen shows as much of it as fits" };
    let (mw, mh) = (w as f32 / px_per_mm, h as f32 / px_per_mm);
    let (sw, sh) = (skin.screen_width, skin.screen_height);
    if sw <= mw && sh <= mh {
        "full screen shows all of it"
    } else if sh <= mw && sw <= mh {
        "full screen shows all of it turned to landscape"
    } else {
        "this display is smaller, so part of it is always cropped"
    }
}
