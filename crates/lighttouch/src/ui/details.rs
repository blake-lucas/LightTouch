//! A device's Details page (the device row's menu), like the Android app's: its model and iOS, the hardware it stands
//! for, the space it takes on this computer, and LightTouch's own facts. The facts come from
//! `lighttouch_model::details`; the space is added up in the background.
use super::{App, Page, icons, theme, widgets};
use eframe::egui::{self, RichText, Ui};
use lighttouch_model::Device;
use lighttouch_model::details::{self, Facts, Space, format_bytes};
use std::sync::mpsc::{self, Receiver};

pub struct DetailsState {
    device: Device,
    facts: Facts,
    space: Option<Space>,
    measuring: Option<Receiver<Space>>,
}

/// Opens `device`'s Details page.
pub fn open(app: &mut App, device: &Device) {
    let (tx, rx) = mpsc::channel();
    let record = device.record.clone();
    std::thread::spawn(move || {
        let _ = tx.send(details::space(&record));
    });
    app.details = Some(DetailsState {
        device: device.clone(),
        facts: details::facts(&device.record),
        space: None,
        measuring: Some(rx),
    });
    app.page = Page::Details;
}

pub fn show(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    let Some(state) = app.details.as_mut() else {
        app.page = Page::Device;
        return;
    };
    if let Some(space) = state.measuring.as_ref().and_then(|rx| rx.try_recv().ok()) {
        state.space = Some(space);
        state.measuring = None;
    } else if state.measuring.is_some() {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
    }
    let mut back = false;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.add_space(18.0);
        let width = (ui.available_width() - 48.0).clamp(200.0, 640.0);
        let margin = ((ui.available_width() - width) / 2.0).max(24.0);
        egui::Frame::new().inner_margin(egui::Margin::symmetric(margin as i8, 0)).show(ui, |ui| {
            ui.set_max_width(width);
            ui.horizontal(|ui| {
                back = widgets::icon_button(ui, &p, icons::BACK, "Back").clicked();
                ui.label(RichText::new(&state.device.name).font(theme::headline()));
            });
            let (device, f) = (&state.device, &state.facts);

            section(ui, &p, "Device", None, |ui| {
                let model = match &f.model_number {
                    Some(number) => format!("{} ({number})", device.model),
                    None => device.model.clone(),
                };
                fact(ui, &p, "Model", &model);
                fact(ui, &p, "iOS", &f.ios);
                fact(ui, &p, "Board", &device.board);
                for (label, value) in
                    [("Serial number", &f.serial_number), ("UDID", &f.udid), ("Wi-Fi address", &f.wifi_mac)]
                {
                    if let Some(value) = value {
                        fact(ui, &p, label, value);
                    }
                }
            });

            if let Some(s) = details::board_specs(&device.board) {
                let note = "As on the real device; LightTouch emulates it, so speed depends on your computer.";
                section(ui, &p, "Hardware", Some(note), |ui| {
                    fact(ui, &p, "Chip", s.chip);
                    fact(ui, &p, "CPU", s.cpu);
                    fact(ui, &p, "GPU", s.gpu);
                    fact(ui, &p, "Memory", s.memory);
                    fact(ui, &p, "Screen", s.screen);
                    fact(
                        ui,
                        &p,
                        "Motion sensors",
                        if s.gyroscope { "Accelerometer and gyroscope" } else { "Accelerometer" },
                    );
                });
            }

            let note = details::storage_note(&device.board, &device.build, "this computer");
            section(ui, &p, "Storage", Some(&note), |ui| {
                if let Some(bytes) = f.flash_bytes {
                    fact(ui, &p, "Disk size", &format!("{} of flash", format_bytes(bytes)));
                }
                match &state.space {
                    None => fact(ui, &p, "On this computer", "Measuring…"),
                    Some(u) => {
                        fact(ui, &p, "On this computer", &format_bytes(u.total()));
                        fact(ui, &p, "Prepared system", &format_bytes(u.base));
                        fact(ui, &p, "Changes since then", &format_bytes(u.changes));
                        if u.logs > 0 {
                            fact(ui, &p, "Engine logs", &format_bytes(u.logs));
                        }
                    }
                }
            });

            section(ui, &p, "LightTouch", None, |ui| {
                if let Some(serial) = f.tools_serial {
                    let verdict = f.tools_verdict.as_deref().filter(|v| *v != "good");
                    let tools = verdict.map_or(format!("Serial {serial}"), |v| format!("Serial {serial} ({v})"));
                    fact(ui, &p, "Guest tools", &tools);
                }
                if let Some(created) = &f.created {
                    // "2026-11-26T09:32:05Z" → "2026-11-26 09:32"
                    fact(ui, &p, "Prepared", &created.chars().take(16).collect::<String>().replacen('T', " ", 1));
                }
                fact(ui, &p, "Record", &device.id);
                if device.dirty {
                    fact(ui, &p, "State", "Not shut down cleanly; it recovers at the next start");
                }
            });
            ui.add_space(28.0);
        });
    });
    if back {
        app.details = None;
        app.page = Page::Device;
    }
}

/// A titled card of facts with an optional note under it, as Android's Details sections are.
fn section(ui: &mut Ui, p: &theme::Palette, title: &str, note: Option<&str>, add: impl FnOnce(&mut Ui)) {
    widgets::section(ui, p, title);
    widgets::card(ui, p, add);
    if let Some(note) = note {
        ui.add_space(4.0);
        ui.label(widgets::muted(p, note).size(12.5));
    }
}

/// One fact: its label in a fixed column, its value selectable for copying.
fn fact(ui: &mut Ui, p: &theme::Palette, label: &str, value: &str) {
    ui.horizontal_top(|ui| {
        ui.set_min_height(28.0);
        let column = egui::Layout::left_to_right(egui::Align::Min);
        ui.allocate_ui_with_layout(egui::vec2(150.0, 20.0), column, |ui| {
            ui.set_min_width(150.0);
            ui.label(widgets::muted(p, label).size(14.0));
        });
        ui.add(egui::Label::new(RichText::new(value).size(14.0)).selectable(true).wrap());
    });
}
