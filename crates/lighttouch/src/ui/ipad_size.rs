//! Settings → iPad size: show iPads at the real device's size, for toys and pieces used on the screen. The display's
//! density is measured against a card or a ruler held to the screen (or worked out from its diagonal); the preview
//! beside it draws the iPad at that size as it is adjusted. The measure applies to every iPad.
use super::device_frame::{self, Skin};
use super::{App, Page, icons, theme, widgets};
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Frame, Margin, Rect, RichText, Sense, Stroke, Ui, vec2,
};
use lighttouch_model::real_size::{self, CARD_CORNER, CARD_HEIGHT, CARD_WIDTH, DisplayMeasure};

pub const TABLET: &str = "\u{E1E6}";
const MINUS: &str = "\u{E32A}";

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Reference {
    #[default]
    Card,
    Ruler,
}

/// The page's unsaved state: the density being adjusted, and what it is matched against.
#[derive(Default)]
pub struct IpadSizeState {
    draft: Option<f32>,
    reference: Reference,
    /// The card stands upright: on its side it wouldn't fit beside the preview. Changed only between drags.
    portrait: bool,
}

/// Open the page from Settings, starting from the saved measure.
pub fn open(app: &mut App) {
    app.ipad_size.draft = None;
    app.page = Page::IpadSize;
}

/// The Settings row's subtitle.
pub fn summary(app: &App) -> &'static str {
    if app.model.settings.ipad_px_per_mm().is_some() {
        "On: iPads show at the real device's size"
    } else {
        "Show iPads at the real device's size, for toys and pieces used on the screen"
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
    let p = app.palette;
    let ctx = ui.ctx().clone();
    let native = ctx.input(|i| i.viewport().native_pixels_per_point).unwrap_or(ctx.pixels_per_point());
    let saved = app.model.settings.display;
    let mut draft =
        *app.ipad_size.draft.get_or_insert_with(|| saved.map_or_else(|| real_size::guess(native), |d| d.px_per_mm));
    let before = (app.model.settings.ipad_real_size, app.model.settings.display);

    let area = ui.available_rect_before_wrap();
    // The settings column holds the card at this density, on its side or else upright, with its handle, padding and
    // the scroll bar: on a display denser than the first guess it widens, leaving the preview at least 320 points,
    // and when even the upright card leaves less, the preview goes below the settings instead.
    let ppm = real_size::points_per_mm(draft, ctx.pixels_per_point());
    let (side_room, upright_room) = (CARD_WIDTH * ppm + 90.0, CARD_HEIGHT * ppm + 90.0);
    let cap = area.width() - 48.0 - 28.0 - 320.0;
    let wide = area.width() >= 820.0 && upright_room.max(420.0) <= cap.max(420.0);
    let column = if !wide {
        (area.width() - 48.0).max(200.0)
    } else if side_room <= cap {
        side_room.max(420.0)
    } else {
        upright_room.clamp(420.0, cap.max(420.0))
    };
    let left = Rect::from_min_size(area.min + vec2(24.0, 0.0), vec2(column, area.height()));
    let mut left_ui = ui.new_child(egui::UiBuilder::new().id_salt("ipad-size-settings").max_rect(left));
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(&mut left_ui, |ui| {
        // Room for the scroll bar beside the cards.
        ui.set_max_width(column - 14.0);
        ui.add_space(18.0);
        ui.horizontal(|ui| {
            if widgets::icon_button(ui, &p, icons::BACK, "Back to settings").clicked() {
                app.page = Page::Settings;
            }
            ui.label(RichText::new("iPad size").font(theme::headline()));
        });
        settings(app, ui, &mut draft, native);
        if !wide {
            ui.add_space(70.0);
            let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 480.0), Sense::hover());
            preview(app, ui, rect, draft);
        }
        ui.add_space(28.0);
    });
    if wide {
        let right = Rect::from_min_max(egui::pos2(left.right() + 28.0, area.top() + 18.0), area.max - vec2(24.0, 24.0));
        let mut right_ui = ui.new_child(egui::UiBuilder::new().id_salt("ipad-size-preview").max_rect(right));
        right_ui.add_space(58.0);
        let rest = Rect::from_min_max(right_ui.cursor().min, right.max);
        preview(app, &mut right_ui, rest, draft);
    }
    ui.allocate_rect(area, Sense::hover());

    app.ipad_size.draft = Some(draft);
    if (app.model.settings.ipad_real_size, app.model.settings.display) != before {
        app.model.save_settings();
    }
}

fn settings(app: &mut App, ui: &mut Ui, draft: &mut f32, native: f32) {
    let p = app.palette;
    let ctx = ui.ctx().clone();
    let ppp = ctx.pixels_per_point();
    let monitor = monitor_pixels(&ctx);
    let saved = app.model.settings.display;
    let measure = |px_per_mm: f32| DisplayMeasure { px_per_mm, monitor, estimate: false };

    widgets::card(ui, &p, |ui| {
        let mut on = app.model.settings.ipad_real_size;
        widgets::setting_row(
            ui,
            &p,
            TABLET,
            "Show iPads at real size",
            "The screen matches a real iPad's, 9.7 inches across. When the window is smaller, the edges are cropped",
            |ui| widgets::switch(ui, &p, &mut on),
        );
        if on != app.model.settings.ipad_real_size {
            app.model.settings.ipad_real_size = on;
            // Turned on before any measure: the size shown here is the one to use, until one is saved.
            if on && saved.is_none() {
                app.model.settings.display = Some(DisplayMeasure { estimate: true, ..measure(*draft) });
            }
        }
    });

    widgets::section(ui, &p, "Measure this display");
    widgets::card(ui, &p, |ui| {
        // On its side when it fits, else upright; never switched mid-drag.
        let fits = CARD_WIDTH * real_size::points_per_mm(*draft, ppp) + 24.0 <= ui.available_width();
        if !ui.input(|i| i.pointer.any_down()) {
            app.ipad_size.portrait = !fits;
        }
        let portrait = app.ipad_size.portrait;
        let reference = &mut app.ipad_size.reference;
        widgets::segmented(ui, &p, reference, &[(Reference::Card, "Bank card"), (Reference::Ruler, "Ruler")]);
        ui.add_space(8.0);
        let text = match reference {
            Reference::Card if portrait => {
                "Hold a bank or ID card upright against the screen, its corner in the outline's top-left corner. \
                 Drag the handle until the outline's bottom edge meets the card's."
            }
            Reference::Card => {
                "Hold a bank or ID card flat against the screen, its corner in the outline's top-left corner. Drag \
                 the handle until the outline's right edge meets the card's."
            }
            Reference::Ruler => {
                "Lay a ruler along the marks, its 0 on the first one. Drag the handle until the last mark meets the \
                 same number on the ruler."
            }
        };
        ui.label(widgets::muted(&p, text).size(12.5));
        ui.add_space(10.0);
        let length = match reference {
            Reference::Card => card(ui, &p, draft, ppp, portrait),
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
                let range =
                    real_size::per_inch(real_size::MIN_PX_PER_MM)..=real_size::per_inch(real_size::MAX_PX_PER_MM);
                let edit =
                    egui::DragValue::new(&mut ppi).speed(0.1).range(range).fixed_decimals(1).suffix(" pixels/inch");
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
                    let edit = egui::DragValue::new(&mut inches)
                        .speed(0.05)
                        .range(3.0..=120.0)
                        .fixed_decimals(1)
                        .suffix(" in");
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
                app.model.settings.ipad_real_size = true;
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
            } else {
                ui.label(
                    widgets::muted(&p, format!("Saved: {:.1} pixels per inch", real_size::per_inch(s.px_per_mm)))
                        .size(12.5),
                );
            }
        }
    });
    Frame::new().inner_margin(Margin { left: 18, right: 18, top: 8, bottom: 0 }).show(ui, |ui| {
        ui.label(
            widgets::muted(
                &p,
                "Measure again after moving LightTouch to another display or changing its resolution. Display scaling \
                 and zoom are allowed for.",
            )
            .size(12.5),
        );
    });
}

/// A card's outline at the draft density, its right edge dragged to fit. Returns the reference's length in mm.
fn card(ui: &mut Ui, p: &theme::Palette, draft: &mut f32, ppp: f32, upright: bool) -> f32 {
    let ppm = real_size::points_per_mm(*draft, ppp);
    let size = if upright { vec2(CARD_HEIGHT, CARD_WIDTH) } else { vec2(CARD_WIDTH, CARD_HEIGHT) } * ppm;
    let (area, _) =
        ui.allocate_exact_size(vec2(ui.available_width(), size.y + if upright { 16.0 } else { 4.0 }), Sense::hover());
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
    if upright {
        grip(&painter, p, outline.center_bottom(), true);
        let handle = Rect::from_center_size(outline.center_bottom(), vec2(outline.width(), 36.0));
        drag(ui, handle, true, draft, ppp, CARD_WIDTH);
    } else {
        grip(&painter, p, outline.right_center(), false);
        let handle = Rect::from_center_size(outline.right_center(), vec2(36.0, area.height()));
        drag(ui, handle, false, draft, ppp, CARD_WIDTH);
    }
    CARD_WIDTH
}

/// Ruler marks at the draft density: millimetres, half centimetres and numbered centimetres, as long as fits.
fn ruler(ui: &mut Ui, p: &theme::Palette, draft: &mut f32, ppp: f32) -> f32 {
    let ppm = real_size::points_per_mm(*draft, ppp);
    let room = ui.available_width() - 24.0;
    let length = [150.0, 100.0, 80.0, 50.0].into_iter().find(|mm| mm * ppm <= room).unwrap_or(50.0);
    let (area, _) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::hover());
    let painter = ui.painter().with_clip_rect(area.intersect(ui.clip_rect()));
    let x0 = area.left() + 6.0;
    let top = area.top() + 2.0;
    let line = Stroke::new(1.0, p.on_surface);
    painter.line_segment([egui::pos2(x0, top), egui::pos2(x0 + length * ppm, top)], line);
    for mm in 0..=length as u32 {
        let x = x0 + mm as f32 * ppm;
        let (tall, stroke) = if mm % 10 == 0 {
            (16.0, Stroke::new(1.5, p.primary))
        } else if mm % 5 == 0 {
            (10.0, line)
        } else {
            (5.0, Stroke::new(1.0, p.on_surface_variant))
        };
        painter.line_segment([egui::pos2(x, top), egui::pos2(x, top + tall)], stroke);
        if mm % 10 == 0 {
            painter.text(
                egui::pos2(x, top + 20.0),
                Align2::CENTER_TOP,
                format!("{}", mm / 10),
                FontId::new(11.0, egui::FontFamily::Proportional),
                p.on_surface_variant,
            );
        }
    }
    grip(&painter, p, egui::pos2(x0 + length * ppm, top + 8.0), false);
    let handle = Rect::from_center_size(egui::pos2(x0 + length * ppm, area.center().y), vec2(36.0, area.height()));
    drag(ui, handle, false, draft, ppp, length);
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

/// The iPad drawn at the draft density: the live screen when an iPad is running, else a centimetre grid.
fn preview(app: &mut App, ui: &mut Ui, rect: Rect, draft: f32) {
    let p = app.palette;
    let ctx = ui.ctx().clone();
    let ppp = ctx.pixels_per_point();
    let ppm = real_size::points_per_mm(draft, ppp);
    let rotation = app.model.settings.rotation;
    let skin = Skin::IPAD;

    // Above the picture: what will show where.
    let caption = fullscreen_fit(&skin, monitor_pixels(&ctx), draft);
    let head = Rect::from_min_size(rect.min - vec2(0.0, 52.0), vec2(rect.width(), 48.0));
    let mut head_ui =
        ui.new_child(egui::UiBuilder::new().max_rect(head).layout(egui::Layout::bottom_up(egui::Align::Min)));
    let screen_mm = format!("Screen {:.1} × {:.1} cm", skin.screen_width / 10.0, skin.screen_height / 10.0);
    head_ui.label(widgets::muted(&p, format!("{screen_mm}. {caption}")).size(12.5));
    head_ui.label(RichText::new("Preview at real size").font(theme::label()).color(p.primary));

    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    painter.rect_filled(rect, CornerRadius::same(theme::radius::CARD), p.container);
    let space = rect.shrink(12.0);
    let placed = device_frame::place(&skin, space, rotation, app.model.settings.device_frame, Some(ppm));
    let screen = placed.screen;
    let screen_painter = painter.with_clip_rect(space.intersect(painter.clip_rect()));
    if let Some(layout) = &placed.device {
        device_frame::paint(&screen_painter, &skin, layout, |_| false);
    }
    let live = app.model.current().is_some_and(|d| Skin::for_board(Some(&d.board)) == skin)
        && app.guest.paint(&screen_painter, screen, rotation);
    if !live {
        screen_painter.rect_filled(screen, 0.0, Color32::from_rgb(0x14, 0x16, 0x1A));
        let grid = Stroke::new(1.0, Color32::from_white_alpha(26));
        let cm = 10.0 * ppm;
        let mut x = screen.left() + cm;
        while x < screen.right() - 0.5 {
            screen_painter.line_segment([egui::pos2(x, screen.top()), egui::pos2(x, screen.bottom())], grid);
            x += cm;
        }
        let mut y = screen.top() + cm;
        while y < screen.bottom() - 0.5 {
            screen_painter.line_segment([egui::pos2(screen.left(), y), egui::pos2(screen.right(), y)], grid);
            y += cm;
        }
        let visible = screen.intersect(space);
        screen_painter.text(
            visible.center() - vec2(0.0, 10.0),
            Align2::CENTER_CENTER,
            TABLET,
            FontId::new(30.0, theme::icons()),
            Color32::from_white_alpha(120),
        );
        screen_painter.text(
            visible.center() + vec2(0.0, 22.0),
            Align2::CENTER_CENTER,
            "Squares are 1 cm",
            FontId::new(12.5, egui::FontFamily::Proportional),
            Color32::from_white_alpha(140),
        );
    }
    let whole = placed.device.as_ref().map_or(screen, |d| d.body);
    if !space.expand(0.5).contains_rect(whole) {
        // Cropped here: say so at the bottom of the picture.
        let note = "Cropped to fit here";
        let galley = painter.layout_no_wrap(note.into(), FontId::new(12.0, theme::medium()), Color32::WHITE);
        let chip = Rect::from_center_size(
            egui::pos2(space.center().x, space.bottom() - 18.0),
            galley.size() + vec2(20.0, 10.0),
        );
        painter.rect_filled(chip, CornerRadius::same(theme::radius::PILL), Color32::from_black_alpha(170));
        painter.galley(chip.center() - galley.size() / 2.0, galley, Color32::WHITE);
    }
}

/// Whether full screen shows the whole iPad screen on this display.
fn fullscreen_fit(skin: &Skin, monitor: Option<[u32; 2]>, px_per_mm: f32) -> &'static str {
    let Some([w, h]) = monitor else { return "Full screen shows as much of it as fits" };
    let (mw, mh) = (w as f32 / px_per_mm, h as f32 / px_per_mm);
    let (sw, sh) = (skin.screen_width, skin.screen_height);
    if sw <= mw && sh <= mh {
        "Full screen shows all of it"
    } else if sh <= mw && sw <= mh {
        "Full screen shows all of it turned to landscape"
    } else {
        "This display is smaller than an iPad's screen, so part of it is always cropped"
    }
}
