//! App settings, saved as JSON in the data folder. Mirrors the Android app's settings where they apply.
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    #[default]
    System,
    Dark,
    Light,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Scaling {
    /// Linear filtering.
    #[default]
    Smooth,
    /// Nearest-neighbour: crisp pixels.
    Sharp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OverlayLevel {
    #[default]
    Off,
    Fps,
    Basic,
    Full,
}

impl OverlayLevel {
    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::Fps,
            Self::Fps => Self::Basic,
            Self::Basic => Self::Full,
            Self::Full => Self::Off,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Fps => "FPS",
            Self::Basic => "Basic",
            Self::Full => "Full",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OverlayGraph {
    #[default]
    FrameTime,
    FrameRate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Corner {
    #[default]
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: Theme,
    /// The arrow keys tilt the device (its accelerometer, and gyroscope where it has one).
    pub tilt: bool,
    /// Tell iOS headphones are plugged in (skips speaker protection; read at start).
    pub headphones: bool,
    pub muted: bool,
    pub scaling: Scaling,
    /// Draw the device around the screen on the home view.
    pub device_frame: bool,
    /// Pause the guest while the window is minimized, and resume when it comes back.
    pub pause_when_hidden: bool,
    pub overlay: OverlayLevel,
    pub overlay_graph: OverlayGraph,
    pub overlay_corner: Corner,
    pub overlay_opacity: f32,
    pub overlay_in_device_view: bool,
    /// Show the frame inspector button.
    pub inspect_button: bool,
    pub detailed_timing: bool,
    /// Settings → Network. Off by default; a section that doesn't parse or validate is off.
    #[serde(deserialize_with = "crate::network::lenient")]
    pub network: crate::network::NetworkSettings,
    /// How the device is turned on the home view and in full screen.
    pub rotation: crate::rotation::Rotation,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: Theme::System,
            tilt: true,
            headphones: true,
            muted: false,
            scaling: Scaling::Smooth,
            device_frame: true,
            pause_when_hidden: true,
            overlay: OverlayLevel::Off,
            overlay_graph: OverlayGraph::FrameTime,
            overlay_corner: Corner::TopLeft,
            overlay_opacity: 0.55,
            overlay_in_device_view: true,
            inspect_button: false,
            detailed_timing: false,
            network: Default::default(),
            rotation: crate::rotation::Rotation::Portrait,
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Self {
        let mut settings: Self =
            std::fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default();
        settings.network = settings.network.sanitized(crate::network::Day::today());
        settings
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let partial = path.with_extension("json.partial");
        std::fs::write(&partial, serde_json::to_vec_pretty(self).unwrap())?;
        std::fs::rename(partial, path)
    }
}
