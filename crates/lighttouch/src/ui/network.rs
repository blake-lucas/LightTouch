//! Settings → Network, like Android's network screen: whether the guest can reach the internet, directly or through
//! the Wayback Machine at a chosen date, local network access, and what the running session is doing. It only
//! renders the model's settings and the session's reported status; changes go through `Model::set_network`.
use super::{App, Page, icons, theme, widgets};
use eframe::egui::{self, Color32, CornerRadius, Frame, Margin, RichText, Ui};
use lighttouch_core::protocol::NetworkStatus;
use lighttouch_core::runtime::NetworkMode;
use lighttouch_model::network::{Day, Route, days_in_month, month_name};

pub fn show(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    let saved = app.model.settings.network.clone();
    let mut edited = saved.clone();
    let active = app.model.session_active();
    let status = app.model.session.as_ref().filter(|s| s.phase.active()).and_then(|s| s.network.clone());
    let today = Day::today();
    let https_id = egui::Id::unique("network-https-info");
    let mut show_https = ui.ctx().data(|d| d.get_temp::<bool>(https_id)).unwrap_or(false);
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.add_space(18.0);
        let width = (ui.available_width() - 48.0).clamp(200.0, 640.0);
        let margin = ((ui.available_width() - width) / 2.0).max(24.0);
        egui::Frame::new().inner_margin(egui::Margin::symmetric(margin as i8, 0)).show(ui, |ui| {
            ui.set_max_width(width);
            ui.horizontal(|ui| {
                if widgets::icon_button(ui, &p, icons::BACK, "Back to settings").clicked() {
                    app.page = Page::Settings;
                }
                ui.label(RichText::new("Network").font(theme::headline()));
            });
            ui.scope(|ui| {
                widgets::section(ui, &p, "Connection");
                widgets::card(ui, &p, |ui| {
                    widgets::setting_row(
                        ui,
                        &p,
                        icons::GLOBE,
                        "Enable networking",
                        "Let the emulated device connect through this computer's internet connection",
                        |ui| {
                            widgets::switch(ui, &p, &mut edited.enabled);
                        },
                    );
                });
                note(
                    ui,
                    &p,
                    "Changes apply right away, even while the device is running: reload the page to use them. The \
                     first time networking is on in a session, the device takes a few seconds to set up.",
                );

                widgets::section(ui, &p, "Browse with");
                widgets::card(ui, &p, |ui| {
                    route_row(
                        ui,
                        &p,
                        &mut edited.route,
                        Route::Direct,
                        icons::GLOBE,
                        "Connect to websites as they are today",
                    );
                    route_row(
                        ui,
                        &p,
                        &mut edited.route,
                        Route::Archive,
                        icons::CLOCK_COUNTER,
                        "Visit pages preserved by the Internet Archive",
                    );
                    if edited.route == Route::Archive {
                        let date = edited.date();
                        widgets::setting_row(ui, &p, icons::CLOCK, "Archive date", "", |ui| {
                            if let Some(day) = date_picker(ui, date, today) {
                                edited.archive_date = day.compact();
                            }
                        });
                        const HELP: &str = "Uses the closest available capture. Some pages, images and online \
                                            services weren't archived. Forms and sign-ins need Direct internet.";
                        Frame::new().inner_margin(Margin { left: 30, right: 0, top: 0, bottom: 4 }).show(ui, |ui| {
                            ui.label(widgets::muted(&p, HELP).size(12.5));
                        });
                    }
                });

                widgets::section(ui, &p, "Compatibility");
                widgets::card(ui, &p, |ui| {
                    widgets::setting_row(
                        ui,
                        &p,
                        icons::INFO,
                        "HTTPS compatibility",
                        "Connect old iOS to modern HTTPS websites",
                        |ui| {
                            if widgets::tonal(ui, &p, "Learn more").clicked() {
                                show_https = true;
                            }
                        },
                    );
                    if edited.route == Route::Direct {
                        widgets::setting_row(
                            ui,
                            &p,
                            icons::WIFI,
                            "Allow local network",
                            "Reach other devices on your network. Off by default",
                            |ui| {
                                widgets::switch(ui, &p, &mut edited.local_network);
                            },
                        );
                    }
                });
            });

            if active {
                widgets::section(ui, &p, "Current session");
                widgets::card(ui, &p, |ui| session_info(ui, &p, status.as_ref()));
            }
            ui.add_space(28.0);
        });
    });
    if edited != saved {
        app.model.set_network(edited);
    }
    if show_https {
        show_https = https_dialog(ui.ctx(), &p);
    }
    ui.ctx().data_mut(|d| d.insert_temp(https_id, show_https));
}

fn note(ui: &mut Ui, p: &theme::Palette, text: &str) {
    Frame::new().inner_margin(Margin { left: 18, right: 18, top: 8, bottom: 0 }).show(ui, |ui| {
        ui.label(widgets::muted(p, text).size(12.5));
    });
}

fn route_row(ui: &mut Ui, p: &theme::Palette, route: &mut Route, this: Route, icon: &str, subtitle: &str) {
    widgets::setting_row(ui, p, icon, this.label(), subtitle, |ui| {
        if radio(ui, p, *route == this).clicked() {
            *route = this;
        }
    });
}

/// A radio button in the palette's colours (egui's own is faint on these cards).
fn radio(ui: &mut Ui, p: &theme::Palette, selected: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::click());
    let painter = ui.painter();
    let color = if selected { p.primary } else { p.outline };
    painter.circle_stroke(rect.center(), 9.0, egui::Stroke::new(2.0, color));
    if selected {
        painter.circle_filled(rect.center(), 5.0, color);
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Day, month and year, kept to a real day from 1996 through today. Laid out right to left (the row's trailing
/// side), so the year goes first.
fn date_picker(ui: &mut Ui, date: Day, today: Day) -> Option<Day> {
    let (mut year, mut month, mut day) = (date.year, date.month, date.day);
    let mut changed = ui.add(egui::DragValue::new(&mut year).range(Day::FIRST.year..=today.year)).changed();
    egui::ComboBox::from_id_salt("network-archive-month").selected_text(month_name(month)).width(110.0).show_ui(
        ui,
        |ui| {
            for m in 1..=12 {
                changed |= ui.selectable_value(&mut month, m, month_name(m)).changed();
            }
        },
    );
    changed |= ui.add(egui::DragValue::new(&mut day).range(1..=days_in_month(year, month))).changed();
    changed.then(|| Day::clamped(year, month, day, today)).filter(|d| *d != date)
}

/// What the running session applied and is doing, apart from the saved settings.
fn session_info(ui: &mut Ui, p: &theme::Palette, status: Option<&NetworkStatus>) {
    ui.spacing_mut().item_spacing.y = 12.0;
    ui.horizontal(|ui| {
        ui.label(widgets::icon_text(icons::INFO, 18.0).color(p.on_surface_variant));
        ui.label(widgets::muted(p, "Connection information for this session").size(12.5));
    });
    let Some(status) = status else {
        info_line(ui, p, "Connection", "Waiting for network status…", false);
        return;
    };
    let mode = status.applied.mode;
    let connection = match mode {
        NetworkMode::Off => "Networking is off",
        NetworkMode::Direct => Route::Direct.label(),
        NetworkMode::Archive => Route::Archive.label(),
    };
    info_line(ui, p, "Connection", connection, false);
    if mode == NetworkMode::Archive {
        let date = Day::parse(&status.applied.archive_date).map_or(status.applied.archive_date.clone(), Day::long);
        info_line(ui, p, "Archive date", &date, false);
    }
    if mode != NetworkMode::Off || status.setup_error {
        let label = if status.setup_error { "Guest setup needs attention" } else { "Guest setup" };
        let setup =
            if status.setup == "Ready" { "Ready — certificate and proxy settings installed" } else { &status.setup };
        info_line(ui, p, label, setup, status.setup_error);
    }
    if mode != NetworkMode::Off {
        info_line(ui, p, "Web activity", &status.activity, false);
        if status.connections > 0 {
            let totals = format!(
                "Connections received: {}\nFinished: {} · Errors: {}",
                status.connections, status.completed, status.failed
            );
            info_line(ui, p, "Session totals", &totals, false);
        }
        if !status.last_result.is_empty() {
            let label = if status.last_result_error { "Last result (error)" } else { "Last result" };
            info_line(ui, p, label, &status.last_result, status.last_result_error);
        }
        if status.setup == "Ready" && status.connections == 0 {
            ui.label(
                widgets::muted(
                    p,
                    "Setup is complete. Open a page in Safari to check the connection. Being ready doesn't confirm \
                     that a website can load.",
                )
                .size(12.5),
            );
        }
    }
    if !status.notice.is_empty() {
        info_line(ui, p, "Settings update", &status.notice, false);
    }
}

fn info_line(ui: &mut Ui, p: &theme::Palette, label: &str, value: &str, error: bool) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 3.0;
        ui.label(widgets::muted(p, label).size(12.5));
        ui.label(RichText::new(value).size(14.0).color(if error { theme::DANGER } else { p.on_surface }));
    });
}

/// Returns whether it stays open.
fn https_dialog(ctx: &egui::Context, p: &theme::Palette) -> bool {
    let mut open = true;
    let modal = egui::Modal::new(egui::Id::unique("network-https-dialog"))
        .backdrop_color(Color32::from_black_alpha(140))
        .frame(
            Frame::new()
                .fill(p.container_high)
                .corner_radius(CornerRadius::same(theme::radius::DOCK))
                .inner_margin(Margin::same(24)),
        );
    let response = modal.show(ctx, |ui| {
        ui.set_width(400.0);
        ui.label(widgets::icon_text(icons::INFO, 26.0).color(p.primary));
        ui.add_space(6.0);
        ui.label(RichText::new("HTTPS compatibility").font(theme::title()));
        ui.add_space(4.0);
        for text in [
            "Old versions of iOS can't connect to many HTTPS websites today. LightTouch gives the emulated device its \
             own proxy certificate, then connects to websites using modern TLS and the certificates this computer \
             trusts.",
            "The certificate is trusted only inside the emulated device. This computer's certificate settings stay as \
             they are. Apps that pin certificates may still refuse the connection.",
            "Setup runs once the guest tools are available, at startup or when you turn networking on. If setup \
             fails, update guest tools and restart the device.",
        ] {
            ui.label(widgets::muted(p, text));
            ui.add_space(6.0);
        }
        ui.add_space(10.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if widgets::text_button(ui, p, "Got it").clicked() {
                open = false;
            }
        });
    });
    open && !response.should_close()
}
