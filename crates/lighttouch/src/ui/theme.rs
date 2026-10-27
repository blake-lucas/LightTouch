//! Design tokens from the Android app's Compose theme (Theme.kt): one palette per mode, Inter for text, Phosphor
//! for icons, and the radii/spacing the screens use. Everything visual reads from here.
use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Stroke, TextStyle, Visuals,
};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub dark: bool,
    pub primary: Color32,
    pub on_primary: Color32,
    pub primary_container: Color32,
    pub on_primary_container: Color32,
    pub background: Color32,
    pub on_surface: Color32,
    pub on_surface_variant: Color32,
    pub surface_variant: Color32,
    /// Containers, lowest to highest.
    pub container_lowest: Color32,
    pub container_low: Color32,
    pub container: Color32,
    pub container_high: Color32,
    pub container_highest: Color32,
    pub outline: Color32,
    pub outline_variant: Color32,
    pub error: Color32,
    pub error_container: Color32,
    pub on_error_container: Color32,
}

/// Shared accents (LtColors).
pub const ACCENT: Color32 = Color32::from_rgb(0x71, 0xDC, 0xC8);
pub const DANGER: Color32 = Color32::from_rgb(0xFF, 0x6B, 0x6B);
pub const WARNING: Color32 = Color32::from_rgb(0xFF, 0xC5, 0x6B);
pub const PAUSED: Color32 = Color32::from_rgb(0x9A, 0xA2, 0xA6);
/// Overlay hues, one per subsystem (MangoHud's convention).
pub const FPS: Color32 = Color32::from_rgb(0xFF, 0xB8, 0x6B);
pub const CPU: Color32 = Color32::from_rgb(0x6B, 0xB8, 0xFF);
pub const GPU: Color32 = Color32::from_rgb(0x7C, 0xE3, 0x8B);
pub const MEM: Color32 = Color32::from_rgb(0xD5, 0x9B, 0xFF);
pub const LABEL: Color32 = Color32::from_rgb(0xB8, 0xBC, 0xC4);

pub const DARK: Palette = Palette {
    dark: true,
    primary: Color32::from_rgb(0x71, 0xDC, 0xC8),
    on_primary: Color32::from_rgb(0x00, 0x38, 0x2F),
    primary_container: Color32::from_rgb(0x12, 0x48, 0x3F),
    on_primary_container: Color32::from_rgb(0xB6, 0xF3, 0xE6),
    background: Color32::from_rgb(0x0D, 0x0F, 0x10),
    on_surface: Color32::from_rgb(0xE4, 0xE6, 0xE7),
    on_surface_variant: Color32::from_rgb(0x9A, 0xA2, 0xA6),
    surface_variant: Color32::from_rgb(0x22, 0x27, 0x2A),
    container_lowest: Color32::from_rgb(0x08, 0x09, 0x0A),
    container_low: Color32::from_rgb(0x14, 0x17, 0x18),
    container: Color32::from_rgb(0x18, 0x1B, 0x1D),
    container_high: Color32::from_rgb(0x1F, 0x23, 0x26),
    container_highest: Color32::from_rgb(0x28, 0x2D, 0x30),
    outline: Color32::from_rgb(0x3A, 0x41, 0x45),
    outline_variant: Color32::from_rgb(0x2A, 0x30, 0x33),
    error: Color32::from_rgb(0xFF, 0x6B, 0x6B),
    error_container: Color32::from_rgb(0x4A, 0x16, 0x16),
    on_error_container: Color32::from_rgb(0xFF, 0xDA, 0xD6),
};

pub const LIGHT: Palette = Palette {
    dark: false,
    primary: Color32::from_rgb(0x0F, 0x7F, 0x6F),
    on_primary: Color32::WHITE,
    primary_container: Color32::from_rgb(0xC9, 0xF1, 0xE8),
    on_primary_container: Color32::from_rgb(0x00, 0x20, 0x1B),
    background: Color32::from_rgb(0xF3, 0xF5, 0xF5),
    on_surface: Color32::from_rgb(0x15, 0x18, 0x19),
    on_surface_variant: Color32::from_rgb(0x5A, 0x63, 0x66),
    surface_variant: Color32::from_rgb(0xE1, 0xE6, 0xE7),
    container_lowest: Color32::WHITE,
    container_low: Color32::from_rgb(0xF8, 0xFA, 0xFA),
    container: Color32::WHITE,
    container_high: Color32::from_rgb(0xED, 0xF0, 0xF0),
    container_highest: Color32::from_rgb(0xE5, 0xE9, 0xE9),
    outline: Color32::from_rgb(0xC3, 0xCA, 0xCC),
    outline_variant: Color32::from_rgb(0xD9, 0xDE, 0xDF),
    error: Color32::from_rgb(0xC6, 0x28, 0x28),
    error_container: Color32::from_rgb(0xFF, 0xDA, 0xD6),
    on_error_container: Color32::from_rgb(0x41, 0x00, 0x02),
};

/// Corner radii.
pub mod radius {
    pub const SMALL: u8 = 10;
    pub const ROW: u8 = 14;
    pub const CARD: u8 = 20;
    pub const DOCK: u8 = 24;
    pub const PILL: u8 = 28;
}

/// Font families beyond egui's two built-ins.
pub fn medium() -> FontFamily {
    FontFamily::Name("medium".into())
}
pub fn semibold() -> FontFamily {
    FontFamily::Name("semibold".into())
}
pub fn icons() -> FontFamily {
    FontFamily::Name("icons".into())
}
pub fn icons_fill() -> FontFamily {
    FontFamily::Name("icons-fill".into())
}

pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let mut add = |name: &str, bytes: &'static [u8]| {
        fonts.font_data.insert(name.to_owned(), Arc::new(FontData::from_static(bytes)));
    };
    add("inter", include_bytes!("../../assets/fonts/Inter-Regular.ttf"));
    add("inter-medium", include_bytes!("../../assets/fonts/Inter-Medium.ttf"));
    add("inter-semibold", include_bytes!("../../assets/fonts/Inter-SemiBold.ttf"));
    add("phosphor", include_bytes!("../../assets/fonts/Phosphor.ttf"));
    add("phosphor-fill", include_bytes!("../../assets/fonts/Phosphor-Fill.ttf"));
    let fallback: Vec<String> = fonts.families[&FontFamily::Proportional].clone();
    let family = |primary: &str| -> Vec<String> {
        let mut list = vec![primary.to_string()];
        list.extend(fallback.iter().cloned());
        list
    };
    fonts.families.insert(FontFamily::Proportional, family("inter"));
    fonts.families.insert(medium(), family("inter-medium"));
    fonts.families.insert(semibold(), family("inter-semibold"));
    fonts.families.insert(icons(), vec!["phosphor".into()]);
    fonts.families.insert(icons_fill(), vec!["phosphor-fill".into()]);
    ctx.set_fonts(fonts);
}

/// Fonts used across screens beyond egui's built-in text styles.
pub fn title() -> FontId {
    FontId::new(16.0, semibold())
}
pub fn headline() -> FontId {
    FontId::new(26.0, semibold())
}
pub fn label() -> FontId {
    FontId::new(12.0, medium())
}

pub fn apply(ctx: &egui::Context, palette: &Palette) {
    let mut style = (*ctx.global_style()).clone();
    style.text_styles = [
        (TextStyle::Small, FontId::new(12.0, FontFamily::Proportional)),
        (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
        (TextStyle::Button, FontId::new(14.0, medium())),
        (TextStyle::Heading, FontId::new(20.0, semibold())),
        (TextStyle::Monospace, FontId::new(12.0, FontFamily::Monospace)),
    ]
    .into();
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.button_padding = egui::vec2(16.0, 8.0);
    style.spacing.interact_size.y = 36.0;
    style.spacing.window_margin = Margin::same(20);
    style.spacing.menu_margin = Margin::same(6);
    style.spacing.scroll.floating = true;
    style.interaction.selectable_labels = false;

    let mut v = if palette.dark { Visuals::dark() } else { Visuals::light() };
    v.override_text_color = Some(palette.on_surface);
    v.panel_fill = palette.background;
    v.window_fill = palette.container;
    v.window_stroke = Stroke::new(1.0, palette.outline_variant);
    v.window_corner_radius = CornerRadius::same(radius::CARD);
    v.menu_corner_radius = CornerRadius::same(radius::ROW);
    v.extreme_bg_color = palette.container_highest;
    v.faint_bg_color = palette.container_low;
    v.code_bg_color = palette.container_high;
    v.hyperlink_color = palette.primary;
    v.selection.bg_fill = palette.primary.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, palette.primary);
    v.window_shadow =
        egui::epaint::Shadow { offset: [0, 8], blur: 28, spread: 0, color: Color32::from_black_alpha(90) };
    v.popup_shadow = egui::epaint::Shadow { offset: [0, 6], blur: 18, spread: 0, color: Color32::from_black_alpha(70) };
    v.slider_trailing_fill = true;

    let corner = CornerRadius::same(radius::SMALL);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = palette.container;
    w.noninteractive.weak_bg_fill = palette.container;
    w.noninteractive.bg_stroke = Stroke::new(1.0, palette.outline_variant);
    w.noninteractive.fg_stroke = Stroke::new(1.0, palette.on_surface);
    w.noninteractive.corner_radius = corner;
    for (state, fill) in [
        (&mut w.inactive, palette.container_highest),
        (&mut w.hovered, palette.outline_variant),
        (&mut w.active, palette.outline),
        (&mut w.open, palette.container_highest),
    ] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.bg_stroke = Stroke::NONE;
        state.fg_stroke = Stroke::new(1.5, palette.on_surface);
        state.corner_radius = corner;
        state.expansion = 0.0;
    }
    w.hovered.bg_stroke = Stroke::new(1.0, palette.outline);
    style.visuals = v;
    // The palette already follows the system theme when asked to; give egui's dark and light slots the same style.
    ctx.set_theme(if palette.dark { egui::Theme::Dark } else { egui::Theme::Light });
    ctx.set_style_of(egui::Theme::Dark, style.clone());
    ctx.set_style_of(egui::Theme::Light, style);
}
