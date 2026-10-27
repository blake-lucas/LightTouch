//! The Legacy Store page, as on Android: the device's own list and search, the website's browsing tabs (Featured,
//! Charts, Categories, Collections), the apps on the device with newer versions, an app's page with its versions
//! and screenshots, and the install queue behind a button in the header. Also installs a local IPA and shows the
//! newest crash report.
use super::{App, Page, icons, theme, widgets};
use eframe::egui::{self, Align2, CornerRadius, FontId, Margin, RichText, Sense, Stroke, Ui, vec2};
use lighttouch_catalog::catalog::short_product_name;
use lighttouch_catalog::store::{self, CATEGORY_SORTS, CHART_TYPES, StoreApp};
use lighttouch_model::queue::{self, QueueItem, QueueState};
use lighttouch_model::store::{Icon, ListingKind, Lookup, Tab, app_title, app_version, byte_size};
use lighttouch_services::instproxy::InstalledApp;

const WIDTH: f32 = 760.0;

/// The page's column: centred, at most `WIDTH` wide.
fn column(ui: &mut Ui, top: i8, add: impl FnOnce(&mut Ui)) {
    let w = (ui.available_width() - 48.0).clamp(200.0, WIDTH);
    let margin = ((ui.available_width() - w) / 2.0).max(24.0);
    egui::Frame::new().inner_margin(Margin { left: margin as i8, right: margin as i8, top, bottom: 0 }).show(
        ui,
        |ui| {
            ui.set_max_width(w);
            add(ui);
        },
    );
}

pub fn show(app: &mut App, ui: &mut Ui) {
    let browsing = app.model.store.chosen.is_none() && app.model.store.installed_apps.plain.is_none();
    column(ui, 24, |ui| {
        header(app, ui, browsing);
        notices(app, ui);
        if browsing {
            let mut tab = app.model.store.tab;
            let options: Vec<(Tab, &str)> = Tab::ALL.iter().map(|t| (*t, t.title())).collect();
            // A narrow window scrolls the tabs sideways.
            let changed = egui::ScrollArea::horizontal()
                .id_salt("store-tabs")
                .show(ui, |ui| ui.horizontal(|ui| widgets::segmented(ui, &app.palette, &mut tab, &options)).inner)
                .inner;
            if changed {
                app.model.store.show(tab);
            }
            ui.add_space(10.0);
        }
    });
    // Each view keeps its own scroll position, so a list stays where it was while an app's page is open.
    let store = &app.model.store;
    let view = match (&store.chosen, &store.installed_apps.plain) {
        (Some(chosen), _) => format!("app-{}-{}", chosen.bundle_id, chosen.name),
        (_, Some(plain)) => format!("plain-{}", plain.bundle_id),
        _ => match store.tab {
            Tab::Categories => format!("cat-{:?}", store.browser.category.as_ref().map(|g| &g.id)),
            Tab::Collections => format!("col-{:?}", store.browser.collection.as_ref().map(|c| &c.slug)),
            tab => format!("{tab:?}"),
        },
    };
    egui::ScrollArea::vertical().id_salt(("store", view)).auto_shrink([false, false]).show(ui, |ui| {
        column(ui, 4, |ui| {
            if app.model.store.chosen.is_some() {
                details(app, ui);
            } else if app.model.store.installed_apps.plain.is_some() {
                plain_page(app, ui);
            } else {
                match app.model.store.tab {
                    Tab::Device => device_tab(app, ui),
                    Tab::Installed => installed_tab(app, ui),
                    Tab::Featured => featured_tab(app, ui),
                    Tab::Charts => charts_tab(app, ui),
                    Tab::Categories => categories_tab(app, ui),
                    Tab::Collections => collections_tab(app, ui),
                }
            }
            ui.add_space(24.0);
        });
    });
}

fn header(app: &mut App, ui: &mut Ui, browsing: bool) {
    let p = app.palette;
    let subtitle = match &app.model.store.target {
        Some(t) => format!("{} · iOS {}", short_product_name(&t.product_type), t.ios),
        None => "No device selected".into(),
    };
    let mut submit = false;
    ui.horizontal(|ui| {
        if widgets::icon_button(ui, &p, icons::BACK, "Back").clicked() {
            let store = &mut app.model.store;
            if store.chosen.is_some() {
                store.close();
            } else if store.installed_apps.plain.is_some() {
                store.close_plain();
            } else if store.can_go_back() {
                store.back();
            } else {
                app.page = Page::Device;
            }
        }
        ui.vertical(|ui| {
            ui.label(RichText::new("Legacy Store").font(theme::headline()));
            ui.label(widgets::muted(&p, subtitle));
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            more_menu(app, ui);
            queue_button(app, ui);
            if browsing {
                submit = search_field(app, ui);
            }
        });
    });
    if submit {
        app.model.store.search();
    }
    ui.add_space(14.0);
}

fn more_menu(app: &mut App, ui: &mut Ui) {
    let connected = app.model.store.guest_ready();
    let more = widgets::icon_button(ui, &app.palette, icons::MORE, "More");
    egui::Popup::menu(&more).show(|ui| {
        ui.set_min_width(200.0);
        if ui
            .add_enabled(connected, egui::Button::new(widgets::icon_label(icons::FILE, "Install an IPA file…", None)))
            .clicked()
        {
            if let Some(file) =
                rfd::FileDialog::new().set_title("Choose an IPA").add_filter("iOS app", &["ipa"]).pick_file()
            {
                app.model.store.install_file(file);
            }
        }
        if ui
            .add_enabled(connected, egui::Button::new(widgets::icon_label(icons::INFO, "Show last crash", None)))
            .clicked()
        {
            app.model.store.show_last_crash();
        }
    });
}

/// The search field in the header; Enter searches, in the device's own list.
fn search_field(app: &mut App, ui: &mut Ui) -> bool {
    let p = app.palette;
    let mut submit = false;
    // As wide as the header leaves, up to 270 points; none when the window is too narrow for it.
    let width = (ui.available_width() - 12.0).min(270.0);
    if width < 150.0 {
        return false;
    }
    // A fixed-height pill with its contents centred in it: a frame sized by its contents sat taller than the
    // text in the header's row and left the text near its top.
    let (rect, pill) = ui.allocate_exact_size(vec2(width, 36.0), Sense::click());
    ui.painter().rect_filled(rect, CornerRadius::same(theme::radius::PILL), p.container_high);
    let layout = egui::Layout::left_to_right(egui::Align::Center);
    let edit = ui
        .scope_builder(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(12.0, 0.0))).layout(layout), |ui| {
            ui.label(widgets::icon_text(icons::SEARCH, 16.0).color(p.on_surface_variant));
            let edit = ui.add(
                egui::TextEdit::singleline(&mut app.model.store.query)
                    .hint_text("Search apps and games")
                    .frame(egui::Frame::NONE)
                    .desired_width(ui.available_width() - 24.0)
                    .align(egui::Align2::LEFT_CENTER)
                    .margin(vec2(4.0, 2.0)),
            );
            submit = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if app.model.store.searching {
                widgets::spinner(ui, &p, 14.0);
            }
            edit
        })
        .inner;
    // The whole pill focuses the field, not just its text.
    if pill.clicked() {
        edit.request_focus();
    }
    submit
}

/// The install queue's button: a download icon in a progress ring, which opens the queue. Absent while empty.
fn queue_button(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    let items = app.model.store.queue.clone();
    if items.is_empty() {
        return;
    }
    let busy = items.iter().any(QueueItem::active);
    let failed = !busy && items.iter().any(|i| i.state == QueueState::Failed);
    let response =
        widgets::icon_button(ui, &p, icons::DOWNLOAD, if busy { "Installs in progress" } else { "Installs" });
    let center = response.rect.center();
    if failed {
        ui.painter().circle_filled(response.rect.right_top() + vec2(-6.0, 6.0), 4.0, theme::DANGER);
    }
    if busy {
        // The share done; before any (fetching, preparing) a quarter arc going round.
        let fraction = queue::fraction(&items);
        let (start, sweep) = if fraction > 0.01 {
            (0.0, fraction.min(1.0))
        } else {
            ((ui.input(|i| i.time) * 0.9).fract() as f32, 0.25)
        };
        let radius = 17.0;
        ui.painter().circle_stroke(center, radius, Stroke::new(2.5, p.container_highest));
        let steps = 48;
        let points: Vec<egui::Pos2> = (0..=((steps as f32 * sweep) as usize).max(2))
            .map(|i| {
                let turn = start + sweep * i as f32 / (steps as f32 * sweep).max(2.0);
                let angle = -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * turn;
                center + vec2(angle.cos(), angle.sin()) * radius
            })
            .collect();
        ui.painter().add(egui::Shape::line(points, Stroke::new(2.5, p.primary)));
        if fraction > 0.01 {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
        } else {
            ui.ctx().request_repaint();
        }
    }
    egui::Popup::menu(&response)
        .align(egui::RectAlign::BOTTOM_END)
        .gap(6.0)
        // Room around the contents, like Android's sheet; a menu's own margin is a few points.
        .frame(
            egui::Frame::popup(ui.style())
                .inner_margin(Margin::symmetric(18, 14))
                .corner_radius(CornerRadius::same(16)),
        )
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_width(320.0);
            ui.horizontal(|ui| {
                ui.set_min_height(28.0);
                let title = if busy { "Installing" } else { "Installs" };
                ui.label(RichText::new(title).size(14.5).family(theme::semibold()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if items.iter().any(|i| !i.active()) && widgets::text_button(ui, &p, "Clear finished").clicked() {
                        app.model.store.clear_finished();
                    }
                });
            });
            // Tall enough for the list as laid out last frame, up to the cap: a popup's area keeps last frame's size,
            // so without it a row that grew (a progress bar appearing) scrolled inside a list that should fit.
            let list = ui.scope_id().with("queue-list");
            let content = ui.data(|d| d.get_temp::<f32>(list)).unwrap_or(0.0);
            let scroll =
                egui::ScrollArea::vertical().max_height(340.0).min_scrolled_height(content.min(340.0)).show(ui, |ui| {
                    for item in &items {
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.set_width(220.0);
                                ui.add(egui::Label::new(RichText::new(&item.name).family(theme::medium())).truncate());
                                match &item.error {
                                    Some(error) => {
                                        ui.add(egui::Label::new(RichText::new(error).color(p.error).size(12.5)).wrap())
                                    }
                                    None => ui.add(egui::Label::new(widgets::muted(&p, &item.phase).size(12.5)).wrap()),
                                };
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if !item.active()
                                    && widgets::icon_button_sized(
                                        ui,
                                        &p,
                                        icons::CLOSE,
                                        "Remove from the list",
                                        30.0,
                                        true,
                                    )
                                    .clicked()
                                {
                                    app.model.store.dismiss(item.id);
                                }
                                if item.cancellable() && widgets::text_button(ui, &p, "Cancel").clicked() {
                                    app.model.store.cancel(item.id);
                                }
                                if item.state == QueueState::Failed && widgets::text_button(ui, &p, "Retry").clicked() {
                                    app.model.store.retry(item.id);
                                }
                            });
                        });
                        if item.active() {
                            // Nothing measured yet (waiting, preparing): a moving bar, not an empty one.
                            ui.add_space(4.0);
                            let fraction = item.progress.filter(|f| *f > 0.01);
                            widgets::progress(ui, &p, fraction, ui.available_width());
                        }
                    }
                });
            if (scroll.content_size.y - content).abs() > 0.5 {
                ui.data_mut(|d| d.insert_temp(list, scroll.content_size.y));
                ui.ctx().request_repaint();
            }
            if items.iter().any(|i| i.committed && i.active()) {
                ui.add_space(6.0);
                ui.label(widgets::muted(&p, "Keep the device running until iOS finishes installing.").size(12.0));
            }
        });
}

/// The crash report, and a guest USB bridge that couldn't start.
fn notices(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    if let Some(crash) = app.model.store.crash.clone() {
        widgets::card(ui, &p, |ui| {
            ui.label(RichText::new("Last crash").font(theme::title()));
            ui.add_space(6.0);
            egui::ScrollArea::vertical().max_height(260.0).id_salt("crash").show(ui, |ui| {
                ui.add(egui::Label::new(RichText::new(&crash).monospace().size(12.0)).wrap());
            });
            ui.add_space(4.0);
            if widgets::text_button(ui, &p, "Close").clicked() {
                app.model.store.dismiss_crash();
            }
        });
        ui.add_space(12.0);
    }
    // Waiting for iOS shows on the Install button; only a bridge that failed to start gets a line here.
    if let Some(problem) = app.model.store.usb_problem.clone().filter(|_| app.model.session_active()) {
        ui.horizontal(|ui| {
            ui.label(widgets::icon_text(icons::WARNING, 16.0).color(p.error));
            ui.add(egui::Label::new(RichText::new(problem).color(p.error)).wrap());
        });
        ui.add_space(10.0);
    }
}

fn error_line(app: &App, ui: &mut Ui, message: &str) -> bool {
    let mut retry = false;
    ui.horizontal(|ui| {
        ui.add(egui::Label::new(RichText::new(message).color(app.palette.error).size(13.0)).wrap());
        retry = widgets::text_button(ui, &app.palette, "Retry").clicked();
    });
    retry
}

fn loading(app: &App, ui: &mut Ui) {
    ui.add_space(8.0);
    widgets::progress(ui, &app.palette, None, ui.available_width());
    ui.add_space(8.0);
}

fn empty_state(app: &App, ui: &mut Ui, title: &str, body: &str) {
    let p = app.palette;
    ui.add_space(30.0);
    ui.vertical_centered(|ui| {
        ui.label(widgets::icon_text(icons::BAG, 40.0).color(p.on_surface_variant));
        ui.add_space(6.0);
        ui.label(RichText::new(title).font(theme::title()));
        ui.label(widgets::muted(&p, body));
    });
}

/// The apps the catalog has checked against this device, and search results (which also show the ones that won't
/// run).
fn device_tab(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    ui.horizontal(|ui| {
        let on = app.model.store.show_incompatible;
        if widgets::chip(ui, &p, on, "Include apps that won't run").clicked() {
            app.model.store.show_incompatible = !on;
        }
    });
    ui.add_space(8.0);
    if app.model.store.searching {
        loading(app, ui);
    }
    if !app.model.store.message.is_empty() {
        ui.label(widgets::muted(&p, &app.model.store.message));
        ui.add_space(8.0);
    }
    let rows: Vec<StoreApp> = app.model.store.visible_results().into_iter().cloned().collect();
    if rows.is_empty() {
        if !app.model.store.searching && app.model.store.message.is_empty() {
            if app.model.store.results.is_empty() {
                empty_state(
                    app,
                    ui,
                    "Find apps and games",
                    "Search the archive of classic iOS apps. Versions that run on this device are picked for you.",
                );
            } else {
                empty_state(
                    app,
                    ui,
                    "Nothing here runs on this device",
                    "Include apps that won't run to see every result.",
                );
            }
        }
        return;
    }
    widgets::card(ui, &p, |ui| {
        for row in rows {
            if result_row(app, ui, &row, None).clicked() {
                app.model.store.open(&row);
            }
        }
    });
}

/// An app list that pages in as it is scrolled to the end.
fn listing(app: &mut App, ui: &mut Ui, kind: ListingKind, empty: &str) {
    let p = app.palette;
    let (rows, loading_now, error, loaded, has_next) = {
        let b = &app.model.store.browser;
        let l = match kind {
            ListingKind::Featured => &b.featured,
            ListingKind::Category => &b.category_apps,
            ListingKind::Collection => &b.collection_apps,
        };
        (l.rows.clone(), l.loading, l.error.clone(), l.loaded, l.next.is_some())
    };
    if loading_now && rows.is_empty() {
        loading(app, ui);
    }
    if let Some(error) = error {
        if error_line(app, ui, &error) {
            app.model.store.retry_listing(kind);
        }
    }
    if loaded && rows.is_empty() {
        ui.label(widgets::muted(&p, empty));
    }
    if rows.is_empty() {
        return;
    }
    let mut more = false;
    widgets::card(ui, &p, |ui| {
        let count = rows.len();
        for (i, row) in rows.iter().enumerate() {
            let response = result_row(app, ui, row, None);
            if response.clicked() && !row.ghost {
                app.model.store.open(row);
            }
            // Near the end and on screen: fetch the next page.
            if has_next && i + 5 >= count && ui.is_rect_visible(response.rect) {
                more = true;
            }
        }
        if loading_now {
            loading(app, ui);
        }
    });
    if more && !loading_now {
        app.model.store.more(kind);
    }
}

fn featured_tab(app: &mut App, ui: &mut Ui) {
    ui.label(widgets::muted(
        &app.palette,
        "Hand-picked by the archive. Whether a version runs on this device is checked when you open an app.",
    ));
    ui.add_space(8.0);
    listing(app, ui, ListingKind::Featured, "No apps found.");
}

fn charts_tab(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    ui.horizontal_wrapped(|ui| {
        for (slug, title) in CHART_TYPES {
            let selected = app.model.store.browser.chart_type == *slug;
            if widgets::chip(ui, &p, selected, title).clicked() && !selected {
                app.model.store.pick_chart_type(slug);
            }
        }
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        for (device, title) in [("iphone", "iPhone"), ("ipad", "iPad")] {
            let selected = app.model.store.chart_device() == device;
            if widgets::chip(ui, &p, selected, title).clicked() && !selected {
                app.model.store.pick_chart_device(device);
            }
        }
        let label = app.model.store.browser.chart_genre.as_ref().map_or("All apps".to_string(), |g| g.name.clone());
        let menu = widgets::chip_menu(ui, &p, &label);
        egui::Popup::menu(&menu).show(|ui| {
            egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                if ui.button("All apps").clicked() {
                    app.model.store.pick_chart_genre(None);
                }
                for genre in app.model.store.browser.genres.clone().unwrap_or_default() {
                    if ui.button(&genre.name).clicked() {
                        app.model.store.pick_chart_genre(Some(genre));
                    }
                }
            });
        });
    });
    ui.add_space(6.0);
    let chart = app.model.store.browser.chart.clone();
    if let Some(chart) = &chart {
        // Which snapshot is showing, with the archived ones before and after it.
        let index = chart.dates.iter().position(|d| *d == chart.date);
        let busy = app.model.store.browser.chart_loading;
        ui.horizontal(|ui| {
            let earlier = index.is_some_and(|i| i > 0) && !busy;
            if widgets::icon_button_sized(ui, &p, icons::CARET_LEFT, "Earlier snapshot", 32.0, earlier).clicked() {
                app.model.store.shift_chart(-1);
            }
            let date = widgets::chip_menu(ui, &p, &snapshot_date(&chart.date));
            egui::Popup::menu(&date).show(|ui| {
                egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                    let mut years: Vec<&str> = chart.dates.iter().map(|d| &d[..4.min(d.len())]).collect();
                    years.dedup();
                    for year in years {
                        if ui.button(year).clicked() {
                            app.model.store.pick_chart_year(year);
                        }
                    }
                });
            });
            let later = index.is_some_and(|i| i + 1 < chart.dates.len()) && !busy;
            if widgets::icon_button_sized(ui, &p, icons::CARET_RIGHT, "Later snapshot", 32.0, later).clicked() {
                app.model.store.shift_chart(1);
            }
        });
        ui.add_space(4.0);
    }
    if app.model.store.browser.chart_loading {
        loading(app, ui);
    }
    if let Some(error) = app.model.store.browser.chart_error.clone() {
        if error_line(app, ui, &error) {
            app.model.store.reload_chart();
        }
    }
    let Some(chart) = chart else { return };
    widgets::card(ui, &p, |ui| {
        for row in &chart.rows {
            let shown = row.app.clone().unwrap_or_else(|| StoreApp {
                name: row.name.clone(),
                developer: row.developer.clone(),
                ghost: true,
                ..Default::default()
            });
            if result_row(app, ui, &shown, Some(row.position)).clicked() && !shown.ghost {
                app.model.store.open(&shown);
            }
        }
    });
    if chart.rows.iter().any(|r| r.app.is_none()) {
        ui.add_space(8.0);
        ui.label(widgets::muted(&p, "Titles in grey haven't been preserved in the archive yet.").size(12.5));
    }
}

/// "2010-06-29" as "29 Jun 2010".
fn snapshot_date(date: &str) -> String {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let parts: Vec<&str> = date.split('-').collect();
    match parts.as_slice() {
        [y, m, d] => match (m.parse::<usize>().ok().filter(|m| (1..=12).contains(m)), d.parse::<u32>()) {
            (Some(m), Ok(d)) => format!("{d} {} {y}", MONTHS[m - 1]),
            _ => date.to_string(),
        },
        _ => date.to_string(),
    }
}

/// A row that opens something (a category, a collection): its name, a note, and a count.
fn link_row(app: &App, ui: &mut Ui, title: &str, subtitle: Option<&str>, count: u64) -> egui::Response {
    let p = app.palette;
    let height = if subtitle.is_some() { 52.0 } else { 42.0 };
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(
            rect.expand2(vec2(8.0, 0.0)),
            CornerRadius::same(theme::radius::ROW),
            p.container_high,
        );
    }
    let painter = ui.painter();
    let y = if subtitle.is_some() { -8.0 } else { 0.0 };
    painter.text(
        rect.left_center() + vec2(4.0, y),
        Align2::LEFT_CENTER,
        title,
        FontId::new(14.5, theme::medium()),
        p.on_surface,
    );
    if let Some(subtitle) = subtitle {
        painter.text(
            rect.left_center() + vec2(4.0, 10.0),
            Align2::LEFT_CENTER,
            subtitle,
            FontId::new(12.5, egui::FontFamily::Proportional),
            p.on_surface_variant,
        );
    }
    painter.text(
        rect.right_center() - vec2(30.0, 0.0),
        Align2::RIGHT_CENTER,
        count.to_string(),
        FontId::new(12.5, egui::FontFamily::Proportional),
        p.on_surface_variant,
    );
    painter.text(
        rect.right_center() - vec2(8.0, 0.0),
        Align2::RIGHT_CENTER,
        icons::CARET_RIGHT,
        FontId::new(16.0, theme::icons()),
        p.on_surface_variant,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn section_header(app: &App, ui: &mut Ui, title: &str, subtitle: Option<&str>) {
    // Back is the header's arrow, which steps out of the category or collection first.
    ui.label(RichText::new(title).font(theme::title()));
    if let Some(subtitle) = subtitle {
        ui.label(widgets::muted(&app.palette, subtitle));
    }
    ui.add_space(6.0);
}

fn categories_tab(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    match app.model.store.browser.category.clone() {
        None => {
            let genres = app.model.store.browser.genres.clone();
            if let Some(error) = app.model.store.browser.genres_error.clone() {
                if error_line(app, ui, &error) {
                    app.model.store.load_genres();
                }
            } else if genres.is_none() {
                loading(app, ui);
            }
            let Some(genres) = genres else { return };
            widgets::card(ui, &p, |ui| {
                for genre in genres {
                    if link_row(app, ui, &genre.name, None, genre.count).clicked() {
                        app.model.store.open_category(genre);
                    }
                }
            });
        }
        Some(category) => {
            let total = app.model.store.browser.category_apps.total;
            let count = (total > 0).then(|| format!("{total} apps"));
            section_header(app, ui, &category.name, count.as_deref());
            ui.horizontal_wrapped(|ui| {
                for (sort, title) in CATEGORY_SORTS {
                    let selected = app.model.store.browser.category_sort == *sort;
                    if widgets::chip(ui, &p, selected, title).clicked() && !selected {
                        app.model.store.pick_category_sort(sort);
                    }
                }
            });
            ui.add_space(8.0);
            listing(app, ui, ListingKind::Category, "No apps found.");
        }
    }
}

fn collections_tab(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    match app.model.store.browser.collection.clone() {
        None => {
            let list = app.model.store.browser.collections.clone();
            if let Some(error) = app.model.store.browser.collections_error.clone() {
                if error_line(app, ui, &error) {
                    app.model.store.load_collections();
                }
            } else if list.is_none() {
                loading(app, ui);
            }
            let Some(list) = list else { return };
            widgets::card(ui, &p, |ui| {
                for item in list {
                    if link_row(app, ui, &item.title, item.subtitle.as_deref(), item.count).clicked() {
                        app.model.store.open_collection(item);
                    }
                }
            });
        }
        Some(collection) => {
            section_header(app, ui, &collection.title, collection.subtitle.as_deref());
            listing(app, ui, ListingKind::Collection, "No apps found.");
        }
    }
}

fn installed_tab(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    let store = &app.model.store;
    let list = store.installed_apps.list.clone();
    if !store.running() {
        return empty_state(
            app,
            ui,
            "Start the device to see its apps",
            "The apps on a device are read from it while it runs.",
        );
    }
    if let Some(problem) = store.usb_problem.clone() {
        return empty_state(app, ui, "Not available", &problem);
    }
    if let (Some(error), None) = (store.installed_apps.error.clone(), &list) {
        empty_state(app, ui, "Couldn't read this device's apps", &error);
        ui.vertical_centered(|ui| {
            if widgets::text_button(ui, &p, "Try again").clicked() {
                app.model.store.refresh_installed(true);
            }
        });
        return;
    }
    let Some(list) = list else {
        loading(app, ui);
        ui.label(widgets::muted(&p, "Reading the apps on this device…"));
        return;
    };
    if list.is_empty() {
        return empty_state(
            app,
            ui,
            "No apps added yet",
            "Apps you install from the Legacy Store or from an IPA file show up here.",
        );
    }
    if let Some(error) = app.model.store.installed_apps.lookup_error.clone() {
        if error_line(app, ui, &format!("Couldn't check for newer versions. {error}")) {
            app.model.store.look_up();
        }
    }
    widgets::card(ui, &p, |ui| {
        for item in &list {
            if installed_row(app, ui, item).clicked() {
                app.model.store.open_installed(item);
            }
        }
    });
}

fn installed_row(app: &mut App, ui: &mut Ui, item: &InstalledApp) -> egui::Response {
    let p = app.palette;
    let store = &app.model.store;
    let icon = store.installed_apps.known(&item.bundle_id).and_then(|k| k.app.icon_url.clone());
    let update = store.installed_apps.update(item);
    let queued = store.queued_bundle(&item.bundle_id).filter(|q| q.active()).cloned();
    let title = app_title(item).to_string();
    let detail = app_version(item).map_or(item.bundle_id.clone(), |v| format!("Version {v}"));
    let tag = match (&queued, &update) {
        (Some(q), _) => Some(if q.state == QueueState::Pending { "In the queue".to_string() } else { q.phase.clone() }),
        (None, Some(update)) => Some(format!("Version {update} is available")),
        _ => None,
    };
    row_frame(app, ui, |app, ui| {
        app_icon(app, ui, icon.as_deref(), Some(&title), 46.0);
        ui.add_space(12.0);
        ui.vertical(|ui| {
            ui.add_space(4.0);
            ui.add(egui::Label::new(RichText::new(&title).family(theme::medium()).size(14.5)).truncate());
            ui.add(egui::Label::new(widgets::muted(&p, detail).size(12.5)).truncate());
            if let Some(tag) = &tag {
                ui.add(egui::Label::new(RichText::new(tag).color(p.primary).size(12.5)).truncate());
            }
        });
    })
}

/// An installed app the Legacy Store has no page for: what the device itself says about it.
fn plain_page(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    let Some(item) = app.model.store.installed_apps.plain.clone() else { return };
    let title = app_title(&item).to_string();
    ui.horizontal(|ui| {
        app_icon(app, ui, None, Some(&title), 72.0);
        ui.add_space(12.0);
        ui.vertical(|ui| {
            ui.add_space(10.0);
            ui.add(egui::Label::new(RichText::new(&title).font(theme::title()).size(19.0)).wrap());
            if let Some(version) = app_version(&item) {
                ui.label(widgets::muted(&p, format!("Version {version}")));
            }
        });
    });
    ui.add_space(12.0);
    widgets::card(ui, &p, |ui| {
        ui.label(widgets::muted(&p, "Identifier").size(12.5));
        ui.label(&item.bundle_id);
        if let (Some(short), Some(build)) = (&item.short_version, &item.version) {
            if short != build {
                ui.add_space(10.0);
                ui.label(widgets::muted(&p, "Build").size(12.5));
                ui.label(build);
            }
        }
    });
    ui.add_space(12.0);
    let installed = &app.model.store.installed_apps;
    let lookup = installed.lookups.get(&item.bundle_id);
    let (text, failed) = match (lookup, &installed.lookup_error) {
        (Some(Lookup::Missing), _) => (
            "The Legacy Store doesn't have this app, so there is no page for it and no newer version to offer."
                .to_string(),
            false,
        ),
        (None, Some(error)) => (format!("Couldn't check the Legacy Store: {error}"), true),
        _ => ("Looking for this app in the Legacy Store…".to_string(), false),
    };
    ui.add(
        egui::Label::new(RichText::new(text).size(13.0).color(if failed { p.error } else { p.on_surface_variant }))
            .wrap(),
    );
    if failed && widgets::text_button(ui, &p, "Try again").clicked() {
        app.model.store.look_up();
    }
}

/// An app icon, or (with no icon to be had) its initial, so the row still has something to tap.
fn app_icon(app: &mut App, ui: &mut Ui, url: Option<&str>, fallback: Option<&str>, size: f32) {
    let p = app.palette;
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    let radius = CornerRadius::same((size / 4.5) as u8);
    ui.painter().rect_filled(rect, radius, p.container_highest);
    match url {
        Some(url) => {
            if let Icon::Ready(bytes) = app.model.store.icon(url) {
                egui::Image::from_bytes(format!("bytes://store-icon/{url}"), egui::load::Bytes::Shared(bytes))
                    .corner_radius(radius)
                    .paint_at(ui, rect);
            }
        }
        None => {
            if let Some(initial) = fallback.and_then(|f| f.trim().chars().next()) {
                ui.painter().text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    initial.to_uppercase().to_string(),
                    FontId::new(size * 0.42, theme::semibold()),
                    p.on_surface_variant,
                );
            }
        }
    }
}

/// A clickable list row of the store's height, highlighted under the pointer.
fn row_frame(app: &mut App, ui: &mut Ui, add: impl FnOnce(&mut App, &mut Ui)) -> egui::Response {
    let p = app.palette;
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 64.0), Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(
            rect.expand2(vec2(8.0, 0.0)),
            CornerRadius::same(theme::radius::ROW),
            p.container_high,
        );
    }
    let inner = egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x - 30.0, rect.max.y));
    let mut child =
        ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::left_to_right(egui::Align::Center)));
    add(app, &mut child);
    ui.painter().text(
        rect.right_center() - vec2(8.0, 0.0),
        Align2::RIGHT_CENTER,
        icons::CARET_RIGHT,
        FontId::new(16.0, theme::icons()),
        p.on_surface_variant,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// One app in a list. `rank` is its chart position. Rows from the plain listings have no compatibility verdict
/// until opened; ghosts (not in the archive) are greyed out.
fn result_row(app: &mut App, ui: &mut Ui, row: &StoreApp, rank: Option<u32>) -> egui::Response {
    let p = app.palette;
    if row.ghost {
        let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 48.0), Sense::hover());
        let painter = ui.painter();
        let x = rect.left() + if rank.is_some() { 40.0 } else { 4.0 };
        if let Some(rank) = rank {
            painter.text(
                rect.left_center() + vec2(16.0, 0.0),
                Align2::CENTER_CENTER,
                rank.to_string(),
                FontId::new(13.5, theme::medium()),
                p.on_surface_variant,
            );
        }
        painter.text(
            egui::pos2(x, rect.center().y - 8.0),
            Align2::LEFT_CENTER,
            &row.name,
            FontId::new(14.0, theme::medium()),
            p.on_surface_variant.gamma_multiply(0.7),
        );
        painter.text(
            egui::pos2(x, rect.center().y + 9.0),
            Align2::LEFT_CENTER,
            "Not in the archive",
            FontId::new(12.0, egui::FontFamily::Proportional),
            p.on_surface_variant.gamma_multiply(0.6),
        );
        return response;
    }
    let problem = row.incompatibility();
    let installed = app.model.store.installed.get(&row.bundle_id).cloned();
    let queued = app.model.store.queued(row).filter(|q| q.active()).cloned();
    let tag = match (&problem, &queued, &installed) {
        (Some(problem), _, _) => Some((problem.clone(), p.error)),
        (None, Some(q), _) => {
            Some((if q.state == QueueState::Pending { "In the queue".to_string() } else { q.phase.clone() }, p.primary))
        }
        (None, None, Some(version)) => Some((format!("Installed {version}"), p.primary)),
        _ => None,
    };
    let details: Vec<String> = [
        row.developer.clone(),
        row.version.clone(),
        row.label.clone(),
        row.size.filter(|s| *s > 0).map(byte_size),
        row.version_count
            .filter(|n| *n > 0 && row.version.is_none())
            .map(|n| if n == 1 { "1 version".to_string() } else { format!("{n} versions") }),
    ]
    .into_iter()
    .flatten()
    .collect();
    row_frame(app, ui, |app, ui| {
        if let Some(rank) = rank {
            let (rect, _) = ui.allocate_exact_size(vec2(32.0, 20.0), Sense::hover());
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                rank.to_string(),
                FontId::new(13.5, theme::medium()),
                p.on_surface,
            );
            ui.add_space(4.0);
        }
        ui.scope(|ui| {
            ui.multiply_opacity(if problem.is_some() { 0.55 } else { 1.0 });
            app_icon(app, ui, row.icon_url.as_deref(), Some(&row.name), 46.0);
        });
        ui.add_space(12.0);
        ui.vertical(|ui| {
            ui.add_space(if tag.is_some() { 4.0 } else { 11.0 });
            let name = RichText::new(&row.name).family(theme::medium()).size(14.5);
            let name = if problem.is_some() { name.color(p.on_surface_variant) } else { name };
            ui.add(egui::Label::new(name).truncate());
            ui.add(egui::Label::new(widgets::muted(&p, details.join(" · ")).size(12.5)).truncate());
            if let Some((text, color)) = &tag {
                ui.add(egui::Label::new(RichText::new(text).color(*color).size(12.5)).truncate());
            }
        });
    })
}

fn details(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    let Some(chosen) = app.model.store.chosen.clone() else { return };
    let installed = app.model.store.installed.get(&chosen.bundle_id).cloned();
    let info = app.model.store.info.clone().unwrap_or_default();
    ui.horizontal(|ui| {
        app_icon(app, ui, chosen.icon_url.as_deref(), Some(&chosen.name), 72.0);
        ui.add_space(12.0);
        ui.vertical(|ui| {
            ui.add_space(2.0);
            ui.add(egui::Label::new(RichText::new(&chosen.name).font(theme::title()).size(19.0)).wrap());
            if let Some(developer) = chosen.developer.clone().or(info.developer.clone()) {
                ui.label(widgets::muted(&p, developer));
            }
            let facts: Vec<String> = [info.genre.clone(), info.released.clone()].into_iter().flatten().collect();
            if !facts.is_empty() {
                ui.label(widgets::muted(&p, facts.join(" · ")).size(12.5));
            }
            if let Some(version) = &installed {
                ui.label(RichText::new(format!("Installed {version}")).color(p.primary).size(12.5));
            }
        });
    });
    screenshots(app, ui);
    ui.add_space(10.0);
    let store = &app.model.store;
    let queued = store.queued(&chosen).filter(|q| q.active()).is_some();
    let ready = store.candidate.is_some() && store.guest_ready() && !queued;
    let waiting = store.running() && !store.guest_ready() && !queued;
    ui.horizontal(|ui| {
        let label = if queued {
            "In the queue"
        } else if waiting {
            "Waiting for iOS…"
        } else if installed.is_some() {
            "Reinstall"
        } else {
            "Install"
        };
        if widgets::primary_enabled(ui, &p, label, ready).clicked() {
            app.model.store.install_candidate();
        }
        let store = &app.model.store;
        let selected = store.copies.iter().flatten().find(|c| Some(c.ipa_id) == store.selected);
        if let Some(problem) = store.problem.clone() {
            ui.add(egui::Label::new(RichText::new(problem).color(p.error)).wrap());
        } else if store.selected.is_some() && store.candidate.is_none() {
            widgets::spinner(ui, &p, 18.0);
            ui.label(widgets::muted(&p, "Checking this version…"));
        } else if let Some(copy) = selected {
            let size = copy.size.map(|s| format!(" · {}", byte_size(s))).unwrap_or_default();
            ui.label(widgets::muted(&p, format!("Version {}{size}", copy.version)));
        }
    });
    let store = &app.model.store;
    let chosen_version =
        store.copies.iter().flatten().find(|c| Some(c.ipa_id) == store.selected).map(|c| c.version.clone());
    if let (Some(have), Some(want)) = (&installed, &chosen_version) {
        if store::older(want, have) {
            ui.label(
                widgets::muted(&p, format!("Version {have} is installed. An older version may not read its data."))
                    .size(12.5),
            );
        }
    }
    if !store.running() {
        ui.label(widgets::muted(&p, "Start the device to install.").size(12.5));
    }
    widgets::section(ui, &p, "Versions that run on this device");
    match app.model.store.copies.clone() {
        None => {
            ui.horizontal(|ui| {
                widgets::spinner(ui, &p, 18.0);
                ui.label(widgets::muted(&p, "Loading versions…"));
            });
        }
        Some(copies) if copies.is_empty() => {
            ui.label(widgets::muted(&p, "None found."));
        }
        Some(copies) => {
            widgets::card(ui, &p, |ui| {
                for (i, copy) in copies.iter().enumerate() {
                    let twin = copies.iter().enumerate().any(|(j, c)| j != i && c.version == copy.version);
                    let selected = app.model.store.selected == Some(copy.ipa_id);
                    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::click());
                    if selected || response.hovered() {
                        let fill = if selected { p.primary_container } else { p.container_high };
                        ui.painter().rect_filled(
                            rect.expand2(vec2(8.0, 0.0)),
                            CornerRadius::same(theme::radius::ROW),
                            fill,
                        );
                    }
                    let painter = ui.painter();
                    let title =
                        if twin { format!("{} · copy {}", copy.version, copy.ipa_id) } else { copy.version.clone() };
                    let subtitle: Vec<String> =
                        [copy.size.map(byte_size), copy.minimum_os.as_ref().map(|m| format!("iOS {m} or later"))]
                            .into_iter()
                            .flatten()
                            .collect();
                    let text_color = if selected { p.on_primary_container } else { p.on_surface };
                    painter.text(
                        rect.left_center() + vec2(36.0, -8.0),
                        Align2::LEFT_CENTER,
                        title,
                        FontId::new(14.5, theme::medium()),
                        text_color,
                    );
                    painter.text(
                        rect.left_center() + vec2(36.0, 10.0),
                        Align2::LEFT_CENTER,
                        subtitle.join(" · "),
                        FontId::new(12.0, egui::FontFamily::Proportional),
                        if selected { p.on_primary_container } else { p.on_surface_variant },
                    );
                    let mark = if selected { icons::CHECK_CIRCLE } else { icons::CIRCLE };
                    painter.text(
                        rect.left_center() + vec2(12.0, 0.0),
                        Align2::CENTER_CENTER,
                        mark,
                        FontId::new(18.0, theme::icons()),
                        if selected { p.primary } else { p.outline },
                    );
                    if response.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() && !selected {
                        app.model.store.choose(copy.ipa_id);
                    }
                }
            });
        }
    }
}

/// The newest archived screenshots, in a strip that scrolls sideways.
fn screenshots(app: &mut App, ui: &mut Ui) {
    let shots = app.model.store.shots.clone();
    if shots.is_empty() {
        return;
    }
    ui.add_space(12.0);
    let p = app.palette;
    egui::ScrollArea::horizontal().id_salt("store-shots").show(ui, |ui| {
        ui.horizontal(|ui| {
            for shot in &shots {
                let ratio =
                    if shot.width > 0 && shot.height > 0 { shot.width as f32 / shot.height as f32 } else { 0.6 };
                let height = 220.0;
                let (rect, _) = ui.allocate_exact_size(vec2(height * ratio, height), Sense::hover());
                let radius = CornerRadius::same(10);
                ui.painter().rect_filled(rect, radius, p.container_highest);
                if ui.is_rect_visible(rect) {
                    if let Icon::Ready(bytes) = app.model.store.icon(&shot.url) {
                        egui::Image::from_bytes(
                            format!("bytes://store-shot/{}", shot.url),
                            egui::load::Bytes::Shared(bytes),
                        )
                        .corner_radius(radius)
                        .paint_at(ui, rect);
                    }
                }
            }
        });
    });
}
