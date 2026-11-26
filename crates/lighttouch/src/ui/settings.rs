//! Settings: grouped cards like the Android app's settings screen.
use super::{App, Dialog, Page, icons, overlay, theme, widgets};
use eframe::egui::{self, RichText, Ui};
use lighttouch_model::session::Phase;
use lighttouch_model::settings::{Corner, OverlayGraph, OverlayLevel, Theme};

pub fn show(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.add_space(18.0);
        let width = (ui.available_width() - 48.0).clamp(200.0, 640.0);
        let margin = ((ui.available_width() - width) / 2.0).max(24.0);
        egui::Frame::new().inner_margin(egui::Margin::symmetric(margin as i8, 0)).show(ui, |ui| {
            ui.set_max_width(width);
            ui.horizontal(|ui| {
                if widgets::icon_button(ui, &p, icons::BACK, "Back").clicked() {
                    app.page = Page::Device;
                }
                ui.label(RichText::new("Settings").font(theme::headline()));
            });
            let before = app.model.settings.clone();
            let recording = app.model.recording_status();
            let running = app.model.session.as_ref().is_some_and(|s| matches!(s.phase, Phase::Running));
            let mut toggle_recording = false;
            let exporting = app.model.exporting_logs();
            let mut export_logs = false;
            let mut open_scaling = false;
            let scaling_summary = super::screen_scaling::summary(app);
            let settings = &mut app.model.settings;

            widgets::section(ui, &p, "Display");
            widgets::card(ui, &p, |ui| {
                widgets::setting_row(ui, &p, icons::SUN, "Theme", "", |ui| {
                    widgets::segmented(
                        ui,
                        &p,
                        &mut settings.theme,
                        &[(Theme::System, "System"), (Theme::Dark, "Dark"), (Theme::Light, "Light")],
                    );
                });
                widgets::setting_row(ui, &p, icons::MONITOR, "Screen scaling", &scaling_summary, |ui| {
                    if widgets::tonal(ui, &p, "Open").clicked() {
                        open_scaling = true;
                    }
                });
                widgets::setting_row(
                    ui,
                    &p,
                    icons::PHONE,
                    "Show the device",
                    "Draw the iPod, iPhone or iPad around its screen",
                    |ui| {
                        widgets::switch(ui, &p, &mut settings.device_frame);
                    },
                );
                widgets::setting_row(
                    ui,
                    &p,
                    icons::FULLSCREEN,
                    "Show controls in full screen",
                    "Volume, Home, Sleep/Wake and More along the bottom. Without them, Esc or F11 leaves full screen",
                    |ui| {
                        widgets::switch(ui, &p, &mut settings.fullscreen_controls);
                    },
                );
            });

            widgets::section(ui, &p, "Controls");
            widgets::card(ui, &p, |ui| {
                #[cfg(target_os = "macos")]
                const MOUSE: &str = "Click to tap; Option-drag pinches or rotates, Option-Shift-drag moves two fingers. H is Home, P Sleep/Wake, + and - the volume, F11 full screen";
                #[cfg(not(target_os = "macos"))]
                const MOUSE: &str = "Click to tap; Alt-drag pinches or rotates, Alt+Shift-drag moves two fingers. H is Home, P Sleep/Wake, + and - the volume, F11 full screen";
                widgets::setting_row(ui, &p, icons::INFO, "Mouse and keyboard", MOUSE, |_| {});
                let mut open_controller = false;
                widgets::setting_row(
                    ui,
                    &p,
                    super::controller::GAME_CONTROLLER,
                    "Controllers and key bindings",
                    "Play with a game controller or keys: taps, on-screen sticks, a pointer, tilt and the controller's motion",
                    |ui| {
                        if widgets::tonal(ui, &p, "Open").clicked() {
                            open_controller = true;
                        }
                    },
                );
                if open_controller {
                    app.controller.open = true;
                }
                widgets::setting_row(
                    ui,
                    &p,
                    icons::KEYBOARD,
                    "Tilt with the arrow keys",
                    "Rocks the device toward the left or right arrow and tips it with up and down, for games you steer by tilting",
                    |ui| {
                        widgets::switch(ui, &p, &mut settings.tilt);
                    },
                );
                widgets::setting_row(
                    ui,
                    &p,
                    icons::CAMERA,
                    "Screenshot button",
                    "Adds Screenshot to the controls under the device. Full screen's More menu always has it",
                    |ui| {
                        widgets::switch(ui, &p, &mut settings.screenshot_button);
                    },
                );
            });

            widgets::section(ui, &p, "Audio");
            widgets::card(ui, &p, |ui| {
                widgets::setting_row(
                    ui,
                    &p,
                    icons::HEADPHONES,
                    "Headphones plugged in",
                    "Stereo sound, as through the headphone jack; off plays through the device's speaker. Applies the next time the device starts",
                    |ui| {
                        widgets::switch(ui, &p, &mut settings.headphones);
                        if widgets::icon_button(ui, &p, icons::INFO, "About this setting").clicked() {
                            app.dialog = Some(Dialog::Headphones);
                        }
                    },
                );
                widgets::setting_row(ui, &p, icons::VOLUME_UP, "Mute", "Silence the device without pausing it", |ui| {
                    widgets::switch(ui, &p, &mut settings.muted);
                });
                widgets::setting_row(
                    ui,
                    &p,
                    icons::MICROPHONE,
                    "Microphone",
                    "iOS apps can record from this computer's microphone. It is only on while an app is recording. iPhone 3GS, iPhone 4, iPod touch 4G and iPad",
                    |ui| {
                        widgets::switch(ui, &p, &mut settings.microphone);
                    },
                );
            });

            widgets::section(ui, &p, "Connection");
            let network = app.model.settings.network.summary();
            widgets::card(ui, &p, |ui| {
                widgets::setting_row(ui, &p, icons::GLOBE, "Network", &network, |ui| {
                    if widgets::tonal(ui, &p, "Open").clicked() {
                        app.page = Page::Network;
                    }
                });
                super::carrier::settings_row(app, ui);
            });
            let settings = &mut app.model.settings;

            // Pausing and how the emulator trades speed for fewer stalls.
            widgets::section(ui, &p, "Running");
            widgets::card(ui, &p, |ui| {
                widgets::setting_row(
                    ui,
                    &p,
                    icons::PAUSE,
                    "Pause when minimized",
                    "Resumes when the window comes back",
                    |ui| {
                        widgets::switch(ui, &p, &mut settings.pause_when_hidden);
                    },
                );
                widgets::setting_row(
                    ui,
                    &p,
                    icons::GAUGE,
                    "Fewer pauses when apps open",
                    "The emulator keeps its translated code when an iOS app opens instead of clearing it, so apps open with fewer stalls. Games may run up to about 5% slower. Applies the next time the device starts",
                    |ui| {
                        widgets::switch(ui, &p, &mut settings.fewer_app_pauses);
                    },
                );
            });


            widgets::section(ui, &p, "Performance overlay");
            widgets::card(ui, &p, |ui| {
                widgets::setting_row(ui, &p, icons::CHART, "Detail", "", |ui| {
                    widgets::segmented(
                        ui,
                        &p,
                        &mut settings.overlay,
                        &[
                            (OverlayLevel::Off, "Off"),
                            (OverlayLevel::Fps, "FPS"),
                            (OverlayLevel::Basic, "Basic"),
                            (OverlayLevel::Full, "Full"),
                        ],
                    );
                });
                if settings.overlay != OverlayLevel::Off {
                    egui::Frame::new().inner_margin(egui::Margin { left: 30, right: 4, top: 4, bottom: 8 }).show(ui, |ui| {
                        overlay::preview(ui, settings, app.model.session.as_ref());
                    });
                    widgets::setting_row(ui, &p, icons::CHART, "Graph", "", |ui| {
                        widgets::segmented(
                            ui,
                            &p,
                            &mut settings.overlay_graph,
                            &[(OverlayGraph::FrameTime, "Frame time"), (OverlayGraph::FrameRate, "Frame rate")],
                        );
                    });
                    widgets::setting_row(ui, &p, icons::MONITOR, "Position", "", |ui| {
                        widgets::segmented(
                            ui,
                            &p,
                            &mut settings.overlay_corner,
                            &[
                                (Corner::TopLeft, "↖"),
                                (Corner::TopRight, "↗"),
                                (Corner::BottomLeft, "↙"),
                                (Corner::BottomRight, "↘"),
                            ],
                        );
                    });
                    widgets::setting_row(ui, &p, icons::SUN, "Background", "", |ui| {
                        ui.add(egui::Slider::new(&mut settings.overlay_opacity, 0.0..=0.9).show_value(false));
                    });
                    widgets::setting_row(
                        ui,
                        &p,
                        icons::PHONE,
                        "Show in device view",
                        "Otherwise only in full screen",
                        |ui| {
                            widgets::switch(ui, &p, &mut settings.overlay_in_device_view);
                        },
                    );
                }
            });

            widgets::section(ui, &p, "Diagnostics");
            widgets::card(ui, &p, |ui| {
                widgets::setting_row(
                    ui,
                    &p,
                    icons::CHART,
                    "Inspect button",
                    "Adds the frame inspector to the controls",
                    |ui| {
                        widgets::switch(ui, &p, &mut settings.inspect_button);
                    },
                );
                widgets::setting_row(
                    ui,
                    &p,
                    icons::CPU,
                    "Detailed frame timing",
                    "Shows where each frame's time went in the frame inspector. Costs a few percent of speed.",
                    |ui| {
                        widgets::switch(ui, &p, &mut settings.detailed_timing);
                    },
                );
                if !settings.detailed_timing {
                    let status = &recording;
                    let (title, detail) = match status {
                        Some(s) => ("Stop profile recording", s.as_str()),
                        None => (
                            "Record a profile",
                            "Capture engine timing for the app on screen, for tools/perf/profile.py",
                        ),
                    };
                    widgets::setting_row(ui, &p, icons::CLOCK, title, detail, |ui| {
                        let label = if status.is_some() { "Stop" } else { "Record" };
                        if widgets::tonal_enabled(ui, &p, label, running || status.is_some()).clicked() {
                            toggle_recording = true;
                        }
                    });
                    if status.is_some() {
                        ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
                    }
                }
                widgets::setting_row(
                    ui,
                    &p,
                    icons::DOWNLOAD,
                    "Export logs",
                    "Saves LightTouch's logs and a summary of this computer as a zip, to attach to a problem report",
                    |ui| {
                        export_logs = widgets::tonal_enabled(
                            ui,
                            &p,
                            if exporting { "Saving…" } else { "Export" },
                            !exporting,
                        )
                        .clicked();
                    },
                );
                widgets::setting_row(ui, &p, icons::FILE, "Log folder", "Where LightTouch keeps its logs", |ui| {
                    if widgets::tonal(ui, &p, "Open folder").clicked() {
                        let dir = app.model.dirs.logs();
                        let _ = std::fs::create_dir_all(&dir);
                        open_path(&dir);
                    }
                });
            });

            let session_active = app.model.session_active();
            let selected = app.model.selected().cloned();
            widgets::section(ui, &p, "Device");
            widgets::card(ui, &p, |ui| {
                if let Some(device) = &selected {
                    widgets::setting_row(
                        ui,
                        &p,
                        icons::RECOVER,
                        "Recover retained state",
                        "Cold boot the selected device with its data as it was left",
                        |ui| {
                            if widgets::tonal_enabled(ui, &p, "Recover", !session_active).clicked() {
                                app.model.start(true);
                                app.page = Page::Device;
                            }
                        },
                    );
                    widgets::setting_row(
                        ui,
                        &p,
                        icons::PACKAGE,
                        "Update guest tools",
                        &format!("Install a newer .itpack into {}; it applies at the next start", device.name),
                        |ui| {
                            if widgets::tonal_enabled(ui, &p, "Choose", !session_active).clicked() {
                                if let Some(file) = rfd::FileDialog::new()
                                    .set_title("Choose guest tools")
                                    .add_filter("Guest tools", &["itpack"])
                                    .pick_file()
                                {
                                    app.model.update_guest_tools(&file);
                                }
                            }
                        },
                    );
                }
            });

            widgets::section(ui, &p, "Storage");
            widgets::card(ui, &p, |ui| super::storage::rows(app, ui));

            widgets::section(ui, &p, "About");
            widgets::card(ui, &p, |ui| {
                widgets::setting_row(
                    ui,
                    &p,
                    icons::INFO,
                    &format!("LightTouch {}", env!("CARGO_PKG_VERSION")),
                    "Free software under the GNU GPL, version 2 or later",
                    |ui| {
                        if widgets::tonal(ui, &p, "Source code").clicked() {
                            open_path(std::path::Path::new("https://github.com/blake-lucas/LightTouch"));
                        }
                    },
                );
                widgets::setting_row(
                    ui,
                    &p,
                    icons::FILE,
                    "Open source licenses",
                    "The components LightTouch includes and their licenses",
                    |ui| {
                        if widgets::tonal(ui, &p, "View").clicked() {
                            app.page = Page::Licenses;
                        }
                    },
                );
            });
            ui.add_space(28.0);

            if open_scaling {
                super::screen_scaling::open(app);
            }
            if export_logs {
                super::save_logs(app);
            }
            if toggle_recording {
                app.model.toggle_recording();
            }
            if app.model.settings != before {
                let muted_changed = app.model.settings.muted != before.muted;
                let microphone_changed = app.model.settings.microphone != before.microphone;
                app.model.save_settings();
                if muted_changed {
                    let muted = app.model.settings.muted;
                    app.model.set_muted(muted);
                }
                if microphone_changed {
                    let on = app.model.settings.microphone;
                    app.model.set_microphone(on);
                }
            }
        });
    });
}

/// Open a folder or link with the desktop's default handler.
pub fn open_path(path: &std::path::Path) {
    #[cfg(target_os = "linux")]
    let program = "xdg-open";
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(windows)]
    let program = "explorer";
    let _ = std::process::Command::new(program).arg(path).spawn();
}
