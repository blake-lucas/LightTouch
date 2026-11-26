//! App settings, saved as JSON in the data folder. Mirrors the Android app's settings where they apply.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    #[default]
    System,
    Dark,
    Light,
}

/// How the guest's pixels are drawn at the size they're shown (Settings → Screen scaling).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Scaling {
    /// Bilinear filtering.
    #[default]
    Smooth,
    /// Nearest neighbour: hard-edged pixels, some a screen pixel wider than others at most sizes.
    Sharp,
    /// Sharp-bilinear: a whole-number nearest-neighbour prescale, then bilinear to the size shown, so pixels stay
    /// crisp and even.
    Crisp,
    /// Integer scaling: the screen is shown at a whole multiple of its pixels (`integer_scale`), with borders.
    PixelPerfect,
}

impl Scaling {
    pub const ALL: [Scaling; 4] = [Scaling::Smooth, Scaling::Sharp, Scaling::Crisp, Scaling::PixelPerfect];

    pub fn label(self) -> &'static str {
        match self {
            Self::Smooth => "Smooth",
            Self::Sharp => "Sharp",
            Self::Crisp => "Crisp",
            Self::PixelPerfect => "Pixel perfect",
        }
    }

    /// The closest choice versions before 0.0.15 understand (they read `scaling`, smooth or sharp).
    fn legacy(self) -> LegacyScaling {
        if self == Self::Smooth { LegacyScaling::Smooth } else { LegacyScaling::Sharp }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
enum LegacyScaling {
    #[default]
    Smooth,
    Sharp,
}

/// Which devices show at their real size, using the display's measure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RealSize {
    #[default]
    Off,
    /// iPads only: what the iPad size page of 0.0.14 turned on.
    Ipads,
    All,
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
    /// iOS apps that record hear this computer's microphone, only while they record.
    pub microphone: bool,
    /// Skip the engine's code-cache flushes as an iOS app opens (fewer pauses, ~5% slower games; read at start).
    pub fewer_app_pauses: bool,
    /// Saved as `screen_scaling`; `scaling` keeps smooth or sharp for older versions (see `load`).
    #[serde(rename = "screen_scaling", deserialize_with = "lenient")]
    pub scaling: Scaling,
    /// Pixel perfect's multiple: 0 is the largest that fits.
    pub integer_scale: u8,
    #[serde(rename = "scaling")]
    legacy_scaling: LegacyScaling,
    /// Draw the device around the screen on the home view.
    pub device_frame: bool,
    /// Pause the guest while the window is minimized, and resume when it comes back.
    pub pause_when_hidden: bool,
    /// Full screen shows its button bar (volume, Home, Sleep/Wake, More); without it Esc or F11 leaves.
    pub fullscreen_controls: bool,
    pub overlay: OverlayLevel,
    pub overlay_graph: OverlayGraph,
    pub overlay_corner: Corner,
    pub overlay_opacity: f32,
    pub overlay_in_device_view: bool,
    /// Show the frame inspector button.
    pub inspect_button: bool,
    /// Show Screenshot in the device view's controls (off by default, for settings saved before it too; full screen's
    /// More menu always has it).
    pub screenshot_button: bool,
    /// Show the Carrier button on every device with a modem (it was each device's own setting in 0.0.15's test builds).
    pub carrier_button: bool,
    pub detailed_timing: bool,
    /// Settings → Network. Off by default; a section that doesn't parse or validate is off.
    #[serde(deserialize_with = "crate::network::lenient")]
    pub network: crate::network::NetworkSettings,
    /// How the device is turned on the home view and in full screen.
    pub rotation: crate::rotation::Rotation,
    /// The controller preset in use, by name; none turns controller and key bindings off.
    pub controller_preset: Option<String>,
    /// Controllers keep acting while another window is in front.
    pub controller_background: bool,
    /// Devices that show at the real device's size (Settings → Screen scaling), using `display`.
    #[serde(deserialize_with = "lenient")]
    pub real_size: RealSize,
    /// 0.0.14's iPad-only switch, read once into `real_size` and kept up to date for older versions.
    ipad_real_size: bool,
    /// The display's measured density, for `real_size`.
    pub display: Option<crate::real_size::DisplayMeasure>,
    /// The folder the devices are kept in (Settings → Storage); none keeps them in the data folder. Logs and these
    /// settings stay in the data folder either way.
    #[serde(deserialize_with = "lenient")]
    pub library: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: Theme::System,
            tilt: true,
            headphones: true,
            muted: false,
            microphone: false,
            fewer_app_pauses: false,
            scaling: Scaling::Smooth,
            integer_scale: 0,
            legacy_scaling: LegacyScaling::Smooth,
            device_frame: true,
            pause_when_hidden: true,
            fullscreen_controls: true,
            overlay: OverlayLevel::Off,
            overlay_graph: OverlayGraph::FrameTime,
            overlay_corner: Corner::TopLeft,
            overlay_opacity: 0.55,
            overlay_in_device_view: true,
            inspect_button: false,
            screenshot_button: false,
            carrier_button: false,
            detailed_timing: false,
            network: Default::default(),
            rotation: crate::rotation::Rotation::Portrait,
            controller_preset: None,
            controller_background: false,
            real_size: RealSize::Off,
            ipad_real_size: false,
            display: None,
            library: None,
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Self {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let mut settings = Self::parse(&text).unwrap_or_default();
        settings.network = settings.network.sanitized(crate::network::Day::today());
        settings
    }

    /// Settings from their JSON, taking up what versions before 0.0.15 saved: Sharp (nearest neighbour, which
    /// stays Sharp) and the iPad-only real size.
    fn parse(text: &str) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_str(text).ok()?;
        let mut settings: Self = serde_json::from_value(value.clone()).ok()?;
        if value.get("screen_scaling").is_none() && settings.legacy_scaling == LegacyScaling::Sharp {
            settings.scaling = Scaling::Sharp;
        }
        if value.get("real_size").is_none() && settings.ipad_real_size {
            settings.real_size = RealSize::Ipads;
        }
        Some(settings)
    }

    /// The display's density in physical pixels per millimetre when `ipad` (or any device, for `ipad` false) shows
    /// at its real size.
    pub fn real_px_per_mm(&self, ipad: bool) -> Option<f32> {
        let on = match self.real_size {
            RealSize::Off => false,
            RealSize::Ipads => ipad,
            RealSize::All => true,
        };
        self.display.filter(|_| on).map(|d| crate::real_size::clamp(d.px_per_mm))
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut saved = self.clone();
        saved.legacy_scaling = self.scaling.legacy();
        saved.ipad_real_size = self.real_size != RealSize::Off;
        let partial = path.with_extension("json.partial");
        std::fs::write(&partial, serde_json::to_vec_pretty(&saved).unwrap())?;
        std::fs::rename(partial, path)
    }
}

/// A value a newer version may have added reads as the default rather than failing the whole file.
fn lenient<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn earlier_settings_carry_over() {
        // 0.0.14: Sharp, and iPads at real size with a saved measure.
        let old = r#"{"theme":"dark","scaling":"sharp","ipad_real_size":true,
            "display":{"px_per_mm":4.2,"monitor":[1920,1080],"estimate":false}}"#;
        let s = Settings::parse(old).unwrap();
        assert_eq!(s.theme, Theme::Dark);
        assert_eq!(s.scaling, Scaling::Sharp);
        assert_eq!(s.real_size, RealSize::Ipads);
        assert_eq!(s.real_px_per_mm(true), Some(4.2));
        assert_eq!(s.real_px_per_mm(false), None);
        let fresh = Settings::parse(r#"{"scaling":"smooth"}"#).unwrap();
        assert_eq!((fresh.scaling, fresh.real_size), (Scaling::Smooth, RealSize::Off));
    }

    #[test]
    fn saved_settings_round_trip_and_stay_readable_by_older_versions() {
        let dir = std::env::temp_dir().join(format!("lt-settings-{}", std::process::id()));
        let path = dir.join("settings.json");
        let s = Settings {
            scaling: Scaling::PixelPerfect,
            integer_scale: 3,
            real_size: RealSize::All,
            library: Some(PathBuf::from("/media/games/LightTouch")),
            ..Default::default()
        };
        s.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["scaling"], "sharp");
        assert_eq!(value["ipad_real_size"], true);
        let back = Settings::load(&path);
        assert_eq!((back.scaling, back.integer_scale, back.real_size), (Scaling::PixelPerfect, 3, RealSize::All));
        assert_eq!(back.library.as_deref(), Some(Path::new("/media/games/LightTouch")));
        // A choice from a newer version leaves the rest of the file alone.
        let newer = r#"{"theme":"light","screen_scaling":"hologram","real_size":"some","library":7}"#;
        let s = Settings::parse(newer).unwrap();
        assert_eq!((s.theme, s.scaling, s.real_size), (Theme::Light, Scaling::Smooth, RealSize::Off));
        assert_eq!(s.library, None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
