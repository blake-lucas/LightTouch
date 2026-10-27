//! The catalog order of a case-insensitive HFS+ volume (an iOS rootfs or restore ramdisk): Apple's FastUnicodeCompare
//! from TN1150, which folds case with its lower-case table, skips the ignorable characters and sorts NUL last. The
//! table is kept as runs (first, count, step, offset to the lower case); Linux's fs/hfsplus/tables.c has it in full.
use std::cmp::Ordering;
use std::sync::OnceLock;

const RUNS: &[(usize, usize, usize, usize)] = &[
    (0x0041, 26, 1, 32), (0x00C6, 1, 1, 32), (0x00D0, 1, 1, 32), (0x00D8, 1, 1, 32), (0x00DE, 1, 1, 32),
    (0x0110, 1, 1, 1), (0x0126, 1, 1, 1), (0x0132, 1, 1, 1), (0x013F, 2, 2, 1), (0x014A, 1, 1, 1),
    (0x0152, 1, 1, 1), (0x0166, 1, 1, 1), (0x0181, 1, 1, 210), (0x0182, 2, 2, 1), (0x0186, 1, 1, 206),
    (0x0187, 1, 1, 1), (0x0189, 2, 1, 205), (0x018B, 1, 1, 1), (0x018E, 1, 1, 79), (0x018F, 1, 1, 202),
    (0x0190, 1, 1, 203), (0x0191, 1, 1, 1), (0x0193, 1, 1, 205), (0x0194, 1, 1, 207), (0x0196, 1, 1, 211),
    (0x0197, 1, 1, 209), (0x0198, 1, 1, 1), (0x019C, 1, 1, 211), (0x019D, 1, 1, 213), (0x019F, 1, 1, 214),
    (0x01A2, 2, 2, 1), (0x01A7, 1, 1, 1), (0x01A9, 1, 1, 218), (0x01AC, 1, 1, 1), (0x01AE, 1, 1, 218),
    (0x01B1, 2, 1, 217), (0x01B3, 2, 2, 1), (0x01B7, 1, 1, 219), (0x01B8, 1, 1, 1), (0x01BC, 1, 1, 1),
    (0x01C4, 1, 1, 2), (0x01C5, 1, 1, 1), (0x01C7, 1, 1, 2), (0x01C8, 1, 1, 1), (0x01CA, 1, 1, 2),
    (0x01CB, 1, 1, 1), (0x01E4, 1, 1, 1), (0x01F1, 1, 1, 2), (0x01F2, 1, 1, 1), (0x0391, 17, 1, 32),
    (0x03A3, 7, 1, 32), (0x03E2, 7, 2, 1), (0x0402, 2, 2, 80), (0x0405, 2, 1, 80), (0x0408, 4, 1, 80),
    (0x040F, 1, 1, 80), (0x0410, 9, 1, 32), (0x041A, 22, 1, 32), (0x0460, 11, 2, 1), (0x0478, 5, 2, 1),
    (0x0490, 24, 2, 1), (0x04C3, 1, 1, 1), (0x04C7, 1, 1, 1), (0x04CB, 1, 1, 1), (0x0531, 38, 1, 48),
    (0x10A0, 38, 1, 48), (0x2160, 16, 1, 16), (0xFF21, 26, 1, 32),
];

fn table() -> &'static [u16] {
    static TABLE: OnceLock<Vec<u16>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t: Vec<u16> = (0..=0xFFFFu32).map(|c| c as u16).collect();
        t[0] = 0xFFFF;
        for &(first, count, step, delta) in RUNS {
            for k in 0..count {
                t[first + k * step] = (first + k * step + delta) as u16;
            }
        }
        for range in [0x200C..=0x200F, 0x202A..=0x202E, 0x206A..=0x206F, 0xFEFF..=0xFEFF] {
            for c in range {
                t[c] = 0;
            }
        }
        t
    })
}

/// How `a` sorts against `b` in a case-insensitive catalog.
pub fn compare(a: &[u16], b: &[u16]) -> Ordering {
    let t = table();
    let (mut i, mut j) = (0, 0);
    loop {
        let (mut c1, mut c2) = (0u16, 0u16);
        while c1 == 0 && i < a.len() {
            c1 = t[a[i] as usize];
            i += 1;
        }
        while c2 == 0 && j < b.len() {
            c2 = t[b[j] as usize];
            j += 1;
        }
        if c1 != c2 {
            return c1.cmp(&c2);
        }
        if c1 == 0 {
            return Ordering::Equal;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn folds_case_and_skips_ignorables() {
        assert_eq!(compare(&u("ABC"), &u("abc")), Ordering::Equal);
        assert_eq!(compare(&u("a\u{200C}b"), &u("ab")), Ordering::Equal);
        assert_eq!(compare(&u("a"), &u("B")), Ordering::Less);
        assert_eq!(compare(&u("ab"), &u("a")), Ordering::Greater);
    }
}
