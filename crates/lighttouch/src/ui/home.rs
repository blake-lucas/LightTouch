//! The device page: the selected device drawn with its live screen, what state it is in, and the dock of session
//! controls while it runs. Start lives only on the screen itself (one control per action).
use super::device_frame::{self, Skin};
use super::{App, Dialog, Page, icons, overlay, sidebar, theme, widgets};
use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, Frame, Margin, Rect, RichText, Sense, Ui, vec2};
use lighttouch_model::session::Phase;

pub fn show(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    top_bar(app, ui);
    let active = app.model.session_active();
    // The dock's room stays while a device is selected, so starting, stopping or failing never resizes the device.
    let dock_height = if app.model.current().is_some() { 92.0 } else { 0.0 };
    let space = Rect::from_min_max(ui.cursor().min, ui.max_rect().max - vec2(0.0, dock_height));
    // A soft glow behind the device.
    glow(ui, space, p.primary.gamma_multiply(if p.dark { 0.07 } else { 0.10 }));

    let board = app.model.current().map(|d| d.board.clone());
    let skin = Skin::for_board(board.as_deref());
    let device_space = space.shrink2(vec2(24.0, 16.0));
    let screen = if app.model.settings.device_frame {
        let layout = device_frame::layout_turned(&skin, device_space, app.model.settings.rotation);
        device_frame::paint(ui.painter(), &skin, &layout, |b| app.guest.is_held(b));
        hardware_buttons(app, ui, &layout);
        layout.screen
    } else {
        fit_aspect(device_space, skin.shown_aspect(app.model.settings.rotation))
    };
    let running = app.model.session.as_ref().is_some_and(|s| matches!(s.phase, Phase::Running));
    let asleep = app.model.session.as_ref().is_some_and(|s| s.display_asleep());
    app.guest.show(ui, screen, &mut app.model, running && !asleep);
    if running && app.model.settings.overlay_in_device_view {
        overlay::show(ui, &app.model, screen, 0.85);
    }
    state_card(app, ui, screen);

    if active {
        let dock = Rect::from_min_max(egui::pos2(space.left(), space.bottom()), ui.max_rect().max);
        dock_bar(app, ui, dock);
    }
}

fn top_bar(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    Frame::new().inner_margin(Margin { left: 24, right: 16, top: 14, bottom: 6 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.set_min_height(44.0);
            let (name, subtitle) = match app.model.current() {
                Some(d) => (d.name.clone(), d.subtitle.clone()),
                None => ("No device".to_string(), String::new()),
            };
            let (status, color) = match &app.model.session {
                Some(s) => (s.phase.label().to_string(), sidebar::phase_color(&s.phase)),
                None => ("Off".to_string(), p.outline),
            };
            widgets::status_dot(ui, color, 10.0);
            ui.add_space(4.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(RichText::new(name).font(theme::title()));
                let detail = if subtitle.is_empty() { status } else { format!("{subtitle} · {status}") };
                ui.label(widgets::muted(&p, detail).size(12.5));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let has_device = app.model.current().is_some();
                if widgets::icon_button_sized(ui, &p, icons::BAG, "Legacy Store", 40.0, has_device).clicked() {
                    app.page = super::Page::Store;
                }
                error_chip(app, ui);
            });
        });
    });
}

/// The last error, in the header beside the device's name rather than over it: one line, the whole message on
/// hover, dismissable.
fn error_chip(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    let Some(error) = app.model.error.clone() else { return };
    ui.add_space(8.0);
    let close = 28.0;
    let text = RichText::new(&error).size(12.5).color(p.on_error_container);
    let galley = egui::WidgetText::from(text.clone()).into_galley(
        ui,
        Some(egui::TextWrapMode::Extend),
        f32::INFINITY,
        egui::TextStyle::Body,
    );
    // The frame's margins, the icon, the close button and the gaps between them.
    let spacing = ui.spacing().item_spacing.x;
    let chrome = 12.0 + 4.0 + 20.0 + close + 8.0 + 3.0 * spacing;
    let width = (galley.size().x + chrome).min(ui.available_width() - 16.0).min(560.0);
    if width < 120.0 {
        return;
    }
    let mut dismiss = false;
    ui.allocate_ui_with_layout(vec2(width, 36.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        Frame::new()
            .fill(p.error_container)
            .corner_radius(CornerRadius::same(theme::radius::ROW))
            .inner_margin(Margin { left: 12, right: 4, top: 2, bottom: 2 })
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(widgets::icon_text(icons::WARNING, 16.0).color(p.on_error_container));
                    ui.scope(|ui| {
                        ui.set_max_width((ui.available_width() - close - 8.0).max(40.0));
                        ui.add(egui::Label::new(text).truncate()).on_hover_text(&error);
                    });
                    if widgets::icon_button_sized(ui, &p, icons::CLOSE, "Dismiss", close, true).clicked() {
                        dismiss = true;
                    }
                });
            });
    });
    if dismiss {
        app.model.dismiss_error();
    }
}

fn glow(ui: &Ui, rect: Rect, color: Color32) {
    let center = rect.center();
    let radius = rect.width().max(rect.height()) * 0.55;
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(center, color);
    let steps = 64;
    for i in 0..steps {
        let a = std::f32::consts::TAU * i as f32 / steps as f32;
        mesh.colored_vertex(center + vec2(a.cos() * radius, a.sin() * radius * 0.8), Color32::TRANSPARENT);
    }
    for i in 0..steps as u32 {
        mesh.add_triangle(0, 1 + i, 1 + (i + 1) % steps as u32);
    }
    ui.painter().with_clip_rect(rect).add(egui::Shape::mesh(mesh));
}

pub fn fit_aspect(space: Rect, aspect: f32) -> Rect {
    let (w, h) = if space.width() / space.height() > aspect {
        (space.height() * aspect, space.height())
    } else {
        (space.width(), space.width() / aspect)
    };
    Rect::from_center_size(space.center(), vec2(w, h))
}

/// Home and the side buttons on the drawn device: press and hold, like hardware.
fn hardware_buttons(app: &mut App, ui: &mut Ui, layout: &device_frame::Layout) {
    let enabled = app.model.session.as_ref().is_some_and(|s| matches!(s.phase, Phase::Running | Phase::Starting));
    let mut targets = vec![(device_frame::HOME, layout.home, "Home")];
    for (id, _, hit) in layout.side {
        let label = match id {
            device_frame::POWER => "Sleep/Wake",
            device_frame::VOLUME_UP => "Volume up",
            _ => "Volume down",
        };
        targets.push((id, hit, label));
    }
    for (id, rect, label) in targets {
        let response = ui.interact(rect, ui.scope_id().with(("hw", id)), Sense::click_and_drag());
        let response = if enabled {
            response.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(label)
        } else {
            response
        };
        let down = enabled && response.is_pointer_button_down_on();
        if down != app.guest.is_held(id) {
            app.guest.set_button(&mut app.model, id, down);
        }
    }
}

/// What the screen shows when iOS isn't simply running: progress, a paused or asleep hint, or Start.
fn state_card(app: &mut App, ui: &mut Ui, screen: Rect) {
    let p = app.palette;
    enum Card {
        None,
        Busy(String, Option<f32>),
        Action(&'static str, String),
        Failed(String),
    }
    let card = match &app.model.session {
        Some(s) => match &s.phase {
            Phase::Verifying { done, total } => {
                let fraction = (*total > 0).then(|| *done as f32 / *total as f32);
                Card::Busy("Checking the device's files…".into(), fraction)
            }
            Phase::Starting if !app.guest.has_frame() => Card::Busy("Starting iOS…".into(), None),
            Phase::Starting => Card::None,
            Phase::Stopping => Card::Busy("Shutting down…".into(), None),
            Phase::Paused => Card::Action(icons::PLAY, "Paused · click to resume".into()),
            Phase::Running if s.display_asleep() => Card::Action(icons::POWER, "Screen off · click to wake".into()),
            Phase::Running => Card::None,
            Phase::Ended { clean: false, message, .. } => Card::Failed(message.clone()),
            Phase::Ended { .. } => Card::None,
        },
        None => match app.model.selected() {
            None if app.model.busy().is_some() => {
                let fraction = app.model.busy().and_then(|(_, progress)| progress.fraction());
                Card::Busy("Preparing a device…".into(), fraction)
            }
            None => Card::Action(icons::PLUS, "Add a device".into()),
            Some(d) if d.dirty => Card::Action(icons::RECOVER, "Recover and start".into()),
            Some(_) => Card::Action(icons::PLAY, "Start".into()),
        },
    };
    if matches!(card, Card::None) {
        return;
    }
    let asleep = app.model.session.as_ref().is_some_and(|s| s.display_asleep());
    let scrim = if asleep { 0.35 } else { 0.7 };
    let painter = ui.painter().with_clip_rect(screen);
    if app.guest.has_frame() || app.model.session.is_some() {
        painter.rect_filled(screen, 0.0, Color32::from_black_alpha((255.0 * scrim) as u8));
    }
    let center = screen.center();
    match card {
        Card::None => {}
        Card::Busy(text, fraction) => {
            let area = Rect::from_center_size(center, vec2(screen.width() * 0.8, 90.0));
            ui.scope_builder(
                egui::UiBuilder::new().max_rect(area).layout(egui::Layout::top_down(egui::Align::Center)),
                |ui| {
                    widgets::spinner(ui, &p, 28.0);
                    ui.add_space(8.0);
                    ui.label(RichText::new(text).color(Color32::from_white_alpha(220)).size(13.5));
                    if fraction.is_some() {
                        ui.add_space(6.0);
                        widgets::progress(ui, &p, fraction, (screen.width() * 0.6).min(200.0));
                    }
                },
            );
        }
        Card::Action(icon, label) => {
            let circle = egui::Rect::from_center_size(center - vec2(0.0, 14.0), vec2(64.0, 64.0));
            let hit = circle.union(Rect::from_center_size(center + vec2(0.0, 34.0), vec2(screen.width() * 0.9, 24.0)));
            let response =
                ui.interact(if asleep { screen } else { hit }, ui.scope_id().with("state-action"), Sense::click());
            let hovered = response.hovered();
            painter.circle_filled(circle.center(), 32.0, Color32::from_white_alpha(if hovered { 56 } else { 36 }));
            painter.text(
                circle.center(),
                Align2::CENTER_CENTER,
                icon,
                FontId::new(30.0, theme::icons_fill()),
                Color32::WHITE,
            );
            painter.text(
                center + vec2(0.0, 34.0),
                Align2::CENTER_CENTER,
                &label,
                FontId::new(14.0, theme::medium()),
                Color32::from_white_alpha(220),
            );
            let missing_rom = app.model.session.is_none() && app.model.selected().is_some_and(|d| d.missing_bootrom);
            if missing_rom {
                painter.text(
                    center + vec2(0.0, 58.0),
                    Align2::CENTER_CENTER,
                    "Its boot file is missing",
                    FontId::new(12.0, egui::FontFamily::Proportional),
                    theme::WARNING,
                );
            }
            if response.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                match app.model.session.as_ref().map(|s| s.phase.clone()) {
                    Some(Phase::Paused) => app.model.set_paused(false),
                    Some(Phase::Running) => app.pulse_home(),
                    _ => match app.model.selected().map(|d| d.dirty) {
                        None => super::prepare::open(app),
                        Some(dirty) => app.model.start(dirty),
                    },
                }
            }
        }
        Card::Failed(message) => {
            painter.text(
                center - vec2(0.0, 20.0),
                Align2::CENTER_CENTER,
                icons::WARNING,
                FontId::new(30.0, theme::icons()),
                theme::WARNING,
            );
            let galley = painter.layout(
                if message.is_empty() { "iOS stopped unexpectedly".into() } else { message },
                FontId::new(13.0, egui::FontFamily::Proportional),
                Color32::from_white_alpha(220),
                screen.width() * 0.8,
            );
            let pos = center + vec2(-galley.size().x / 2.0, 6.0);
            painter.galley(pos, galley, Color32::WHITE);
        }
    }
}

fn dock_bar(app: &mut App, ui: &mut Ui, area: Rect) {
    let p = app.palette;
    let Some(session) = &app.model.session else { return };
    let phase = session.phase.clone();
    let running = matches!(phase, Phase::Running);
    let paused = matches!(phase, Phase::Paused);
    let can_stop = !matches!(phase, Phase::Stopping);
    let show_inspect = app.model.settings.inspect_button;
    let count = if show_inspect { 7.0 } else { 6.0 };
    let width = count * 72.0 + 32.0;
    let dock = Rect::from_center_size(area.center() - vec2(0.0, 6.0), vec2(width, 70.0));
    ui.scope_builder(egui::UiBuilder::new().max_rect(dock), |ui| {
        Frame::new()
            .fill(p.container)
            .corner_radius(CornerRadius::same(theme::radius::DOCK))
            .inner_margin(Margin::symmetric(16, 8))
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    let (icon, label) = if paused { (icons::PLAY, "Resume") } else { (icons::PAUSE, "Pause") };
                    if widgets::dock_button(ui, &p, icon, label, running || paused, None).clicked() {
                        app.model.set_paused(!paused);
                    }
                    let muted = app.model.settings.muted;
                    let (icon, label) = if muted { (icons::VOLUME_DOWN, "Unmute") } else { (icons::VOLUME_UP, "Mute") };
                    if widgets::dock_button(ui, &p, icon, label, true, None).clicked() {
                        app.model.set_muted(!muted);
                    }
                    if widgets::dock_button(ui, &p, icons::FULLSCREEN, "Full screen", true, None).clicked() {
                        let ctx = ui.ctx().clone();
                        app.set_fullscreen(&ctx, true);
                    }
                    let rotation = app.model.settings.rotation;
                    let rotate = widgets::dock_button(ui, &p, icons::ROTATE, "Rotate", true, None)
                        .on_hover_text(format!("{} (turns to {})", rotation.label(), rotation.next().label()));
                    if rotate.clicked() {
                        app.model.settings.rotation = rotation.next();
                        app.model.save_settings();
                    }
                    if widgets::dock_button(ui, &p, icons::CAMERA, "Screenshot", running || paused, None).clicked() {
                        app.model.screenshot();
                    }
                    if show_inspect
                        && widgets::dock_button(ui, &p, icons::CHART, "Inspect", running || paused, None).clicked()
                    {
                        if let Some((snapshot, paused)) = app.model.inspect() {
                            app.inspector = Some(super::inspector::InspectorState::new(snapshot, paused));
                            app.page = Page::Inspector;
                        }
                    }
                    if widgets::dock_button(ui, &p, icons::POWER, "Stop", can_stop, Some(theme::DANGER)).clicked() {
                        app.dialog = Some(Dialog::Stop);
                    }
                });
            });
    });
}
