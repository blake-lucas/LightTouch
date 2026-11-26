//! Settings → Storage: which folder the devices are kept in, and the dialogs for changing it (move them, switch
//! to devices already there) or finding it missing (a drive that isn't connected).
use super::{App, Dialog, icons, theme, widgets};
use eframe::egui::{RichText, Ui};
use lighttouch_model::library_move::Choice;
use std::path::Path;

/// The Storage card's rows.
pub fn rows(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    let library = app.model.dirs.library();
    let missing = app.model.missing_library.is_some();
    let locked = app.model.library_locked();
    let mut subtitle =
        if missing { format!("Not found: {}", library.display()) } else { library.display().to_string() };
    if let Some(reason) = &locked {
        subtitle.push_str(&format!(". {reason} to change it"));
    }
    let mut open = false;
    let mut change = false;
    widgets::setting_row(ui, &p, icons::HARD_DRIVES, "Devices folder", &subtitle, |ui| {
        change = widgets::tonal_enabled(ui, &p, "Change", locked.is_none()).clicked();
        if !missing {
            open = widgets::tonal(ui, &p, "Open").clicked();
        }
    });
    let mut reset = false;
    if app.model.dirs.custom_library().is_some() {
        let default = app.model.dirs.default_library();
        widgets::setting_row(
            ui,
            &p,
            icons::FOLDER,
            "Default folder",
            &format!("Keep devices with LightTouch's settings again, in {}", default.display()),
            |ui| reset = widgets::tonal_enabled(ui, &p, "Use", locked.is_none()).clicked(),
        );
    }
    if open {
        super::settings::open_path(&library);
    }
    if change {
        choose(app);
    }
    if reset {
        let default = app.model.dirs.default_library();
        offer(app, &default);
    }
}

/// Ask for a folder, then what to do with it.
pub fn choose(app: &mut App) {
    if let Some(dir) = rfd::FileDialog::new().set_title("Choose a folder for your devices").pick_folder() {
        offer(app, &dir);
    }
}

/// What to do with `dir` as the devices' folder: the next dialog, or why not.
pub fn offer(app: &mut App, dir: &Path) {
    match app.model.library_choice(dir) {
        Ok(choice) => app.dialog = Some(Dialog::Library(choice)),
        Err(e) => app.model.error = Some(e),
    }
}

/// Off the welcome page once devices are found.
fn settle_page(app: &mut App) {
    if !app.model.devices.is_empty() && matches!(app.page, super::Page::Setup(_)) {
        app.page = super::Page::Device;
    }
}

/// A folder on a line of its own, so a long path doesn't bury the sentence around it.
fn path_line(ui: &mut Ui, p: &super::theme::Palette, path: &Path) {
    ui.add_space(4.0);
    ui.label(RichText::new(path.display().to_string()).size(13.0).color(p.on_surface));
    ui.add_space(4.0);
}

fn plural(n: usize) -> String {
    if n == 1 { "1 device".into() } else { format!("{n} devices") }
}

/// Move, start over in, or switch to the chosen folder. Whether the dialog is done.
pub fn choice_dialog(app: &mut App, ui: &mut Ui, choice: &Choice) -> bool {
    let p = app.palette;
    let current_missing = app.model.missing_library.is_some();
    let target = choice.target().to_path_buf();
    let mut close = false;
    ui.label(widgets::icon_text(icons::HARD_DRIVES, 26.0).color(p.primary));
    ui.add_space(6.0);
    match choice {
        Choice::New { devices, bytes, .. } if *devices > 0 && !current_missing => {
            ui.label(RichText::new("Move your devices?").font(theme::title()));
            ui.add_space(4.0);
            ui.label(widgets::muted(
                &p,
                format!(
                    "{} ({}) will be moved to:",
                    if *devices == 1 { "Your device".to_string() } else { format!("Your {devices} devices") },
                    super::transfer_size(*bytes)
                ),
            ));
            path_line(ui, &p, &target);
            ui.label(widgets::muted(&p, "New devices will be saved there too. Settings and logs aren't moved."));
            ui.add_space(6.0);
            ui.label(widgets::muted(
                &p,
                "If you don't move them, the new folder starts empty and your devices stay in the old one. You can switch back in Settings.",
            ));
            ui.add_space(16.0);
            widgets::dialog_buttons(ui, |ui| {
                if widgets::primary(ui, &p, "Move").clicked() {
                    app.model.move_library(target.clone());
                    close = true;
                }
                if widgets::text_button(ui, &p, "Don't move them").clicked() {
                    app.model.use_library(Some(target.clone()));
                    close = true;
                }
                if widgets::text_button(ui, &p, "Cancel").clicked() {
                    close = true;
                }
            });
        }
        Choice::New { .. } => {
            ui.label(RichText::new("Save devices in this folder?").font(theme::title()));
            ui.add_space(4.0);
            path_line(ui, &p, &target);
            ui.label(widgets::muted(&p, "New devices will be saved there. Settings and logs aren't moved."));
            ui.add_space(16.0);
            widgets::dialog_buttons(ui, |ui| {
                if widgets::primary(ui, &p, "Use this folder").clicked() {
                    app.model.move_library(target.clone());
                    close = true;
                }
                if widgets::text_button(ui, &p, "Cancel").clicked() {
                    close = true;
                }
            });
        }
        Choice::Existing { devices, .. } => {
            ui.label(RichText::new("Switch to these devices?").font(theme::title()));
            ui.add_space(4.0);
            ui.label(widgets::muted(&p, format!("This folder already has {} from LightTouch:", plural(*devices))));
            path_line(ui, &p, &target);
            let mut text = "LightTouch will use them from now on.".to_string();
            if !app.model.devices.is_empty() && !current_missing {
                text.push_str(" Your current devices stay in the old folder, and you can switch back in Settings.");
            }
            ui.label(widgets::muted(&p, text));
            ui.add_space(16.0);
            widgets::dialog_buttons(ui, |ui| {
                if widgets::primary(ui, &p, "Switch").clicked() {
                    app.model.use_library(Some(target.clone()));
                    settle_page(app);
                    close = true;
                }
                if widgets::text_button(ui, &p, "Cancel").clicked() {
                    close = true;
                }
            });
        }
    }
    close
}

/// The chosen folder isn't there. Whether the dialog is done.
pub fn missing_dialog(app: &mut App, ui: &mut Ui) -> bool {
    let p = app.palette;
    let Some(missing) = app.model.missing_library.clone() else { return true };
    let mut close = false;
    ui.label(widgets::icon_text(icons::WARNING, 26.0).color(theme::WARNING));
    ui.add_space(6.0);
    ui.label(RichText::new("Can't find your devices").font(theme::title()));
    ui.add_space(4.0);
    ui.label(widgets::muted(&p, "LightTouch saves your devices in:"));
    path_line(ui, &p, &missing);
    ui.label(widgets::muted(
        &p,
        "That folder isn't there. If it's on a drive that isn't connected, connect it and try again.",
    ));
    ui.add_space(16.0);
    widgets::dialog_buttons(ui, |ui| {
        if widgets::primary(ui, &p, "Try again").clicked() {
            app.model.retry_library();
            close = app.model.missing_library.is_none();
            settle_page(app);
        }
        if widgets::text_button(ui, &p, "Choose another").clicked() {
            close = true;
            choose(app);
        }
        if widgets::text_button(ui, &p, "Use default").clicked() {
            app.model.use_library(None);
            settle_page(app);
            close = true;
        }
    });
    close
}
