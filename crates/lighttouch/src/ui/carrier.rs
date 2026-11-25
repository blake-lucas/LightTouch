//! Carrier (Settings → Carrier, and the dock's Carrier button): the fake cellular network of a device with a modem,
//! its calls and its texts, like Light Touch for Mac's Carrier panel. It renders the model's carrier settings and the
//! session's modem state; changes go through `Model::set_carrier`, calls and texts through `Model::ring`/`text`.
use super::{App, Dialog, Page, icons, theme, widgets};
use eframe::egui::{self, Frame, Margin, RichText, Ui, vec2};
use lighttouch_core::carrier::{self, CarrierSettings};

/// The page's text fields as typed, for the device they were filled from.
#[derive(Default)]
pub struct CarrierUi {
    form: Option<Form>,
    /// Where the page's Back goes.
    pub back: Option<Page>,
}

struct Form {
    id: String,
    name: String,
    mcc: String,
    mnc: String,
    latitude: String,
    longitude: String,
}

impl Form {
    fn of(id: &str, s: &CarrierSettings) -> Self {
        Self {
            id: id.to_string(),
            name: s.carrier.clone(),
            mcc: s.mcc().to_string(),
            mnc: s.mnc().to_string(),
            latitude: format!("{:.6}", s.latitude),
            longitude: format!("{:.6}", s.longitude),
        }
    }
}

/// Apple Park, the defaults' location.
const APPLE_PARK: (f64, f64) = (37.3349, -122.0090);

/// Open the Carrier page, coming back to `from`.
pub fn open(app: &mut App, from: Page) {
    app.carrier.back = Some(from);
    app.carrier.form = None;
    app.page = Page::Carrier;
}

pub fn show(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    let back = app.carrier.back.clone().unwrap_or(Page::Settings);
    let (Some(device), Some(saved)) = (app.model.carrier_device().cloned(), app.model.carrier_settings()) else {
        app.page = back;
        return;
    };
    if app.carrier.form.as_ref().is_none_or(|f| f.id != device.id) {
        app.carrier.form = Some(Form::of(&device.id, &saved));
    }
    let live = app.model.carrier_live();
    let status = app.model.modem_status().cloned();
    let modem_error = app.model.modem_error().map(str::to_string);
    let sent = app.model.session.as_ref().map(|s| s.sent_texts.clone()).unwrap_or_default();
    let gps = device.board == "n88ap" || device.board == "n42ap" || status.as_ref().is_some_and(|s| s.gps);
    let mut edited = saved.clone();
    let mut button = app.model.settings.carrier_button;
    let mut problem: Option<String> = None;
    egui::ScrollArea::vertical().id_salt("carrier-page").auto_shrink([false, false]).show(ui, |ui| {
        ui.add_space(18.0);
        let width = (ui.available_width() - 48.0).clamp(200.0, 640.0);
        let margin = ((ui.available_width() - width) / 2.0).max(24.0);
        Frame::new().inner_margin(Margin::symmetric(margin as i8, 0)).show(ui, |ui| {
            ui.set_max_width(width);
            ui.horizontal(|ui| {
                if widgets::icon_button(ui, &p, icons::BACK, "Back").clicked() {
                    app.page = back.clone();
                }
                ui.vertical(|ui| {
                    ui.label(RichText::new("Carrier").font(theme::headline()));
                    ui.label(widgets::muted(&p, &device.name).size(12.5));
                });
            });
            let form = app.carrier.form.as_mut().expect("filled above");

            widgets::section(ui, &p, "Network");
            widgets::card(ui, &p, |ui| {
                widgets::setting_row(ui, &p, icons::CELL_TOWER, "Carrier name", "Shown in the status bar", |ui| {
                    ui.add(egui::TextEdit::singleline(&mut form.name).desired_width(170.0).margin(vec2(8.0, 6.0)));
                });
                widgets::setting_row(
                    ui,
                    &p,
                    icons::GLOBE,
                    "Network code",
                    "MCC and MNC. 001 01 is the test network",
                    |ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut form.mnc)
                                .desired_width(44.0)
                                .char_limit(3)
                                .hint_text("01")
                                .margin(vec2(8.0, 6.0)),
                        );
                        ui.label(widgets::muted(&p, "/"));
                        ui.add(
                            egui::TextEdit::singleline(&mut form.mcc)
                                .desired_width(44.0)
                                .char_limit(3)
                                .hint_text("001")
                                .margin(vec2(8.0, 6.0)),
                        );
                    },
                );
                let typed = (form.name.clone(), format!("{}{}", form.mcc.trim(), form.mnc.trim()));
                if typed.0 != saved.carrier || typed.1 != saved.mcc_mnc {
                    let mut candidate = saved.clone();
                    candidate.carrier = typed.0;
                    candidate.mcc_mnc = typed.1;
                    let valid = form.mcc.trim().len() == 3 && candidate.check().is_ok();
                    ui.horizontal(|ui| {
                        ui.add_space(32.0);
                        ui.label(
                            widgets::muted(
                                &p,
                                if valid {
                                    "Apply to change the network"
                                } else {
                                    "A name is 1 to 32 characters without quotes; the MCC is 3 digits and the MNC 2 or 3"
                                },
                            )
                            .size(12.5),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if widgets::tonal_enabled(ui, &p, "Apply", valid).clicked() {
                                edited.carrier = candidate.carrier.clone();
                                edited.mcc_mnc = candidate.mcc_mnc.clone();
                            }
                        });
                    });
                }
                widgets::setting_row(
                    ui,
                    &p,
                    icons::CELL_SIGNAL,
                    "Signal",
                    &format!("{} bar{}", edited.bars, if edited.bars == 1 { "" } else { "s" }),
                    |ui| {
                        widgets::segmented(
                            ui,
                            &p,
                            &mut edited.bars,
                            &[(0, "0"), (1, "1"), (2, "2"), (3, "3"), (4, "4"), (5, "5")],
                        );
                    },
                );
                widgets::setting_row(
                    ui,
                    &p,
                    icons::CELL_TOWER,
                    "Registered",
                    "On the network. Off, iOS shows No Service",
                    |ui| {
                        widgets::switch(ui, &p, &mut edited.registered);
                    },
                );
                widgets::setting_row(ui, &p, icons::SIM_CARD, "SIM card", "Off, iOS shows No SIM", |ui| {
                    widgets::switch(ui, &p, &mut edited.sim_present);
                });
            });
            note(
                ui,
                &p,
                "Saved with this device and used every time it starts. Changes reach a running device at once; iOS can \
                 take a few seconds to update its status bar.",
            );

            if gps {
                widgets::section(ui, &p, "Location");
                widgets::card(ui, &p, |ui| {
                    widgets::setting_row(
                        ui,
                        &p,
                        icons::MAP_PIN,
                        "GPS position",
                        "Where the iPhone's GPS says it is, in degrees",
                        |ui| {
                            let typed = (form.latitude.trim().parse::<f64>(), form.longitude.trim().parse::<f64>());
                            let valid = matches!(typed, (Ok(a), Ok(o)) if a.abs() <= 90.0 && o.abs() <= 180.0);
                            if widgets::tonal_enabled(ui, &p, "Set", valid).clicked() {
                                if let (Ok(latitude), Ok(longitude)) = typed {
                                    edited.latitude = latitude;
                                    edited.longitude = longitude;
                                }
                            }
                            ui.add(
                                egui::TextEdit::singleline(&mut form.longitude)
                                    .desired_width(110.0)
                                    .hint_text("Longitude")
                                    .margin(vec2(8.0, 6.0)),
                            );
                            ui.add(
                                egui::TextEdit::singleline(&mut form.latitude)
                                    .desired_width(96.0)
                                    .hint_text("Latitude")
                                    .margin(vec2(8.0, 6.0)),
                            );
                        },
                    );
                    widgets::setting_row(ui, &p, icons::MAP_PIN, "Apple Park", "Cupertino, the default", |ui| {
                        if widgets::tonal(ui, &p, "Go there").clicked() {
                            (edited.latitude, edited.longitude) = APPLE_PARK;
                            form.latitude = format!("{:.6}", APPLE_PARK.0);
                            form.longitude = format!("{:.6}", APPLE_PARK.1);
                        }
                    });
                });
            }

            widgets::section(ui, &p, "Session dock");
            widgets::card(ui, &p, |ui| {
                widgets::setting_row(
                    ui,
                    &p,
                    icons::PHONE_CALL,
                    "Show Carrier button",
                    "On every iPhone: a dock button for calling the device, sending it texts and these settings",
                    |ui| {
                        widgets::switch(ui, &p, &mut button);
                    },
                );
            });

            // The calls and texts themselves are the dock button's (one control per action): this shows how they stand.
            widgets::section(ui, &p, "This session");
            widgets::card(ui, &p, |ui| {
                if !live {
                    ui.label(widgets::muted(&p, "Start the device to see its calls and texts here."));
                    return;
                }
                let call = status.as_ref().map_or("No call", |s| s.call_label());
                let detail = status
                    .as_ref()
                    .filter(|s| s.in_call() && !s.last_dialed.is_empty() && s.can_answer())
                    .map(|s| format!("{call}: {}", s.last_dialed))
                    .unwrap_or_else(|| call.to_string());
                let how = if button {
                    "The dock's Carrier button calls the device, sends it texts, answers and hangs up"
                } else {
                    "Turn on Show Carrier button to call and text the device from the dock"
                };
                widgets::setting_row(ui, &p, icons::PHONE_INCOMING, &detail, how, |_| {});
                if sent.is_empty() {
                    ui.label(widgets::muted(&p, "Texts the device sends appear here.").size(12.5));
                } else {
                    ui.label(widgets::muted(&p, "Texts the device sent").size(12.5));
                    for (number, text) in sent.iter().rev().take(5) {
                        ui.horizontal_wrapped(|ui| {
                            ui.add_space(32.0);
                            ui.label(RichText::new(format!("To {number}")).size(12.5).color(p.on_surface_variant));
                            ui.label(RichText::new(text).size(13.5));
                        });
                    }
                }
            });
            if let Some(message) = problem.as_ref().or(modem_error.as_ref()) {
                note(ui, &p, message);
            }
            ui.add_space(28.0);
        });
    });
    if button != app.model.settings.carrier_button {
        app.model.settings.carrier_button = button;
        app.model.save_settings();
    }
    if edited != saved {
        if let Err(e) = app.model.set_carrier(edited) {
            problem = Some(e);
        }
    }
    if let Some(problem) = problem {
        app.model.error = Some(problem);
    }
}

/// The menu's actions (the dock's and full screen's).
fn act(app: &mut App, open_call: bool, open_text: bool, answer: bool, hang_up: bool) {
    let saved = app.model.carrier_settings().unwrap_or_default();
    if open_call {
        app.dialog = Some(Dialog::CarrierCall { number: saved.last_caller.clone() });
    }
    if open_text {
        app.dialog = Some(Dialog::CarrierText { number: saved.last_sender.clone(), text: String::new() });
    }
    let result = if answer {
        app.model.remote_answer()
    } else if hang_up {
        app.model.remote_hangup()
    } else {
        Ok(())
    };
    if let Err(e) = result {
        app.model.error = Some(e);
    }
}

fn note(ui: &mut Ui, p: &theme::Palette, text: &str) {
    Frame::new().inner_margin(Margin { left: 18, right: 18, top: 8, bottom: 0 }).show(ui, |ui| {
        ui.label(widgets::muted(p, text).size(12.5));
    });
}

/// Whether the session dock shows the Carrier button: the running device has a modem and the setting is on.
pub fn wants_dock_button(app: &mut App) -> bool {
    app.model.settings.carrier_button && app.model.carrier_live()
}

/// The dock's Carrier button and its menu.
pub fn dock_button(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    let in_call = app.model.modem_status().is_some_and(|s| s.in_call());
    let r = widgets::dock_button(ui, &p, icons::CELL_SIGNAL, "Carrier", true, in_call.then_some(p.primary));
    let r = if in_call {
        let label = app.model.modem_status().map(|s| s.call_label()).unwrap_or_default();
        r.on_hover_text(label)
    } else {
        r
    };
    egui::Popup::menu(&r).show(|ui| menu_items(app, ui));
}

/// The Carrier menu: the call's state and controls, Call…, Send text… and the settings.
pub fn menu_items(app: &mut App, ui: &mut Ui) {
    let status = app.model.modem_status().cloned();
    let saved = app.model.carrier_settings().unwrap_or_default();
    let on_network = saved.registered && saved.sim_present;
    let (mut open_call, mut open_text, mut answer, mut hang_up) = (false, false, false, false);
    let state = status.as_ref().map_or("No call", |s| s.call_label());
    ui.label(widgets::muted(&app.palette, state).size(12.5));
    if let Some(s) = &status {
        if s.can_answer() && ui.button(widgets::icon_label(icons::PHONE_CALL, "Answer", None)).clicked() {
            answer = true;
        }
        if s.in_call() && ui.button(widgets::icon_label(icons::PHONE_HANG_UP, "Hang up", Some(theme::DANGER))).clicked()
        {
            hang_up = true;
        }
    }
    let idle = !status.as_ref().is_some_and(|s| s.in_call());
    if ui
        .add_enabled(idle && on_network, egui::Button::new(widgets::icon_label(icons::PHONE_INCOMING, "Call…", None)))
        .clicked()
    {
        open_call = true;
    }
    if ui
        .add_enabled(on_network, egui::Button::new(widgets::icon_label(icons::CHAT_TEXT, "Send text…", None)))
        .clicked()
    {
        open_text = true;
    }
    ui.separator();
    if ui.button(widgets::icon_label(icons::GEAR, "Carrier settings…", None)).clicked() {
        let ctx = ui.ctx().clone();
        let from = if app.fullscreen { Page::Device } else { app.page.clone() };
        if app.fullscreen {
            app.set_fullscreen(&ctx, false);
        }
        open(app, from);
    }
    act(app, open_call, open_text, answer, hang_up);
}

/// The Call… dialog's body. Returns whether it closes.
pub fn call_dialog(app: &mut App, ui: &mut Ui, number: &mut String) -> bool {
    let p = app.palette;
    ui.label(widgets::icon_text(icons::PHONE_INCOMING, 26.0).color(p.primary));
    ui.add_space(6.0);
    ui.label(RichText::new("Call the device").font(theme::title()));
    ui.add_space(4.0);
    ui.label(widgets::muted(&p, "It rings from this number; answer it on the device."));
    ui.add_space(10.0);
    let edit = ui.add(
        egui::TextEdit::singleline(number)
            .desired_width(f32::INFINITY)
            .hint_text("+15555550100")
            .margin(vec2(10.0, 8.0)),
    );
    if !edit.has_focus() && !edit.lost_focus() {
        edit.request_focus();
    }
    let valid = carrier::number_ok(number.trim());
    if !valid && !number.trim().is_empty() {
        ui.label(widgets::muted(&p, "1 to 20 digits, optionally after a +").size(12.5));
    }
    let submit = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    ui.add_space(16.0);
    let mut close = false;
    let mut call = false;
    widgets::dialog_buttons(ui, |ui| {
        call = widgets::primary_enabled(ui, &p, "Call", valid).clicked();
        if widgets::text_button(ui, &p, "Cancel").clicked() {
            close = true;
        }
    });
    if (call || submit) && valid {
        if let Err(e) = app.model.ring(number) {
            app.model.error = Some(e);
        }
        close = true;
    }
    close
}

/// The Send text… dialog's body. Returns whether it closes.
pub fn text_dialog(app: &mut App, ui: &mut Ui, number: &mut String, text: &mut String) -> bool {
    let p = app.palette;
    ui.label(widgets::icon_text(icons::CHAT_TEXT, 26.0).color(p.primary));
    ui.add_space(6.0);
    ui.label(RichText::new("Send a text to the device").font(theme::title()));
    ui.add_space(10.0);
    ui.label(RichText::new("From").size(12.5).color(p.on_surface_variant));
    ui.add(
        egui::TextEdit::singleline(number)
            .desired_width(f32::INFINITY)
            .hint_text("+15555550100")
            .margin(vec2(10.0, 8.0)),
    );
    ui.add_space(8.0);
    ui.label(RichText::new("Message").size(12.5).color(p.on_surface_variant));
    let edit = ui.add(
        egui::TextEdit::multiline(text)
            .desired_width(f32::INFINITY)
            .desired_rows(3)
            .hint_text("Type a message")
            .margin(vec2(10.0, 8.0)),
    );
    if !edit.has_focus() && !edit.lost_focus() && text.is_empty() {
        edit.request_focus();
    }
    let (used, limit) = carrier::sms_length(text);
    let number_ok = carrier::number_ok(number.trim());
    let text_ok = !text.is_empty() && used <= limit;
    let hint = if !number_ok && !number.trim().is_empty() {
        "The sender is 1 to 20 digits, optionally after a +".to_string()
    } else if limit == 0 {
        "Emoji can't be sent in a text this old".to_string()
    } else {
        format!("{used} / {limit}")
    };
    ui.label(widgets::muted(&p, hint).size(12.5));
    ui.add_space(12.0);
    let mut close = false;
    let mut send = false;
    widgets::dialog_buttons(ui, |ui| {
        send = widgets::primary_enabled(ui, &p, "Send", number_ok && text_ok).clicked();
        if widgets::text_button(ui, &p, "Cancel").clicked() {
            close = true;
        }
    });
    if send {
        if let Err(e) = app.model.text(number, text) {
            app.model.error = Some(e);
        }
        close = true;
    }
    close
}

/// Why the device Settings is about has no carrier settings: none selected, or a board without a modem we drive.
fn unavailable_reason(app: &App) -> String {
    match app.model.current() {
        None => "Select an iPhone to change its carrier, calls and texts.".to_string(),
        Some(d) => format!(
            "{} has no phone modem. Carrier settings apply to the original iPhone, iPhone 3GS, iPhone 4 and iPhone 5.",
            d.model
        ),
    }
}

/// Settings' Carrier row: always shown, so Settings keeps its shape; greyed out with the reason when the device
/// has no modem LightTouch drives.
pub fn settings_row(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    let Some(settings) = app.model.carrier_settings() else {
        let reason = unavailable_reason(app);
        ui.add_enabled_ui(false, |ui| {
            widgets::setting_row(ui, &p, icons::CELL_SIGNAL, "Carrier", "Not available for this device", |ui| {
                widgets::tonal(ui, &p, "Open")
            });
        });
        Frame::new().inner_margin(Margin { left: 38, right: 18, top: 0, bottom: 8 }).show(ui, |ui| {
            ui.label(widgets::muted(&p, reason).size(12.5));
        });
        return;
    };
    let summary = if !settings.sim_present {
        "No SIM".to_string()
    } else if !settings.registered {
        "No Service".to_string()
    } else {
        format!("{} · {} bar{}", settings.carrier, settings.bars, if settings.bars == 1 { "" } else { "s" })
    };
    let mut open_page = false;
    widgets::setting_row(ui, &p, icons::CELL_SIGNAL, "Carrier", &summary, |ui| {
        if widgets::tonal(ui, &p, "Open").clicked() {
            open_page = true;
        }
    });
    if open_page {
        open(app, Page::Settings);
    }
}
