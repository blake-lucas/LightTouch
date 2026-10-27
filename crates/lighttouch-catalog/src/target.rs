//! The guest an app has to suit: what it runs, not the host.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub product_type: String,
    pub ios: String,
    /// "armv6" or "armv7".
    pub arch: String,
    /// UIDeviceFamily: "1" iPhone/iPod touch, "2" iPad.
    pub family: String,
}

impl Target {
    /// The boards LightTouch boots; any other product has no known architecture.
    pub fn of(product_type: &str, ios: &str) -> Option<Self> {
        let arch = match product_type {
            "iPod1,1" | "iPhone1,1" | "iPhone1,2" | "iPod2,1" => "armv6",
            "iPod3,1" | "iPhone2,1" | "iPad1,1" | "iPod4,1" | "iPhone3,1" => "armv7",
            _ => return None,
        };
        let family = if product_type.starts_with("iPad") { "2" } else { "1" };
        Some(Self { product_type: product_type.into(), ios: ios.into(), arch: arch.into(), family: family.into() })
    }

    /// Whether code built for `arch` runs here (armv7 runs armv6 code).
    pub fn runs(&self, arch: &str) -> bool {
        arch == self.arch || (self.arch == "armv7" && arch == "armv6")
    }
}

/// "3.1" → [3, 1, 0]; `None` for anything that isn't 1–3 numbers starting above zero.
pub fn version(text: &str) -> Option<[u32; 3]> {
    let parts: Vec<&str> = text.split('.').collect();
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    let mut out = [0u32; 3];
    for (slot, part) in out.iter_mut().zip(&parts) {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *slot = part.parse().ok()?;
    }
    (out[0] != 0).then_some(out)
}

/// Whether an app needing iOS `minimum` is too new for `target`. Unreadable versions count as too new.
pub fn requires_newer(minimum: &str, target: &Target) -> bool {
    match (version(minimum), version(&target.ios)) {
        (Some(need), Some(have)) => need > have,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(version("3.1"), Some([3, 1, 0]));
        assert_eq!(version("4.2.1"), Some([4, 2, 1]));
        assert_eq!(version("0.9"), None);
        assert_eq!(version("4.x"), None);
        let t = Target::of("iPod2,1", "3.1.3").unwrap();
        assert!(!requires_newer("3.0", &t));
        assert!(requires_newer("3.2", &t));
        assert!(t.runs("armv6") && !t.runs("armv7"));
    }
}
