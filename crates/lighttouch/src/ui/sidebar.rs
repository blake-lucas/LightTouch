//! The device list on the left: every saved device, the running one marked, and "Add a device".
use super::{App, Dialog, Page, icons, theme, widgets};
use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, Frame, Margin, RichText, Sense, vec2};
use lighttouch_model::session::Phase;

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let p = app.palette;
    egui::Panel::left("devices")
        .resizable(false)
        .exact_size(268.0)
        .frame(Frame::new().fill(p.container_low).inner_margin(Margin { left: 14, right: 14, top: 18, bottom: 14 }))
        .show(ui, |ui| {
            // Brand.
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(vec2(34.0, 34.0), Sense::hover());
                ui.painter().rect_filled(rect, CornerRadius::same(10), p.primary);
                ui.painter().text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    icons::PHONE,
                    FontId::new(20.0, theme::icons_fill()),
                    p.on_primary,
                );
                ui.add_space(4.0);
                ui.label(RichText::new("LightTouch").size(18.0).family(theme::semibold()));
            });
            ui.add_space(18.0);
            ui.label(RichText::new("DEVICES").font(theme::label()).color(p.on_surface_variant));
            ui.add_space(4.0);

            let active = app.model.session_active();
            let busy = app.model.busy().is_some();
            // A preparation runs on its own: the devices stay usable meanwhile (an export, import or move has its own
            // dialog in front instead).
            let preparing = matches!(app.model.busy(), Some((lighttouch_model::Activity::Preparing(_), _)));
            // Every running device has its dot: the one on screen and those left running in the background.
            let running_devices = app.model.running();
            // The Prepare page is the device being made (marked on its own row), not the selected one.
            let preparing_shown = app.page == Page::Prepare;
            egui::ScrollArea::vertical().auto_shrink([false, true]).max_height(ui.available_height() - 168.0).show(
                ui,
                |ui| {
                    // Each row's place, for where a dragged one lands, and the dragged one (with what it shows).
                    let mut rows: Vec<egui::Rect> = Vec::new();
                    let mut dragged: Option<DraggedRow> = None;
                    let making_room = making_room(ui, &app.model.devices);
                    for (index, device) in app.model.devices.clone().into_iter().enumerate() {
                        let phase = running_devices.iter().find(|(id, _)| *id == device.id).map(|(_, p)| p);
                        let running = phase.is_some();
                        let badge = match phase {
                            Some(phase) => Some(Badge::Dot(phase_color(phase))),
                            None if device.dirty => Some(Badge::Warning),
                            None => None,
                        };
                        let selected = device.selected && !preparing_shown;
                        let row_id = egui::Id::unique(("device-row", &device.id));
                        let shift = making_room.as_ref().map_or(0.0, |m| m.shift(ui, row_id, index));
                        let lifted = making_room.as_ref().is_some_and(|m| m.from == index);
                        // One bubble for everything the row has to say (egui stacks each hover text as its own).
                        let mut notes = Vec::new();
                        if badge == Some(Badge::Warning) {
                            notes.push(
                                "Needs recovery: it didn't shut down cleanly. The next start checks its storage first.",
                            );
                        }
                        if active && !device.selected && !running {
                            notes.push("The device on screen keeps running in the background");
                        }
                        let response = device_row(
                            ui,
                            &p,
                            &device.name,
                            &device.subtitle,
                            selected,
                            badge,
                            Some(row_id),
                            shift,
                            lifted,
                            &notes,
                        );
                        rows.push(response.rect);
                        if response.drag_started_by(egui::PointerButton::Primary) {
                            let grab = response.interact_pointer_pos().map_or(0.0, |at| at.y - response.rect.top());
                            let landing =
                                Landing { id: device.id.clone(), gap: index, grab, serial: ui.input(|i| i.time) };
                            ui.ctx().data_mut(|d| d.insert_temp(landing_id(), landing));
                        }
                        let stopped = response.drag_stopped_by(egui::PointerButton::Primary);
                        if stopped || response.dragged_by(egui::PointerButton::Primary) {
                            dragged = Some(DraggedRow {
                                id: device.id.clone(),
                                rect: response.rect,
                                name: device.name.clone(),
                                subtitle: device.subtitle.clone(),
                                selected,
                                badge,
                                dropped: stopped,
                            });
                        }
                        if response.clicked() && (!busy || preparing) && !device.selected {
                            app.model.select(&device.id);
                            app.page = Page::Device;
                        } else if response.clicked() && device.selected && (!busy || preparing) {
                            app.page = Page::Device;
                        }
                        response.context_menu(|ui| {
                            if ui.button(widgets::icon_label(icons::INFO, "Details", None)).clicked() {
                                ui.close();
                                super::details::open(app, &device);
                            }
                            if ui.button(widgets::icon_label(icons::PENCIL, "Rename…", None)).clicked() {
                                app.dialog = Some(Dialog::Rename { id: device.id.clone(), name: device.name.clone() });
                                ui.close();
                            }
                            let export = ui
                                .add_enabled(
                                    !running && !busy,
                                    egui::Button::new(widgets::icon_label(icons::UPLOAD, "Export…", None)),
                                )
                                .on_disabled_hover_text(if running {
                                    "Stop it to export it"
                                } else {
                                    "Wait for the current task to finish"
                                });
                            if export.clicked() {
                                ui.close();
                                super::export_device(app, &device.id, &device.name);
                            }
                            if ui
                                .add_enabled(
                                    !running,
                                    egui::Button::new(widgets::icon_label(icons::TRASH, "Delete…", None)),
                                )
                                .clicked()
                            {
                                app.dialog = Some(Dialog::Delete { id: device.id.clone(), name: device.name.clone() });
                                ui.close();
                            }
                        });
                        ui.add_space(ROW_GAP);
                    }
                    if let Some(row) = dragged {
                        drag_feedback(app, ui, &rows, row);
                    }
                    pending_row(app, ui);
                },
            );

            ui.add_space(6.0);
            // One device is made at a time, while others run; the one being made is listed above, with its progress. Its ⋮
            // holds the other way in: a device exported by LightTouch on another computer or phone.
            let mut import = false;
            let add = ui
                .horizontal(|ui| {
                    let add = add_row(ui, &p, icons::PLUS, "Add a device", !busy, ui.available_width() - 40.0);
                    let more =
                        widgets::icon_button_sized(ui, &p, icons::MORE, "More ways to add a device", 34.0, !busy);
                    egui::Popup::menu(&more).show(|ui| {
                        ui.set_min_width(200.0);
                        if ui
                            .button(widgets::icon_label(icons::DOWNLOAD, "Import a device…", None))
                            .on_hover_text("Add a device exported by LightTouch on another computer or phone")
                            .clicked()
                        {
                            import = true;
                        }
                    });
                    add
                })
                .inner;
            let add = if busy { add.on_hover_text("Wait for the device being prepared to finish") } else { add };
            if add.clicked() && !busy {
                super::prepare::open(app);
            }
            if import && !busy {
                super::import_device(app);
            }

            ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                ui.horizontal(|ui| {
                    let settings_open = matches!(
                        app.page,
                        Page::Settings | Page::Licenses | Page::Network | Page::Carrier | Page::ScreenScaling
                    );
                    if nav_button(ui, &p, icons::GEAR, "Settings", settings_open).clicked() {
                        app.page = if settings_open { Page::Device } else { Page::Settings };
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if help_button(ui, &p).clicked() {
                            app.dialog = Some(Dialog::Controls);
                        }
                    });
                });
            });
        });
}

/// The device being prepared, listed while that runs: its progress, and a way back to it.
fn pending_row(app: &mut App, ui: &mut egui::Ui) {
    let p = app.palette;
    let Some((lighttouch_model::Activity::Preparing(id), progress)) = app.model.busy() else { return };
    let firmware = lighttouch_catalog::catalog::Catalog::get().find(id);
    let name = Some(app.prepare.name.trim().to_string()).filter(|n| !n.is_empty()).unwrap_or_else(|| {
        firmware.map(|f| format!("{} {}", f.device_name(), f.label())).unwrap_or_else(|| "New device".into())
    });
    let page = Page::Prepare;
    let fraction = progress.fraction();
    // The stage itself is on the progress page; one line here.
    let status = match fraction {
        Some(f) => format!("Preparing · {:.0}%", f * 100.0),
        None => "Preparing…".to_string(),
    };
    let showing = app.page == page;
    let response = device_row(ui, &p, &name, &status, showing, Some(Badge::Dot(theme::WARNING)), None, 0.0, false, &[]);
    // A thin bar along the row's foot.
    let bar = egui::Rect::from_min_size(
        response.rect.left_bottom() + vec2(54.0, -6.0),
        vec2(response.rect.width() - 62.0, 3.0),
    );
    ui.painter().rect_filled(bar, CornerRadius::same(2), p.container_highest);
    match fraction {
        Some(f) => {
            let done = egui::Rect::from_min_size(bar.min, vec2(bar.width() * f.clamp(0.0, 1.0), bar.height()));
            ui.painter().rect_filled(done, CornerRadius::same(2), p.primary);
        }
        None => {
            // No total yet: a segment sweeping across.
            let t = ui.input(|i| i.time) as f32;
            let x = (t * 0.8).fract() * (bar.width() + 40.0) - 40.0;
            let segment = egui::Rect::from_min_max(
                egui::pos2(bar.left() + x.max(0.0), bar.top()),
                egui::pos2((bar.left() + x + 40.0).min(bar.right()), bar.bottom()),
            );
            ui.painter().rect_filled(segment, CornerRadius::same(2), p.primary);
            ui.ctx().request_repaint();
        }
    }
    if response.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
        app.page = page;
    }
    ui.add_space(2.0);
}

/// The row being dragged to a new place in the list.
struct DraggedRow {
    id: String,
    rect: egui::Rect,
    name: String,
    subtitle: String,
    selected: bool,
    badge: Option<Badge>,
    /// Let go this frame: move it.
    dropped: bool,
}

/// The dragged row and the gap it would drop into now, kept between frames.
#[derive(Clone)]
struct Landing {
    id: String,
    gap: usize,
    /// Where it was grabbed, from its top.
    grab: f32,
    /// When the drag began: each drag animates the rows making room afresh.
    serial: f64,
}

fn landing_id() -> egui::Id {
    egui::Id::unique("device-row-landing")
}

/// While a row is dragged, the rows between its place and where it would land move a row's height to make room.
struct MakingRoom {
    from: usize,
    to: usize,
    serial: f64,
}

fn making_room(ui: &egui::Ui, devices: &[lighttouch_model::Device]) -> Option<MakingRoom> {
    let landing = ui.ctx().data(|d| d.get_temp::<Landing>(landing_id()))?;
    // Dropped this frame: `drag_feedback` moves it (until then it stays lifted, the others making room). A drag that
    // ended any other way (the window lost the pointer) leaves nothing moved.
    let dragged = ui.ctx().is_being_dragged(egui::Id::unique(("device-row", &landing.id)));
    if !dragged && !ui.input(|i| i.pointer.primary_released()) {
        ui.ctx().data_mut(|d| d.remove::<Landing>(landing_id()));
        return None;
    }
    let from = devices.iter().position(|d| d.id == landing.id)?;
    let to = lighttouch_model::device_order::destination(from, landing.gap, devices.len()).unwrap_or(from);
    Some(MakingRoom { from, to, serial: landing.serial })
}

impl MakingRoom {
    /// How far the row at `index` is drawn from its place, eased.
    fn shift(&self, ui: &egui::Ui, row: egui::Id, index: usize) -> f32 {
        let pitch = ROW_HEIGHT + ROW_GAP;
        let target = if self.from < self.to && index > self.from && index <= self.to {
            -pitch
        } else if self.to < self.from && index >= self.to && index < self.from {
            pitch
        } else {
            0.0
        };
        ui.ctx().animate_value_with_time(row.with(("shift", self.serial.to_bits(), index)), target, 0.12)
    }
}

/// While a row is dragged: a copy of it under the pointer (the others make room, `MakingRoom`); once let go, the
/// move. The list scrolls when the pointer nears its top or bottom edge.
fn drag_feedback(app: &mut App, ui: &mut egui::Ui, rows: &[egui::Rect], row: DraggedRow) {
    use lighttouch_model::device_order::gap_at;
    let Some(pointer) = ui.ctx().pointer_latest_pos() else { return };
    let centers: Vec<f32> = rows.iter().map(|r| r.center().y).collect();
    let gap = gap_at(&centers, pointer.y);
    if row.dropped {
        ui.ctx().data_mut(|d| d.remove::<Landing>(landing_id()));
        app.model.move_device(&row.id, gap);
        return;
    }
    let Some(mut landing) = ui.ctx().data(|d| d.get_temp::<Landing>(landing_id())) else { return };
    landing.gap = gap;
    let grab = landing.grab;
    ui.ctx().data_mut(|d| d.insert_temp(landing_id(), landing));
    let p = app.palette;
    let clip = ui.clip_rect();
    let half = row.rect.height() / 2.0;
    let top = (pointer.y - grab).clamp(clip.top() - half, clip.bottom() - half);
    let floating = egui::Rect::from_min_size(egui::pos2(row.rect.left(), top), row.rect.size());
    let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::unique("device-row-drag")));
    let radius = CornerRadius::same(theme::radius::ROW);
    painter.add(
        egui::epaint::Shadow { offset: [0, 3], blur: 12, spread: 0, color: Color32::from_black_alpha(60) }
            .as_shape(floating, radius),
    );
    painter.rect_filled(floating, radius, p.container_high);
    paint_row(&painter, &p, floating, &row.name, &row.subtitle, row.selected, row.badge, true);
    // Near an edge of a list taller than its space: scroll toward it.
    let edge = 24.0;
    if pointer.y < clip.top() + edge {
        ui.scroll_with_delta(vec2(0.0, 6.0));
    } else if pointer.y > clip.bottom() - edge {
        ui.scroll_with_delta(vec2(0.0, -6.0));
    }
    ui.ctx().request_repaint();
}

pub fn phase_color(phase: &Phase) -> Color32 {
    match phase {
        Phase::Running => theme::ACCENT,
        Phase::Verifying { .. } | Phase::Starting | Phase::Stopping => theme::WARNING,
        Phase::Paused => theme::PAUSED,
        Phase::Ended { clean: false, .. } => theme::DANGER,
        Phase::Ended { .. } => Color32::GRAY,
    }
}

/// What the corner of a device's tile shows: its run state, or that it needs recovery.
#[derive(Clone, Copy, PartialEq)]
enum Badge {
    Dot(Color32),
    Warning,
}

/// A device's row, drawn `shift` points below its place. With an `id` it can be dragged to reorder the list
/// (`drag_feedback`); `lifted` while it is. Hovered, it shows its name when that was cut short and `notes`, in one bubble.
fn device_row(
    ui: &mut egui::Ui,
    p: &theme::Palette,
    name: &str,
    subtitle: &str,
    selected: bool,
    badge: Option<Badge>,
    id: Option<egui::Id>,
    shift: f32,
    lifted: bool,
    notes: &[&str],
) -> egui::Response {
    let (rect, response) = match id {
        Some(id) => {
            let rect = ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), Sense::hover()).0;
            (rect, ui.interact(rect, id, Sense::click_and_drag()))
        }
        None => ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), Sense::click()),
    };
    // A lifted row is drawn under the pointer instead (`drag_feedback`), leaving its place empty.
    let cut = !lifted
        && paint_row(
            ui.painter(),
            p,
            rect.translate(vec2(0.0, shift)),
            name,
            subtitle,
            selected,
            badge,
            response.hovered() && !lifted && shift == 0.0,
        );
    let response =
        response.on_hover_cursor(if lifted { egui::CursorIcon::Grabbing } else { egui::CursorIcon::PointingHand });
    let hover: Vec<&str> = cut.then_some(name).into_iter().chain(notes.iter().copied()).collect();
    if hover.is_empty() || lifted { response } else { response.on_hover_text(hover.join("\n")) }
}

const ROW_HEIGHT: f32 = 58.0;
/// The space under each device row (`ui.add_space` in `show`).
const ROW_GAP: f32 = 2.0;

/// Draw a device row into `rect`. Whether the name was cut short.
#[allow(clippy::too_many_arguments)]
fn paint_row(
    painter: &egui::Painter,
    p: &theme::Palette,
    rect: egui::Rect,
    name: &str,
    subtitle: &str,
    selected: bool,
    badge: Option<Badge>,
    hovered: bool,
) -> bool {
    let fill = if selected {
        p.container_highest
    } else if hovered {
        p.container_high
    } else {
        Color32::TRANSPARENT
    };
    painter.rect_filled(rect, CornerRadius::same(theme::radius::ROW), fill);
    let tile = egui::Rect::from_center_size(rect.left_center() + vec2(26.0, 0.0), vec2(34.0, 40.0));
    painter.rect_filled(tile, CornerRadius::same(9), if selected { p.primary_container } else { p.container_highest });
    painter.text(
        tile.center(),
        Align2::CENTER_CENTER,
        icons::PHONE,
        FontId::new(20.0, theme::icons()),
        if selected { p.on_primary_container } else { p.on_surface_variant },
    );
    let corner = tile.right_bottom() - vec2(3.0, 3.0);
    match badge {
        Some(Badge::Dot(color)) => {
            painter.circle_filled(corner, 5.5, fill.max_alpha_with(p.container_low));
            painter.circle_filled(corner, 4.0, color);
        }
        Some(Badge::Warning) => {
            painter.circle_filled(corner, 9.0, fill.max_alpha_with(p.container_low));
            painter.text(
                corner,
                Align2::CENTER_CENTER,
                icons::WARNING,
                FontId::new(15.0, theme::icons_fill()),
                p.error,
            );
        }
        None => {}
    }
    let left = rect.left() + 54.0;
    let width = rect.right() - left - 8.0;
    // One line each, cut with an ellipsis: a long name wrapped onto the subtitle.
    let line = |text: &str, font: FontId, color: Color32| {
        let mut job = egui::text::LayoutJob::simple_singleline(text.to_string(), font, color);
        job.wrap = egui::text::TextWrapping::truncate_at_width(width);
        painter.layout_job(job)
    };
    let name_galley = line(name, FontId::new(14.5, theme::medium()), p.on_surface);
    let cut = name_galley.elided;
    let sub_galley = line(subtitle, FontId::new(12.0, egui::FontFamily::Proportional), p.on_surface_variant);
    painter.galley(egui::pos2(left, rect.center().y - 17.0), name_galley, p.on_surface);
    painter.galley(egui::pos2(left, rect.center().y + 2.0), sub_galley, p.on_surface_variant);
    cut
}

trait MaxAlpha {
    fn max_alpha_with(self, other: Color32) -> Color32;
}

impl MaxAlpha for Color32 {
    /// The row's fill if it has one, else the panel colour (to cut the dot out of the tile).
    fn max_alpha_with(self, other: Color32) -> Color32 {
        if self.a() == 0 { other } else { self }
    }
}

fn add_row(
    ui: &mut egui::Ui,
    p: &theme::Palette,
    icon: &str,
    label: &str,
    enabled: bool,
    width: f32,
) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(vec2(width, 46.0), if enabled { Sense::click() } else { Sense::hover() });
    let alpha = if enabled { 1.0 } else { 0.4 };
    if response.hovered() && enabled {
        ui.painter().rect_filled(rect, CornerRadius::same(theme::radius::ROW), p.container_high);
    }
    let tile = egui::Rect::from_center_size(rect.left_center() + vec2(26.0, 0.0), vec2(34.0, 34.0));
    ui.painter().rect_stroke(
        tile,
        CornerRadius::same(9),
        egui::Stroke::new(1.2, p.outline.gamma_multiply(alpha)),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        tile.center(),
        Align2::CENTER_CENTER,
        icon,
        FontId::new(18.0, theme::icons()),
        p.primary.gamma_multiply(alpha),
    );
    ui.painter().text(
        egui::pos2(rect.left() + 54.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::new(14.5, theme::medium()),
        p.primary.gamma_multiply(alpha),
    );
    if enabled { response.on_hover_cursor(egui::CursorIcon::PointingHand) } else { response }
}

/// The Controls dialog's button, at the footer's right: the icon alone, on the Settings button's centre line.
fn help_button(ui: &mut egui::Ui, p: &theme::Palette) -> egui::Response {
    let (rect, _) = ui.allocate_exact_size(vec2(36.0, 36.0), Sense::hover());
    // Into the panel's margin, toward the divider.
    let rect = rect.translate(vec2(6.0, 0.0));
    let response = ui.interact(rect, ui.scope_id().with("controls-help"), Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(12), p.container_high);
    }
    // Phosphor's glyphs sit above the centre of their line box; this lines the ink up with the gear's.
    let center = rect.center() + vec2(0.0, HELP_NUDGE);
    ui.painter().text(
        center,
        Align2::CENTER_CENTER,
        icons::QUESTION,
        FontId::new(17.0, theme::icons()),
        p.on_surface_variant,
    );
    response.on_hover_text("Controls").on_hover_cursor(egui::CursorIcon::PointingHand)
}
const HELP_NUDGE: f32 = 1.0;

fn nav_button(ui: &mut egui::Ui, p: &theme::Palette, icon: &str, label: &str, active: bool) -> egui::Response {
    let text = widgets::icon_label(icon, label, Some(if active { p.primary } else { p.on_surface_variant }));
    ui.add(
        egui::Button::new(text)
            .fill(if active { p.container_highest } else { Color32::TRANSPARENT })
            .corner_radius(CornerRadius::same(12))
            .min_size(vec2(0.0, 36.0)),
    )
    .on_hover_cursor(egui::CursorIcon::PointingHand)
}
