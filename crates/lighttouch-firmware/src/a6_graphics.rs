//! The 14G61 armv7s dyld shared cache transform (Swift `A6GraphicsCache`).
//!
//! Later dyld requires a development cache as well as the existing override marker before selecting
//! the installed OpenGLES support framework. Only 14G61 is transformed; every other build is returned
//! byte for byte.
use crate::error::Result;

pub const CACHE_PATH: &str = "System/Library/Caches/com.apple.dyld/dyld_shared_cache_armv7s";
pub const UUID_14G61: [u8; 16] = [
    0xe2, 0x55, 0x3b, 0x54, 0x10, 0x5e, 0x3d, 0x2e, 0xbf, 0xec, 0x9f, 0x01, 0x56, 0xd9, 0x6c, 0x11,
];

pub fn prepare(original: &[u8], build: &str) -> Result<Vec<u8>> {
    if build != "14G61" {
        return Ok(original.to_vec());
    }
    if original.len() < 112
        || original[88..104] != UUID_14G61
        || original[104] > 1
        || original[105..112].iter().any(|&b| b != 0)
    {
        return Err(crate::unsupported!("N42 graphics: unrecognized 14G61 shared cache"));
    }
    if original[104] == 0 {
        return Ok(original.to_vec());
    }
    let mut result = original.to_vec();
    result[104..112].fill(0);
    // Retain every framework instruction and refresh only existing signed page hashes. The exact
    // resulting cache identity is admitted by A6GuestAssets.
    crate::shared_cache::DyldSharedCache::parse(result)?.rehash_code_signature()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn other_builds_untouched() {
        let b = vec![0u8; 128];
        assert_eq!(prepare(&b, "13A344").unwrap(), b);
    }

    #[test]
    fn bad_14g61_cache_rejected() {
        assert!(prepare(&[0u8; 128], "14G61").is_err());
    }
}
