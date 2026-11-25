//! Full screen: the guest centred on black, with a button bar docked below it when there is room, or floating over
//! it (hiding after a few seconds without the mouse) when there is not. Settings can hide the bar for people who use
//! the keys instead. Esc and F11 leave.
use super::device_frame::{self, Skin};
use super::guest::ButtonControl;
use super::{App, Dialog, Page, icons, overlay, theme, widgets};
use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, Rect, Sense, Ui, vec2};
use lighttouch_model::session::Phase;
use lighttouch_model::settings::Scaling;
use std::time::{Duration, Instant};

const BAR: f32 = 88.0;
/// The bar's padding beyond its outermost button.
const PAD: f32 = 36.0;

/// How far the buttons reach left of Home's centre (Volume down's edge).
const LEFT: f32 = 150.0 + 24.0;

/// How far the buttons reach right of Home's centre (More's edge, or the frame inspector's past it).
fn right(app: &App) -> f32 {
    if app.model.settings.inspect_button { 205.0 + 22.0 } else { 150.0 + 24.0 }
}

pub fn show(app: &mut App, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    // With a mouse-look preset in use, Escape switches the mouse between looking and touching (the guest view does
    // that); the bar's button still leaves full screen.
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) && !app.guest.mouse_toggles() {
        app.set_fullscreen(&ctx, false);
        return;
    }
    let full = ui.max_rect();
    ui.painter().rect_filled(full, 0.0, Color32::BLACK);
    let board = app.model.current().map(|d| d.board.clone());
    let skin = Skin::for_board(board.as_deref());
    let rotation = app.model.settings.rotation;
    let pixels = app.guest.shown_pixels().unwrap_or_else(|| device_frame::screen_pixels(board.as_deref()));
    let place =
        |space| device_frame::place_for(&skin, space, rotation, false, &app.model.settings, &ctx, pixels).screen;
    let fit = place(full);
    let controls = app.model.settings.fullscreen_controls;
    let mut docked = controls && (full.height() - fit.height()) / 2.0 >= 0.8 * BAR;
    let mut screen = if docked { place(full.shrink2(vec2(0.0, BAR))) } else { fit };
    // Pixel perfect never steps down a multiple for the bar (a 2x landscape iPod touch 4G on a 1440-pixel display
    // became 1x): the bar takes room at the bottom only, the screen moving up, or it floats over the screen instead.
    if docked && app.model.settings.scaling == Scaling::PixelPerfect && screen.width() < fit.width() - 0.5 {
        let raised = place(Rect::from_min_max(full.min, full.max - vec2(0.0, BAR)));
        (screen, docked) = if raised.width() >= fit.width() - 0.5 { (raised, true) } else { (fit, false) };
    }
    let running = app.model.session.as_ref().is_some_and(|s| matches!(s.phase, Phase::Running));
    let asleep = app.model.session.as_ref().is_some_and(|s| s.display_asleep());
    let editing = app.model.gamepads.editing;
    app.guest.show(ui, screen, &mut app.model, running && !asleep && !editing);
    if running {
        overlay::show(ui, &app.model, screen.intersect(full), 1.0);
        super::controller::overlay(app, ui, screen);
    }
    super::controller::step(app, ui, screen);
    if asleep {
        let response = ui.interact(screen, ui.scope_id().with("wake"), Sense::click());
        ui.painter().text(
            screen.center(),
            Align2::CENTER_CENTER,
            "Screen off · click to wake",
            FontId::new(15.0, theme::medium()),
            Color32::from_white_alpha(180),
        );
        if response.clicked() {
            app.pulse_home();
        }
    }

    // Without the bar, a note on entering says how to leave (Esc toggles mouse-look when a preset uses it).
    if !controls {
        let text = if app.guest.mouse_toggles() { "F11 leaves full screen" } else { "Esc or F11 leaves full screen" };
        super::guest::hint_at(ui, egui::pos2(full.center().x, full.bottom() - 40.0), app.fullscreen_at, text);
        return;
    }

    // The floating bar shows while the mouse moves and hides after 4 s.
    let moved = ctx.input(|i| i.pointer.delta() != egui::Vec2::ZERO || i.pointer.any_pressed());
    let id = egui::Id::unique("fullscreen-bar-shown");
    let mut shown_at: Instant = ctx.data(|d| d.get_temp(id)).unwrap_or_else(Instant::now);
    if moved {
        shown_at = Instant::now();
    }
    ctx.data_mut(|d| d.insert_temp(id, shown_at));
    let visible = docked || shown_at.elapsed() < Duration::from_secs(4);
    if !docked && visible {
        ctx.request_repaint_after(Duration::from_millis(500));
    }
    if visible {
        // As wide as its buttons need, or the window allows (the buttons then move closer together).
        let width = (LEFT + right(app) + 2.0 * PAD).min(full.width() - 32.0);
        let bar = if docked {
            Rect::from_center_size(egui::pos2(full.center().x, screen.bottom() + BAR / 2.0), vec2(width, 64.0))
        } else {
            Rect::from_center_size(
                egui::pos2(full.center().x, full.bottom() - BAR / 2.0 - 12.0),
                vec2(width, BAR - 16.0),
            )
        };
        if !docked {
            ui.painter().rect_filled(bar, CornerRadius::same(theme::radius::PILL), Color32::from_black_alpha(140));
        }
        button_bar(app, ui, bar, running);
    }
}

fn button_bar(app: &mut App, ui: &mut Ui, bar: Rect, running: bool) {
    // Home's centre, placed so the buttons keep the same padding on both ends of the bar.
    let scale = ((bar.width() - 2.0 * PAD) / (LEFT + right(app))).clamp(0.0, 1.0);
    let center = egui::pos2(bar.left() + PAD + LEFT * scale, bar.center().y);
    let keys = [
        (device_frame::VOLUME_DOWN, icons::VOLUME_DOWN, -150.0, 24.0, "Volume down"),
        (device_frame::VOLUME_UP, icons::VOLUME_UP, -95.0, 24.0, "Volume up"),
        (device_frame::HOME, "", 0.0, 32.0, "Home"),
        (device_frame::POWER, icons::POWER, 95.0, 24.0, "Sleep/Wake"),
    ];
    for (button, icon, dx, radius, label) in keys {
        let c = center + vec2(dx * scale, 0.0);
        let rect = Rect::from_center_size(c, vec2(radius * 2.0, radius * 2.0));
        let response =
            ui.interact(rect, ui.scope_id().with(("fs-key", button)), Sense::click_and_drag()).on_hover_text(label);
        let down = running && response.is_pointer_button_down_on();
        app.guest.follow_button(&mut app.model, ButtonControl::Bar, button, down, running && response.clicked());
        let painter = ui.painter();
        if button == device_frame::HOME {
            painter.circle_stroke(
                c,
                radius - 1.0,
                egui::Stroke::new(2.0, Color32::from_white_alpha(if down { 255 } else { 190 })),
            );
            let glyph = Rect::from_center_size(c, vec2(radius * 0.62, radius * 0.62));
            painter.rect_stroke(
                glyph,
                CornerRadius::same(5),
                egui::Stroke::new(1.6, Color32::from_white_alpha(190)),
                egui::StrokeKind::Inside,
            );
        } else {
            painter.circle_filled(
                c,
                radius,
                Color32::from_white_alpha(if down {
                    60
                } else if response.hovered() {
                    34
                } else {
                    20
                }),
            );
            painter.text(
                c,
                Align2::CENTER_CENTER,
                icon,
                FontId::new(20.0, theme::icons()),
                Color32::from_white_alpha(if running { 230 } else { 90 }),
            );
        }
    }
    // The frame inspector, when its button is on (as on the home view's dock).
    if app.model.settings.inspect_button {
        let c = center + vec2(205.0 * scale, 0.0);
        let rect = Rect::from_center_size(c, vec2(44.0, 44.0));
        let response =
            ui.interact(rect, ui.scope_id().with("fs-inspect"), Sense::click()).on_hover_text("Inspect frames");
        ui.painter().circle_filled(c, 22.0, Color32::from_white_alpha(if response.hovered() { 34 } else { 20 }));
        ui.painter().text(c, Align2::CENTER_CENTER, icons::CHART, FontId::new(20.0, theme::icons()), Color32::WHITE);
        if response.clicked() {
            open_inspector(app, ui);
        }
    }
    // More: leave full screen, pause, stop.
    let more = Rect::from_center_size(center + vec2(150.0 * scale, 0.0), vec2(44.0, 44.0));
    let response = ui.interact(more, ui.scope_id().with("fs-more"), Sense::click()).on_hover_text("More");
    ui.painter().circle_filled(
        more.center(),
        22.0,
        Color32::from_white_alpha(if response.hovered() { 34 } else { 20 }),
    );
    ui.painter().text(
        more.center(),
        Align2::CENTER_CENTER,
        icons::MORE,
        FontId::new(20.0, theme::icons()),
        Color32::WHITE,
    );
    egui::Popup::menu(&response).show(|ui| {
        if ui.button(widgets::icon_label(icons::EXIT_FULLSCREEN, "Exit full screen", None)).clicked() {
            let ctx = ui.ctx().clone();
            app.set_fullscreen(&ctx, false);
        }
        let paused = app.model.session.as_ref().is_some_and(|s| matches!(s.phase, Phase::Paused));
        if ui
            .button(if paused {
                widgets::icon_label(icons::PLAY, "Resume", None)
            } else {
                widgets::icon_label(icons::PAUSE, "Pause", None)
            })
            .clicked()
        {
            app.model.set_paused(!paused);
        }
        if ui.button(widgets::icon_label(icons::CAMERA, "Screenshot", None)).clicked() {
            app.model.screenshot();
        }
        let rotation = app.model.settings.rotation;
        if ui.button(widgets::icon_label(icons::ROTATE, &format!("Rotate: {}", rotation.label()), None)).clicked() {
            app.model.settings.rotation = rotation.next();
            app.model.save_settings();
        }
        let level = app.model.settings.overlay;
        if ui
            .button(widgets::icon_label(icons::CHART, &format!("Performance overlay: {}", level.label()), None))
            .clicked()
        {
            app.model.settings.overlay = level.next();
            app.model.save_settings();
        }
        if ui.button(widgets::icon_label(icons::CHART, "Inspect frames", None)).clicked() {
            open_inspector(app, ui);
        }
        if super::carrier::wants_dock_button(app) {
            ui.menu_button(widgets::icon_label(icons::CELL_SIGNAL, "Carrier", None), |ui| {
                super::carrier::menu_items(app, ui);
            });
        }
        if ui.button(widgets::icon_label(icons::GEAR, "Settings", None)).clicked() {
            let ctx = ui.ctx().clone();
            app.set_fullscreen(&ctx, false);
            app.page = Page::Settings;
        }
        ui.separator();
        let stopping = app.model.session.as_ref().is_some_and(|s| s.stopping());
        let stop_label = if stopping { "Force stop" } else { "Stop" };
        if ui.button(widgets::icon_label(icons::POWER, stop_label, Some(theme::DANGER))).clicked() {
            app.dialog = Some(Dialog::Stop);
        }
    });
}

/// Pause the guest and open the frame inspector, out of full screen.
fn open_inspector(app: &mut App, ui: &Ui) {
    if let Some((snapshot, paused)) = app.model.inspect() {
        let ctx = ui.ctx().clone();
        app.set_fullscreen(&ctx, false);
        app.inspector = Some(super::inspector::InspectorState::new(snapshot, paused));
        app.page = Page::Inspector;
    }
}
