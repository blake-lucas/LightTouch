//! The firmware catalog (`Portable/firmware/firmware-catalog.json`, built into the binary): every iOS build the
//! preparer knows, with its IPSW source and recipe. Support levels are LightTouch's own compatibility runs
//! (docs/COMPATIBILITY.md), not the catalog's upstream labels.
use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::OnceLock;

/// The catalog JSON as shipped. firmwarekit also reads it from a file (`--catalog`).
pub const CATALOG_JSON: &str = include_str!("../../../Portable/firmware/firmware-catalog.json");

#[derive(Debug, Clone, Deserialize)]
pub struct Firmware {
    pub id: String,
    pub board: String,
    pub product_type: String,
    pub version: String,
    pub build: String,
    #[serde(default)]
    pub released: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub prerelease: String,
    #[serde(default)]
    pub prerelease_number: u32,
    #[serde(default)]
    pub source: Option<Source>,
    #[serde(default)]
    pub recipe: Option<Recipe>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Source {
    pub kind: String,
    pub url: String,
    pub sha1: String,
    pub bytes: u64,
    #[serde(default)]
    pub mirrors: Vec<Mirror>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Mirror {
    pub url: String,
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Recipe {
    pub name: String,
    /// The entry whose restore ramdisk boots this build's keybag one-shot (its own has no published key).
    #[serde(default)]
    pub keybag_ramdisk_from: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Support {
    Tested,
    TestedSlow,
    Experimental,
    Untested,
}

impl Support {
    pub fn label(self) -> &'static str {
        match self {
            Self::Tested => "Tested",
            Self::TestedSlow => "Tested, slow",
            Self::Experimental => "Experimental",
            Self::Untested => "Untested",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            Self::Tested => "Checked on LightTouch.",
            Self::TestedSlow => "Checked on LightTouch, but slower than the iPod touch 2G on iOS 3.1.3.",
            Self::Experimental => "Starts in Light Touch for Mac's checks; not yet checked on LightTouch.",
            Self::Untested => "Nobody has tried these yet. They may not start.",
        }
    }

    pub fn tested(self) -> bool {
        matches!(self, Self::Tested | Self::TestedSlow)
    }
}

/// Firmwares that pass LightTouch's compatibility runs (docs/COMPATIBILITY.md).
pub const TESTED: &[&str] = &[
    "n72ap-7E18",   // iPod touch 2G 3.1.3
    "n72ap-8C148",  // iPod touch 2G 4.2.1
    "n18ap-8C148",  // iPod touch 3G 4.2.1
    "n81ap-10B500", // iPod touch 4G 6.1.6
    "n88ap-9B206",  // iPhone 3GS 5.1.1
    "n90ap-8C148",  // iPhone 4 4.2.1
    "n90ap-8L1",    // iPhone 4 4.3.5
];

/// Tested, but noticeably slower than the iPod touch 2G on 3.1.3.
pub const SLOW: &[&str] = &["n88ap-9B206", "n90ap-8C148", "n90ap-8L1"];

/// The firmware to pick first: the fastest and most complete.
pub const RECOMMENDED: &str = "n72ap-7E18";

/// Order devices are listed in.
pub const PRODUCT_ORDER: &[&str] =
    &["iPod1,1", "iPod2,1", "iPod3,1", "iPod4,1", "iPhone1,1", "iPhone1,2", "iPhone2,1", "iPhone3,1", "iPad1,1"];

impl Firmware {
    /// "3.1.3", or "4.2 beta 2" for a developer build.
    pub fn label(&self) -> String {
        if self.prerelease.is_empty() {
            self.version.clone()
        } else if self.prerelease_number > 0 {
            format!("{} {} {}", self.version, self.prerelease, self.prerelease_number)
        } else {
            format!("{} {}", self.version, self.prerelease)
        }
    }

    pub fn is_beta(&self) -> bool {
        !self.prerelease.is_empty()
    }

    pub fn file_name(&self) -> String {
        format!("{}_{}_{}_Restore.ipsw", self.product_type, self.version, self.build)
    }

    pub fn support(&self) -> Support {
        if SLOW.contains(&self.id.as_str()) {
            Support::TestedSlow
        } else if TESTED.contains(&self.id.as_str()) {
            Support::Tested
        } else if self.status == "available" || self.status == "experimental" {
            Support::Experimental
        } else {
            Support::Untested
        }
    }

    pub fn device_name(&self) -> &'static str {
        short_product_name(&self.product_type)
    }

    /// "Released March 2010".
    pub fn released_label(&self) -> String {
        const MONTHS: [&str; 12] = [
            "January",
            "February",
            "March",
            "April",
            "May",
            "June",
            "July",
            "August",
            "September",
            "October",
            "November",
            "December",
        ];
        let mut parts = self.released.split('-');
        match (parts.next(), parts.next().and_then(|m| m.parse::<usize>().ok())) {
            (Some(year), Some(month)) if (1..=12).contains(&month) => format!("{} {year}", MONTHS[month - 1]),
            _ => self.released.clone(),
        }
    }

    /// IPSW download candidates from the catalog itself (https only, IPSW kind only).
    pub fn source_urls(&self) -> Vec<String> {
        let Some(source) = &self.source else { return Vec::new() };
        if source.kind != "ipsw" {
            return Vec::new();
        }
        std::iter::once(&source.url)
            .chain(source.mirrors.iter().filter(|m| m.kind.as_deref().is_none_or(|k| k == "ipsw")).map(|m| &m.url))
            .filter(|u| u.starts_with("https://"))
            .cloned()
            .collect()
    }

    pub fn ipsw_bytes(&self) -> u64 {
        self.source.as_ref().map_or(0, |s| s.bytes)
    }

    /// About how long preparation takes and how much room it needs, for the prepare screen.
    pub fn cost_hint(&self) -> (&'static str, &'static str) {
        if self.board == "n72ap" { ("about a minute", "1.5 GB") } else { ("a few minutes", "5 GB") }
    }
}

#[derive(Debug, Deserialize)]
struct CatalogFile {
    entries: Vec<Firmware>,
}

pub struct Catalog {
    firmwares: Vec<Firmware>,
}

impl Catalog {
    /// The built-in catalog, parsed once.
    pub fn get() -> &'static Catalog {
        static CATALOG: OnceLock<Catalog> = OnceLock::new();
        CATALOG.get_or_init(|| Catalog::parse(CATALOG_JSON).expect("the built-in firmware catalog parses"))
    }

    pub fn parse(text: &str) -> Result<Self, serde_json::Error> {
        let file: CatalogFile = serde_json::from_str(text)?;
        // "coming_soon" entries have no preparation recipe yet.
        let firmwares = file.entries.into_iter().filter(|f| f.status != "coming_soon").collect();
        Ok(Self { firmwares })
    }

    pub fn firmwares(&self) -> &[Firmware] {
        &self.firmwares
    }

    pub fn find(&self, id: &str) -> Option<&Firmware> {
        self.firmwares.iter().find(|f| f.id == id)
    }

    pub fn find_build(&self, board: &str, build: &str) -> Option<&Firmware> {
        self.find(&format!("{board}-{build}"))
    }

    /// Devices in display order, each with its firmwares newest first.
    pub fn devices(&self) -> Vec<DeviceGroup<'_>> {
        let mut by_product: BTreeMap<usize, Vec<&Firmware>> = BTreeMap::new();
        for firmware in &self.firmwares {
            let order = PRODUCT_ORDER.iter().position(|p| *p == firmware.product_type).unwrap_or(PRODUCT_ORDER.len());
            by_product.entry(order).or_default().push(firmware);
        }
        by_product
            .into_values()
            .map(|mut firmwares| {
                firmwares.sort_by(|a, b| compare_versions(&b.version, &a.version).then_with(|| b.build.cmp(&a.build)));
                DeviceGroup { product_type: firmwares[0].product_type.clone(), firmwares }
            })
            .collect()
    }
}

pub struct DeviceGroup<'a> {
    pub product_type: String,
    pub firmwares: Vec<&'a Firmware>,
}

impl DeviceGroup<'_> {
    pub fn name(&self) -> &'static str {
        short_product_name(&self.product_type)
    }

    pub fn tested(&self) -> Vec<&Firmware> {
        self.firmwares.iter().copied().filter(|f| f.support().tested()).collect()
    }

    /// "Tested: iOS 3.1.3, 4.2.1 · 23 more to try", or "iOS 1.0 to 3.1.3".
    pub fn summary(&self) -> String {
        let tested = self.tested();
        let releases: Vec<&Firmware> = self.firmwares.iter().copied().filter(|f| !f.is_beta()).collect();
        if tested.is_empty() {
            match (releases.last(), releases.first()) {
                (Some(oldest), Some(newest)) if oldest.version != newest.version => {
                    format!("iOS {} to {}", oldest.version, newest.version)
                }
                (Some(only), _) => format!("iOS {}", only.version),
                _ => String::new(),
            }
        } else {
            let mut versions: Vec<&str> = tested.iter().map(|f| f.version.as_str()).collect();
            versions.reverse();
            let more = releases.len().saturating_sub(tested.len());
            if more > 0 {
                format!("Tested: iOS {} · {more} more to try", versions.join(", "))
            } else {
                format!("Tested: iOS {}", versions.join(", "))
            }
        }
    }
}

/// Compare dotted version numbers numerically ("4.2.1" > "4.2" > "3.1.3").
pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let parse = |v: &str| -> Vec<u32> { v.split('.').map(|p| p.parse().unwrap_or(0)).collect() };
    let (a, b) = (parse(a), parse(b));
    for i in 0..a.len().max(b.len()) {
        let ordering = a.get(i).unwrap_or(&0).cmp(b.get(i).unwrap_or(&0));
        if ordering.is_ne() {
            return ordering;
        }
    }
    std::cmp::Ordering::Equal
}

pub fn short_product_name(product_type: &str) -> &'static str {
    match product_type {
        "iPod1,1" => "iPod touch",
        "iPod2,1" => "iPod touch 2G",
        "iPod3,1" => "iPod touch 3G",
        "iPod4,1" => "iPod touch 4G",
        "iPhone1,1" => "iPhone",
        "iPhone1,2" => "iPhone 3G",
        "iPhone2,1" => "iPhone 3GS",
        "iPhone3,1" => "iPhone 4",
        "iPad1,1" => "iPad",
        _ => "iOS device",
    }
}

/// "iPod touch 2G" for a board, when the catalog knows it.
pub fn model_name(board: &str, build: &str) -> Option<&'static str> {
    Catalog::get().find_build(board, build).map(|f| f.device_name())
}

/// "iPod touch 2G · iOS 3.1.3", or the board and build when the catalog does not know them.
pub fn device_subtitle(board: &str, build: &str) -> String {
    match Catalog::get().find_build(board, build) {
        Some(f) => format!("{} · iOS {}", f.device_name(), f.label()),
        None => format!("{board} · {build}"),
    }
}

/// Default name for a new device: "<model> <iOS version>", numbered when another device has it.
pub fn default_device_name<'a>(board: &str, build: &str, taken: impl IntoIterator<Item = &'a str> + Clone) -> String {
    let base = match Catalog::get().find_build(board, build) {
        Some(f) => format!("{} {}", f.device_name(), f.label()),
        None => format!("{board} {build}"),
    };
    let is_taken = |name: &str| taken.clone().into_iter().any(|t| t == name);
    if !is_taken(&base) {
        return base;
    }
    (2..).map(|n| format!("{base} ({n})")).find(|name| !is_taken(name)).unwrap()
}

/// A name LightTouch made up from the firmware id rather than one the user chose.
pub fn is_generated_name(name: &str, board: &str, build: &str) -> bool {
    name.trim().is_empty() || name == format!("{board} {build}")
}

/// Whether a typed name fits: 60 characters and the library's 127-byte UTF-8 limit.
pub fn fits_device_name(name: &str) -> bool {
    name.chars().count() <= 60 && name.trim().len() <= 127
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_parses_and_groups() {
        let catalog = Catalog::get();
        assert!(catalog.firmwares().len() > 200);
        let recommended = catalog.find(RECOMMENDED).unwrap();
        assert_eq!(recommended.label(), "3.1.3");
        assert_eq!(recommended.support(), Support::Tested);
        assert_eq!(recommended.file_name(), "iPod2,1_3.1.3_7E18_Restore.ipsw");
        let devices = catalog.devices();
        assert_eq!(devices[0].product_type, "iPod1,1");
        let ipod2 = devices.iter().find(|d| d.product_type == "iPod2,1").unwrap();
        assert!(ipod2.summary().starts_with("Tested: iOS 3.1.3, 4.2.1"), "{}", ipod2.summary());
    }

    #[test]
    fn names_follow_the_android_rules() {
        assert_eq!(default_device_name("n72ap", "7E18", ["x"]), "iPod touch 2G 3.1.3");
        assert_eq!(default_device_name("n72ap", "7E18", ["iPod touch 2G 3.1.3"]), "iPod touch 2G 3.1.3 (2)");
        assert_eq!(device_subtitle("zz", "1A1"), "zz · 1A1");
        assert_eq!(compare_versions("4.2.1", "4.2"), std::cmp::Ordering::Greater);
    }
}
