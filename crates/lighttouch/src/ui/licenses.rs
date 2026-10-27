//! Open source licenses: the notices bundled with each component (`resources/licenses`), like the Android app's
//! licence dialog. A group lists its files and links to its source; a file opens in place.
use super::{App, Page, icons, settings::open_path, theme, widgets};
use eframe::egui::{self, RichText, Ui};
use lighttouch_model::notices::{self, NoticeGroup};

#[derive(Default)]
pub struct LicenseState {
    /// Loaded when the page first shows.
    groups: Option<Result<Vec<NoticeGroup>, String>>,
    /// The open file: its group, its index, and its text or why it can't be shown.
    open: Option<(usize, usize, Result<String, String>)>,
}

pub fn show(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    let resources = app.model.resources.licenses();
    let state = &mut app.licenses;
    let groups = state.groups.get_or_insert_with(|| notices::load(&resources));
    let mut back = false;
    let mut choose = None;
    let reading = state.open.is_some(); // its own scroll position: a file opens at the top
    egui::ScrollArea::vertical().id_salt(("licenses", reading)).auto_shrink([false, false]).show(ui, |ui| {
        ui.add_space(18.0);
        let width = (ui.available_width() - 48.0).clamp(200.0, 720.0);
        let margin = ((ui.available_width() - width) / 2.0).max(24.0);
        egui::Frame::new().inner_margin(egui::Margin::symmetric(margin as i8, 0)).show(ui, |ui| {
            ui.set_max_width(width);
            if let (Some((group, file, text)), Ok(list)) = (&state.open, &*groups) {
                ui.horizontal(|ui| {
                    back = widgets::icon_button(ui, &p, icons::BACK, "All licenses").clicked();
                    ui.label(RichText::new(&list[*group].files[*file].path).font(theme::title()));
                });
                ui.add_space(10.0);
                widgets::card(ui, &p, |ui| match text {
                    Ok(text) => {
                        ui.label(RichText::new(text).monospace().size(12.0));
                    }
                    Err(error) => {
                        ui.label(RichText::new(error).color(theme::DANGER));
                    }
                });
                ui.add_space(28.0);
                return;
            }
            ui.horizontal(|ui| {
                back = widgets::icon_button(ui, &p, icons::BACK, "Back").clicked();
                ui.label(RichText::new("Open source licenses").font(theme::headline()));
            });
            let list = match &*groups {
                Ok(list) if !list.is_empty() => list,
                Ok(_) => {
                    ui.add_space(14.0);
                    ui.label(widgets::muted(&p, "This build includes no license notices."));
                    return;
                }
                Err(error) => {
                    ui.add_space(14.0);
                    ui.label(RichText::new(error).color(theme::DANGER));
                    return;
                }
            };
            for (g, group) in list.iter().enumerate() {
                widgets::section(ui, &p, &group.title);
                widgets::card(ui, &p, |ui| {
                    if let Some(f) = group.files.iter().position(|f| f.path == "NOTICE.txt") {
                        widgets::setting_row(ui, &p, icons::INFO, "What's included", "", |ui| {
                            if widgets::tonal(ui, &p, "Read").clicked() {
                                choose = Some((g, f));
                            }
                        });
                    }
                    let count = format!("{} license files", group.files.len());
                    widgets::setting_row(ui, &p, icons::FILE, &count, "", |ui| {
                        if widgets::tonal(ui, &p, "Source code").clicked() {
                            open_path(std::path::Path::new(&group.source));
                        }
                    });
                    egui::CollapsingHeader::new("Show files").id_salt(&group.folder).show(ui, |ui| {
                        for (f, file) in group.files.iter().enumerate() {
                            if ui.link(&file.path).clicked() {
                                choose = Some((g, f));
                            }
                        }
                    });
                });
            }
            ui.add_space(28.0);
        });
    });
    if let (Some((g, f)), Ok(list)) = (choose, &*groups) {
        let text = list[g].read(&list[g].files[f]);
        state.open = Some((g, f, text));
    } else if back {
        if state.open.take().is_none() {
            app.page = Page::Settings;
        }
    }
}
