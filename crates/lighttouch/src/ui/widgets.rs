//! The app's reusable controls, styled from the palette. Buttons size to their label (never full width).
use super::theme::{self, Palette, radius};
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Frame, Margin, Response, RichText, Sense, Stroke, Ui, Vec2, vec2,
};

pub fn icon_text(icon: &str, size: f32) -> RichText {
    RichText::new(icon).family(theme::icons()).size(size)
}

fn button(ui: &mut Ui, text: RichText, fill: Color32, stroke: Stroke, height: f32, enabled: bool) -> Response {
    let button = egui::Button::new(text)
        .fill(fill)
        .stroke(stroke)
        .corner_radius(CornerRadius::same((height / 2.0) as u8))
        .min_size(vec2(0.0, height));
    ui.scope(|ui| {
        ui.spacing_mut().button_padding = vec2(height * 0.5, 0.0);
        ui.add_enabled(enabled, button)
    })
    .inner
}

/// The main action on a screen.
pub fn primary(ui: &mut Ui, p: &Palette, text: &str) -> Response {
    primary_enabled(ui, p, text, true)
}

pub fn primary_enabled(ui: &mut Ui, p: &Palette, text: &str, enabled: bool) -> Response {
    let color = if enabled { p.on_primary } else { p.on_surface_variant };
    let fill = if enabled { p.primary } else { p.container_highest };
    button(ui, RichText::new(text).color(color).size(15.0).family(theme::semibold()), fill, Stroke::NONE, 44.0, enabled)
}

/// A secondary action: tinted container.
pub fn tonal(ui: &mut Ui, p: &Palette, text: &str) -> Response {
    tonal_enabled(ui, p, text, true)
}

pub fn tonal_enabled(ui: &mut Ui, p: &Palette, text: &str, enabled: bool) -> Response {
    button(ui, RichText::new(text).color(p.on_surface), p.container_highest, Stroke::NONE, 40.0, enabled)
}

/// A destructive confirmation ("Shut down", "Delete").
pub fn danger(ui: &mut Ui, text: &str) -> Response {
    button(
        ui,
        RichText::new(text).color(Color32::WHITE).family(theme::semibold()),
        theme::DANGER.gamma_multiply(0.92),
        Stroke::NONE,
        40.0,
        true,
    )
}

/// A low-emphasis action: coloured text only.
pub fn text_button(ui: &mut Ui, p: &Palette, text: &str) -> Response {
    let r = ui.add(
        egui::Button::new(RichText::new(text).color(p.primary).family(theme::medium()))
            .frame(false)
            .min_size(vec2(0.0, 32.0)),
    );
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A round icon-only button.
pub fn icon_button(ui: &mut Ui, p: &Palette, icon: &str, tooltip: &str) -> Response {
    icon_button_sized(ui, p, icon, tooltip, 38.0, true)
}

pub fn icon_button_sized(ui: &mut Ui, p: &Palette, icon: &str, tooltip: &str, size: f32, enabled: bool) -> Response {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::splat(size), if enabled { Sense::click() } else { Sense::hover() });
    if ui.is_rect_visible(rect) {
        let fill = if response.is_pointer_button_down_on() {
            p.outline
        } else if response.hovered() && enabled {
            p.container_highest
        } else {
            Color32::TRANSPARENT
        };
        ui.painter().circle_filled(rect.center(), size / 2.0, fill);
        let color = if enabled { p.on_surface } else { p.on_surface_variant.gamma_multiply(0.5) };
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, icon, FontId::new(size * 0.52, theme::icons()), color);
    }
    let response = if enabled { response.on_hover_cursor(egui::CursorIcon::PointingHand) } else { response };
    response.on_hover_text(tooltip)
}

/// A raised group of rows or content.
pub fn card<R>(ui: &mut Ui, p: &Palette, add: impl FnOnce(&mut Ui) -> R) -> R {
    Frame::new()
        .fill(p.container)
        .corner_radius(CornerRadius::same(radius::CARD))
        .inner_margin(Margin::same(18))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// A group header above a card ("Display", "Controls").
pub fn section(ui: &mut Ui, p: &Palette, text: &str) {
    ui.add_space(14.0);
    ui.label(RichText::new(text).font(theme::label()).color(p.primary));
    ui.add_space(2.0);
}

pub fn status_dot(ui: &mut Ui, color: Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    ui.painter().circle_filled(rect.center(), size / 2.0, color);
}

/// An on/off switch.
pub fn switch(ui: &mut Ui, p: &Palette, on: &mut bool) -> Response {
    let size = vec2(44.0, 24.0);
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let t = ui.ctx().animate_bool_responsive(response.id, *on);
    let track = if *on { p.primary } else { p.container_highest };
    let painter = ui.painter();
    painter.rect(
        rect,
        CornerRadius::same(12),
        track,
        Stroke::new(1.0, if *on { track } else { p.outline }),
        egui::StrokeKind::Inside,
    );
    let x = egui::lerp(rect.left() + 12.0..=rect.right() - 12.0, t);
    let knob = if *on { p.on_primary } else { p.on_surface_variant };
    painter.circle_filled(egui::pos2(x, rect.center().y), if *on { 8.0 } else { 6.0 }, knob);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A row of mutually exclusive choices. Returns true when the choice changed.
pub fn segmented<T: PartialEq + Copy>(ui: &mut Ui, p: &Palette, value: &mut T, options: &[(T, &str)]) -> bool {
    let mut changed = false;
    let label = |text: &str, selected: bool| {
        RichText::new(text).color(if selected { p.on_primary } else { p.on_surface }).size(13.0)
    };
    // Sized to its buttons up front: a frame grown from its contents spans the whole free width of a
    // right-aligned row.
    let padding = ui.spacing().button_padding.x;
    let width = options
        .iter()
        .map(|(_, text)| {
            let galley = egui::WidgetText::from(label(text, false)).into_galley(
                ui,
                Some(egui::TextWrapMode::Extend),
                f32::INFINITY,
                egui::TextStyle::Button,
            );
            galley.size().x + 2.0 * padding
        })
        .sum::<f32>()
        + 2.0 * options.len().saturating_sub(1) as f32
        + 6.0;
    let layout = egui::Layout::left_to_right(egui::Align::Center);
    ui.allocate_ui_with_layout(vec2(width, 36.0), layout, |ui| {
        Frame::new()
            .fill(p.container_highest)
            .corner_radius(CornerRadius::same(18))
            .inner_margin(Margin::same(3))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for (option, text) in options {
                    let selected = *value == *option;
                    let r = ui.add(
                        egui::Button::new(label(text, selected))
                            .fill(if selected { p.primary } else { Color32::TRANSPARENT })
                            .corner_radius(CornerRadius::same(15))
                            .min_size(vec2(0.0, 30.0)),
                    );
                    if r.clicked() && !selected {
                        *value = *option;
                        changed = true;
                    }
                }
            });
    });
    changed
}

/// A filter choice among several (chart type, sort order): a pill, filled when selected.
pub fn chip(ui: &mut Ui, p: &Palette, selected: bool, text: &str) -> Response {
    let label = RichText::new(text).size(13.0).color(if selected { p.on_primary_container } else { p.on_surface });
    let r = ui.add(
        egui::Button::new(label)
            .fill(if selected { p.primary_container } else { Color32::TRANSPARENT })
            .stroke(if selected { Stroke::NONE } else { Stroke::new(1.0, p.outline) })
            .corner_radius(CornerRadius::same(10))
            .min_size(vec2(0.0, 30.0)),
    );
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A chip that opens a menu: its current choice and a caret.
pub fn chip_menu(ui: &mut Ui, p: &Palette, text: &str) -> Response {
    let mut job = egui::text::LayoutJob::default();
    let format = |font: FontId| egui::TextFormat {
        font_id: font,
        color: p.on_surface,
        valign: egui::Align::Center,
        ..Default::default()
    };
    job.append(text, 0.0, format(FontId::new(13.0, egui::FontFamily::Proportional)));
    job.append(super::icons::CARET_DOWN, 6.0, format(FontId::new(13.0, theme::icons())));
    let r = ui.add(
        egui::Button::new(job)
            .fill(Color32::TRANSPARENT)
            .stroke(Stroke::new(1.0, p.outline))
            .corner_radius(CornerRadius::same(10))
            .min_size(vec2(0.0, 30.0)),
    );
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// One button of the dock under the device: icon over a small label.
pub fn dock_button(
    ui: &mut Ui,
    p: &Palette,
    icon: &str,
    label: &str,
    enabled: bool,
    tint: Option<Color32>,
) -> Response {
    let size = vec2(72.0, 54.0);
    let (rect, response) = ui.allocate_exact_size(size, if enabled { Sense::click() } else { Sense::hover() });
    let alpha = if enabled { 1.0 } else { 0.38 };
    if response.hovered() && enabled {
        ui.painter().rect_filled(rect, CornerRadius::same(14), p.container_highest);
    }
    let color = tint.unwrap_or(p.on_surface).gamma_multiply(alpha);
    ui.painter().text(
        rect.center() - vec2(0.0, 8.0),
        Align2::CENTER_CENTER,
        icon,
        FontId::new(22.0, theme::icons()),
        color,
    );
    ui.painter().text(
        rect.center() + vec2(0.0, 15.0),
        Align2::CENTER_CENTER,
        label,
        FontId::new(11.5, theme::medium()),
        tint.unwrap_or(p.on_surface_variant).gamma_multiply(alpha),
    );
    if enabled { response.on_hover_cursor(egui::CursorIcon::PointingHand) } else { response }
}

/// A rounded progress bar; indeterminate (a moving segment) when `fraction` is `None`.
pub fn progress(ui: &mut Ui, p: &Palette, fraction: Option<f32>, width: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 6.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(3), p.container_highest);
    match fraction {
        Some(f) => {
            let mut fill = rect;
            fill.set_width((rect.width() * f.clamp(0.0, 1.0)).max(6.0));
            painter.rect_filled(fill, CornerRadius::same(3), p.primary);
        }
        None => {
            let t = (ui.input(|i| i.time) * 0.8).fract() as f32;
            let seg = rect.width() * 0.3;
            let x0 = rect.left() + (rect.width() + seg) * t - seg;
            let fill = egui::Rect::from_min_max(
                egui::pos2(x0.max(rect.left()), rect.top()),
                egui::pos2((x0 + seg).min(rect.right()), rect.bottom()),
            );
            if fill.width() > 0.0 {
                painter.rect_filled(fill, CornerRadius::same(3), p.primary);
            }
            ui.ctx().request_repaint();
        }
    }
}

/// A round busy indicator in the primary colour.
pub fn spinner(ui: &mut Ui, p: &Palette, size: f32) {
    ui.add(egui::Spinner::new().size(size).color(p.primary));
}

/// A dialog's actions, right-aligned in a row of their own height (a right-to-left layout alone takes all the
/// height a modal offers, which made dialogs tall with their buttons floating in the middle).
pub fn dialog_buttons<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let layout = egui::Layout::right_to_left(egui::Align::Center);
    ui.allocate_ui_with_layout(vec2(ui.available_width(), 40.0), layout, add).inner
}

/// A row in a settings card: icon, title, subtitle, and a trailing control.
pub fn setting_row<R>(
    ui: &mut Ui,
    p: &Palette,
    icon: &str,
    title: &str,
    subtitle: &str,
    trailing: impl FnOnce(&mut Ui) -> R,
) -> R {
    // The trailing control's width, as laid out last frame.
    let id = ui.scope_id().with(("setting-row", title));
    let reserved = ui.data(|d| d.get_temp::<f32>(id)).unwrap_or(150.0);
    ui.horizontal(|ui| {
        ui.set_min_height(48.0);
        ui.add_space(2.0);
        ui.label(icon_text(icon, 20.0).color(p.on_surface_variant));
        ui.add_space(6.0);
        ui.vertical(|ui| {
            // Leave the trailing control its room: a long subtitle wraps instead of running under it.
            ui.set_max_width((ui.available_width() - reserved - 16.0).max(120.0));
            ui.add_space(if subtitle.is_empty() { 14.0 } else { 4.0 });
            ui.label(RichText::new(title).size(14.5));
            if !subtitle.is_empty() {
                ui.label(RichText::new(subtitle).size(12.5).color(p.on_surface_variant));
            }
        });
        let trailing = ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), trailing);
        let width = trailing.response.rect.width();
        if (width - reserved).abs() > 0.5 {
            ui.data_mut(|d| d.insert_temp(id, width));
            ui.ctx().request_discard("a settings row's control changed width");
        }
        trailing.inner
    })
    .inner
}

/// A key or mouse action, drawn as a key cap (the Controls dialog).
pub fn key_cap(ui: &mut Ui, p: &Palette, keys: &str) {
    Frame::new()
        .fill(p.container_highest)
        .stroke(Stroke::new(1.0, p.outline.gamma_multiply(0.5)))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(8, 3))
        .show(ui, |ui| ui.label(RichText::new(keys).font(FontId::new(13.0, theme::medium()))));
}

pub fn muted(p: &Palette, text: impl Into<String>) -> RichText {
    RichText::new(text.into()).color(p.on_surface_variant)
}

/// An icon followed by a label, each in its own font (Inter's private-use glyphs overlap Phosphor's codepoints, so
/// the two never share a text run). `color` of `None` follows the widget's text colour.
pub fn icon_label(icon: &str, text: &str, color: Option<Color32>) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let format = |font: FontId| egui::TextFormat {
        font_id: font,
        color: color.unwrap_or(Color32::PLACEHOLDER),
        valign: egui::Align::Center,
        ..Default::default()
    };
    job.append(icon, 0.0, format(FontId::new(16.0, theme::icons())));
    job.append(text, 10.0, format(FontId::new(14.0, egui::FontFamily::Proportional)));
    job
}
