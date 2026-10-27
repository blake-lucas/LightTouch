//! The frame inspector (the Inspect button, Settings > Diagnostics), like the Android app's: the guest is paused and
//! this shows the frames that led up to it. Drag across the overview to pick a range, click a bar in the detail
//! chart to read one frame, then save the range as a report.
use super::{App, Page, icons, settings::open_path, theme, widgets};
use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, Pos2, Rect, RichText, Sense, Stroke, Ui, vec2};
use lighttouch_model::inspector::{Detail, Frame, RangeStats, SlowEvent, Snapshot, load_detail};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};

/// The detail chart's segment colours: GL, lock waits, other guest work, everything else.
const OTHER: Color32 = Color32::from_rgb(0x5A, 0x63, 0x66);

pub struct InspectorState {
    snapshot: Snapshot,
    /// The inspector paused the guest, so closing it resumes.
    resume_on_close: bool,
    range: (u64, u64),
    picked: Option<Frame>,
    drag_anchor: Option<u64>,
    saved: Option<PathBuf>,
    /// Detailed timing being read in the background.
    loading: Option<Receiver<Detail>>,
    all_events: bool,
}

impl InspectorState {
    pub fn new(snapshot: Snapshot, resume_on_close: bool) -> Self {
        let range = snapshot.initial_range();
        let picked = worst(&snapshot.frames_in(range.0, range.1));
        // The capture was just asked to stop; give the engine a moment to flush it before reading.
        let loading = snapshot.events.clone().map(|path| {
            let (send, receive) = mpsc::channel();
            let from = snapshot.snapshot_ns.saturating_sub(120_000_000_000);
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(600));
                let _ = send.send(load_detail(&path, from));
            });
            receive
        });
        Self { snapshot, resume_on_close, range, picked, drag_anchor: None, saved: None, loading, all_events: false }
    }

    fn select(&mut self, range: (u64, u64)) {
        self.range = range;
        self.picked = worst(&self.snapshot.frames_in(range.0, range.1));
    }
}

fn worst(frames: &[Frame]) -> Option<Frame> {
    frames.iter().copied().max_by(|a, b| a.interval_ms.total_cmp(&b.interval_ms))
}

fn frame_color(ms: f32) -> Color32 {
    if ms > 50.0 {
        theme::DANGER
    } else if ms > 25.0 {
        theme::WARNING
    } else {
        theme::ACCENT
    }
}

fn seconds(ns: u64) -> String {
    format!("{:.1} s", ns as f64 / 1e9)
}

pub fn show(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    let Some(state) = app.inspector.as_mut() else {
        app.page = Page::Device;
        return;
    };
    if let Some(detail) = state.loading.as_ref().and_then(|r| r.try_recv().ok()) {
        state.snapshot.detail = Some(detail);
        state.loading = None;
    }
    if state.loading.is_some() {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
    }
    let mut close = false;
    let mut save = false;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.add_space(18.0);
        let width = (ui.available_width() - 48.0).clamp(200.0, 760.0);
        let margin = ((ui.available_width() - width) / 2.0).max(24.0);
        egui::Frame::new().inner_margin(egui::Margin::symmetric(margin as i8, 0)).show(ui, |ui| {
            ui.set_max_width(width);
            ui.horizontal(|ui| {
                close = widgets::icon_button(ui, &p, icons::BACK, "Close and resume").clicked();
                ui.vertical(|ui| {
                    ui.label(RichText::new("Frame inspector").font(theme::headline()));
                    ui.label(widgets::muted(&p, format!("{} · Guest paused", state.snapshot.device)));
                });
            });
            ui.add_space(12.0);
            if state.snapshot.frames.len() < 2 {
                ui.label(RichText::new("No frames yet").font(theme::title()));
                ui.label(widgets::muted(
                    &p,
                    "The guest has not drawn enough frames to inspect. Resume, play for a few seconds and try again.",
                ));
                return;
            }
            body(ui, &p, state, &mut save);
        });
    });
    if save {
        let (from, to) = state.range;
        if let Some(folder) = app.model.save_frame_report(&state.snapshot, from, to) {
            state.saved = Some(folder);
        }
    }
    if close {
        if state.resume_on_close {
            app.model.set_paused(false);
        }
        app.inspector = None;
        app.page = Page::Device;
    }
}

fn body(ui: &mut Ui, p: &theme::Palette, state: &mut InspectorState, save: &mut bool) {
    let (start, end) = (state.snapshot.start_ns(), state.snapshot.end_ns());
    let label = |ui: &mut Ui, text: String| {
        ui.label(RichText::new(text).font(theme::label()).color(p.on_surface_variant));
    };
    label(ui, format!("Last {:.0} s · drag to select a range, click to jump", (end - start) as f64 / 1e9));
    ui.add_space(4.0);
    overview(ui, p, state);
    ui.horizontal(|ui| {
        let (from, to) = state.range;
        ui.label(widgets::muted(p, seconds(from - start)).size(11.0));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(widgets::muted(p, seconds(to - start)).size(11.0));
            ui.add_space(ui.available_width() / 2.0 - 40.0);
            ui.label(RichText::new(format!("{:.2} s selected", (to - from) as f64 / 1e9)).size(11.0));
        });
    });
    ui.add_space(14.0);
    let selected = state.snapshot.frames_in(state.range.0, state.range.1);
    detail(ui, p, &selected, state.snapshot.detail.as_ref(), &mut state.picked);
    if state.snapshot.detail.is_some() {
        legend(ui, p);
    }
    ui.add_space(14.0);
    let stats = RangeStats::of(&selected);
    ui.columns(4, |columns| {
        stat(&mut columns[0], p, "Avg FPS", format!("{:.1}", stats.avg_fps), None);
        stat(&mut columns[1], p, "1% low", format!("{:.1}", stats.low_fps), None);
        stat(&mut columns[2], p, "Worst", format!("{:.0} ms", stats.worst_ms), None);
        let color = (stats.hitches > 0).then_some(theme::DANGER);
        stat(&mut columns[3], p, "Hitches", stats.hitches.to_string(), color);
    });
    ui.add_space(14.0);
    label(ui, format!("Frame times · {} frames", stats.frames));
    histogram(ui, p, &stats);
    if let Some(frame) = state.picked {
        ui.add_space(14.0);
        frame_card(ui, p, &state.snapshot, frame);
    }
    ui.add_space(14.0);
    match (&state.snapshot.detail, state.loading.is_some()) {
        (Some(detail), _) => {
            label(ui, "Slow engine events in range".to_string());
            let mut slow = detail.events_in(state.range.0, state.range.1);
            slow.sort_by(|a, b| b.ms().total_cmp(&a.ms()));
            slow.truncate(30);
            if slow.is_empty() {
                ui.label(widgets::muted(p, "None over 0.5 ms.").size(12.0));
            }
            let shown = if state.all_events { slow.len() } else { slow.len().min(8) };
            for event in &slow[..shown] {
                event_row(ui, p, event, start);
            }
            if slow.len() > 8 {
                let text = if state.all_events { "Show fewer".to_string() } else { format!("Show all {}", slow.len()) };
                if widgets::text_button(ui, p, &text).clicked() {
                    state.all_events = !state.all_events;
                }
            }
        }
        (None, true) => {
            ui.label(widgets::muted(p, "Reading detailed timing…").size(12.0));
        }
        (None, false) => {
            egui::Frame::new().fill(p.container_high).corner_radius(CornerRadius::same(12)).inner_margin(egui::Margin::same(14)).show(
                ui,
                |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal_wrapped(|ui| {
                        ui.label(widgets::icon_text(icons::INFO, 18.0).color(p.primary));
                        ui.label(RichText::new(
                            "Turn on Detailed frame timing in Settings › Diagnostics to see where each frame's time went: guest CPU, GL and lock waits. It costs a few percent of speed while on.",
                        ).size(12.5));
                    });
                },
            );
        }
    }
    ui.add_space(16.0);
    ui.horizontal(|ui| {
        if widgets::primary(ui, p, "Save report").clicked() {
            *save = true;
        }
        if let Some(folder) = &state.saved {
            ui.add_space(8.0);
            if widgets::text_button(ui, p, "Open folder").clicked() {
                open_path(folder);
            }
        }
    });
    if let Some(folder) = &state.saved {
        ui.label(widgets::muted(p, format!("Saved to {}", folder.display())).size(12.0));
    }
    ui.add_space(28.0);
}

/// Every frame of the snapshot as a thin bar; the selection is the lit band.
fn overview(ui: &mut Ui, p: &theme::Palette, state: &mut InspectorState) {
    let (start, end) = (state.snapshot.start_ns(), state.snapshot.end_ns());
    let span = (end - start).max(1);
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 72.0), Sense::click_and_drag());
    let at = |x: f32| start + (((x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64 * span as f64) as u64;
    if response.drag_started() {
        state.drag_anchor = response.interact_pointer_pos().map(|pos| at(pos.x));
    }
    if let (Some(anchor), Some(pos)) = (state.drag_anchor, response.interact_pointer_pos()) {
        let now = at(pos.x);
        if response.dragged() && now.abs_diff(anchor) > 50_000_000 {
            state.select((anchor.min(now), anchor.max(now)));
        }
    }
    if response.drag_stopped() {
        state.drag_anchor = None;
    }
    if response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            let t = at(pos.x);
            state.select((t.saturating_sub(1_000_000_000).max(start), (t + 1_000_000_000).min(end)));
        }
    }
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(8), p.container_high);
    let x = |t: u64| rect.left() + (t - start) as f32 / span as f32 * rect.width();
    let bottom = rect.bottom() - 4.0;
    let (from, to) = state.range;
    for f in state.snapshot.frames.iter().filter(|f| !f.idle()) {
        let h = f.interval_ms.min(100.0) / 100.0 * (rect.height() - 8.0);
        let lit = (from..=to).contains(&f.published_ns);
        let color = frame_color(f.interval_ms).gamma_multiply(if lit { 1.0 } else { 0.45 });
        let fx = x(f.published_ns);
        painter.line_segment([Pos2::new(fx, bottom), Pos2::new(fx, bottom - h)], Stroke::new(1.5, color));
    }
    let band = Rect::from_x_y_ranges(x(from)..=x(to), rect.y_range());
    painter.rect_filled(band, 0.0, p.primary.gamma_multiply(0.16));
    for edge in [band.left(), band.right()] {
        painter
            .line_segment([Pos2::new(edge, rect.top()), Pos2::new(edge, rect.bottom())], Stroke::new(2.0, p.primary));
    }
}

/// The selected frames as bars sized by frame time, guides at 16.7/33.3/50 ms; click one to read it.
fn detail(ui: &mut Ui, p: &theme::Palette, frames: &[Frame], timing: Option<&Detail>, picked: &mut Option<Frame>) {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 180.0), Sense::click());
    let top = (frames.iter().map(|f| f.interval_ms).fold(0.0, f32::max) * 1.1).clamp(40.0, 250.0);
    let y = |ms: f32| rect.bottom() - ms.min(top) / top * rect.height();
    if response.clicked() && !frames.is_empty() {
        if let Some(pos) = response.interact_pointer_pos() {
            let i = ((pos.x - rect.left()) / rect.width() * frames.len() as f32) as usize;
            *picked = Some(frames[i.min(frames.len() - 1)]);
        }
    }
    let painter = ui.painter_at(rect);
    // Guides that would sit closer than a label's height to the one below are left out.
    let mut guides: Vec<f32> = Vec::new();
    for g in [16.7, 33.3, 50.0, 100.0, 200.0] {
        if g < top && guides.last().is_none_or(|last| y(*last) - y(g) >= 14.0) {
            guides.push(g);
        }
    }
    for g in &guides {
        painter.line_segment(
            [Pos2::new(rect.left(), y(*g)), Pos2::new(rect.right(), y(*g))],
            Stroke::new(1.0, p.outline_variant),
        );
    }
    if !frames.is_empty() {
        let w = rect.width() / frames.len() as f32;
        let gap = if w > 4.0 { 1.0 } else { 0.0 };
        for (i, f) in frames.iter().enumerate() {
            let left = rect.left() + i as f32 * w;
            let bar = Rect::from_min_max(Pos2::new(left, y(f.interval_ms)), Pos2::new(left + w - gap, rect.bottom()));
            match timing.and_then(|d| d.breakdown_for(f)) {
                // Stacked from the bottom: GL, lock waits, other guest work, then everything else.
                Some(b) => {
                    let mut base = rect.bottom();
                    for (ms, color) in [
                        (b.gl_ms, theme::GPU),
                        (b.bql_ms, theme::WARNING),
                        ((b.run_ms - b.gl_ms).max(0.0), theme::CPU),
                        (f.interval_ms, OTHER),
                    ] {
                        let top_y = (base - ms.max(0.0) / top * rect.height()).max(bar.top());
                        if base > top_y {
                            painter.rect_filled(Rect::from_x_y_ranges(bar.x_range(), top_y..=base), 0.0, color);
                        }
                        base = top_y;
                    }
                }
                None => {
                    painter.rect_filled(bar, 0.0, frame_color(f.interval_ms));
                }
            }
            if Some(*f) == *picked {
                let mark = Rect::from_min_size(Pos2::new(left, bar.top() - 5.0), vec2((w - gap).max(2.0), 2.0));
                painter.rect_filled(mark, 0.0, p.on_surface);
            }
        }
    }
    for g in &guides {
        let text = format!("{g:.0} ms");
        painter.text(
            Pos2::new(rect.right() - 4.0, y(*g) - 2.0),
            Align2::RIGHT_BOTTOM,
            text,
            FontId::proportional(10.0),
            p.on_surface_variant,
        );
    }
}

fn stat(ui: &mut Ui, p: &theme::Palette, label: &str, value: String, color: Option<Color32>) {
    egui::Frame::new()
        .fill(p.container_high)
        .corner_radius(CornerRadius::same(12))
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(value).font(theme::title()).color(color.unwrap_or(p.on_surface)));
            ui.label(widgets::muted(p, label).size(11.0));
        });
}

fn histogram(ui: &mut Ui, p: &theme::Palette, stats: &RangeStats) {
    let most = stats.buckets.iter().copied().max().unwrap_or(0).max(1);
    let colors =
        [theme::ACCENT, theme::ACCENT, theme::ACCENT, theme::WARNING, theme::WARNING, theme::DANGER, theme::DANGER];
    for (i, label) in RangeStats::BUCKET_LABELS.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.add_sized([72.0, 16.0], egui::Label::new(widgets::muted(p, *label).size(11.0)));
            let width = ui.available_width() - 44.0 - ui.spacing().item_spacing.x;
            let (rect, _) = ui.allocate_exact_size(vec2(width.max(0.0), 14.0), Sense::hover());
            let filled = Rect::from_min_size(
                rect.min,
                vec2(rect.width() * stats.buckets[i] as f32 / most as f32, rect.height()),
            );
            ui.painter().rect_filled(filled, CornerRadius::same(3), colors[i].gamma_multiply(0.85));
            ui.add_sized([44.0, 16.0], egui::Label::new(RichText::new(stats.buckets[i].to_string()).size(11.0)));
        });
    }
}

fn frame_card(ui: &mut Ui, p: &theme::Palette, snapshot: &Snapshot, frame: Frame) {
    widgets::card(ui, p, |ui| {
        ui.horizontal(|ui| {
            widgets::status_dot(ui, frame_color(frame.interval_ms), 10.0);
            ui.label(RichText::new(format!("Frame {}", frame.sequence)).font(theme::title()));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let text = RichText::new(format!("{:.1} ms", frame.interval_ms)).font(theme::title());
                ui.label(text.color(frame_color(frame.interval_ms)));
            });
        });
        ui.label(widgets::muted(p, format!("at {}", seconds(frame.published_ns - snapshot.start_ns()))).size(11.0));
        ui.add_space(6.0);
        let shown = if frame.presented_ns > 0 {
            format!("{:.2} ms", frame.presented_ns.saturating_sub(frame.published_ns) as f64 / 1e6)
        } else {
            "skipped (a newer frame replaced it)".to_string()
        };
        let line = |ui: &mut Ui, name: &str, value: String| {
            ui.horizontal(|ui| {
                ui.label(widgets::muted(p, name));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(value).monospace());
                });
            });
        };
        line(ui, "Shown in the window after", shown);
        let Some(detail) = &snapshot.detail else { return };
        let ms = |v: f32| format!("{v:.2} ms");
        if let Some(b) = detail.breakdown_for(&frame) {
            line(ui, "Guest code (vCPU running)", ms(b.run_ms));
            line(ui, "  of which GL calls", ms(b.gl_ms));
            line(ui, "  of which GL present", ms(b.gl_present_ms));
            line(ui, "vCPU on a host core", ms(b.vcpu_cpu_ms));
            line(ui, "vCPU halted or outside", ms(b.outside_ms));
            line(ui, "Lock (BQL) waits", ms(b.bql_ms));
            line(ui, "Main loop CPU", ms(b.main_cpu_ms));
            let i = frame.interval_ms;
            let hint = if b.run_ms > i * 0.8 && b.vcpu_cpu_ms < b.run_ms * 0.75 {
                Some("The vCPU was runnable but often off-CPU: the computer scheduled other work or slowed the core.")
            } else if b.gl_ms > i * 0.4 {
                Some("GL calls took most of this frame: host driver or readback cost.")
            } else if b.bql_ms > i * 0.25 {
                Some("The vCPU spent much of this frame waiting for the engine lock.")
            } else if b.run_ms > i * 0.8 {
                Some("The guest was busy for the whole frame: more guest work than usual.")
            } else {
                None
            };
            if let Some(hint) = hint {
                ui.add_space(6.0);
                ui.label(RichText::new(hint).size(12.5).color(p.primary));
            }
        }
        let from = frame.published_ns.saturating_sub((frame.interval_ms * 1e6) as u64);
        let mut overlapping = detail.events_in(from, frame.published_ns);
        overlapping.sort_by(|a, b| b.ms().total_cmp(&a.ms()));
        if !overlapping.is_empty() {
            ui.add_space(6.0);
            ui.label(RichText::new("Slow events in this frame").font(theme::label()));
            for event in overlapping.into_iter().take(6) {
                event_row(ui, p, event, snapshot.start_ns());
            }
        }
    });
}

fn legend(ui: &mut Ui, p: &theme::Palette) {
    ui.horizontal(|ui| {
        for (color, name) in
            [(theme::GPU, "GL"), (theme::WARNING, "Lock wait"), (theme::CPU, "Guest CPU"), (OTHER, "Other")]
        {
            widgets::status_dot(ui, color, 8.0);
            ui.label(widgets::muted(p, name).size(11.0));
            ui.add_space(8.0);
        }
    });
}

fn event_row(ui: &mut Ui, p: &theme::Palette, event: &SlowEvent, start: u64) {
    ui.horizontal(|ui| {
        let color = if event.ms() > 8.0 { theme::DANGER } else { p.on_surface };
        ui.add_sized(
            [72.0, 18.0],
            egui::Label::new(RichText::new(format!("{:6.2} ms", event.ms())).monospace().color(color)),
        );
        ui.vertical(|ui| {
            ui.label(RichText::new(&event.label).size(12.5));
            let when = format!("{} · {}", event.kind.label(), seconds(event.start_ns.saturating_sub(start)));
            ui.label(widgets::muted(p, when).size(11.0));
        });
    });
}
