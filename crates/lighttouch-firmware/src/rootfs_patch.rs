//! The kernel patch a jailbroken iOS 7 device carries so launchd can remount its root filesystem read-write.
//!
//! iOS 7's launchd reads /etc/fstab at boot and remounts `/` as the file says. A stock 7.1.2 kernel refuses that
//! remount: the shipped binary carries the release-kernel check
//! `if ((flags & MNT_RDONLY) == 0) { error = EPERM; goto out; }` in `__mac_mount`'s rootfs branch (public xnu 2422
//! has it under `#if 0`, but the release kernel compiled it in), so `mount -u -o rw /` fails with EPERM and launchd
//! reboots. Clearing that error and skipping the exit makes a read-write remount take exactly the path a read-only one
//! took — which is what the jailbreak needs to write /Applications, /usr and the Cydia bootstrap.
//!
//! The site is located by a Thumb-2 byte pattern, not a fixed offset: the rootfs branch of `__mac_mount` (v_mount's
//! MNT_ROOTFS test, the union/update flags, then the EPERM assignment) occurs once in the kernel's `__TEXT`. Kernels
//! that do not have it (iOS before 7) are left stock: `locate` says `NotFound` and the caller keeps the stock kernel.
//!
//! ```ignore
//! let (patched, found) = rootfs_patch::apply(&kernel)?;   // found.address is the "remount patch at 0x…" log line
//! ```
use std::fmt;

/// The rootfs branch of `__mac_mount`, 22 bytes (Thumb-2, little-endian):
/// `bic r5,r4,#MNT_UPDATE; tst.w r4,#MNT_UNION; mov.w r10,#EPERM; it eq; orreq r5,r4,#MNT_UPDATE;
/// tst.w r5,#MNT_RDONLY`. The `beq.w out` that follows is [`BRANCH`], outside the pattern because it is patched too.
pub const PATTERN: [u8; 22] = [
    0x24, 0xf4, 0x80, 0x35, // bic r5, r4, #0x10000 (MNT_UPDATE)
    0x14, 0xf0, 0x20, 0x0f, // tst.w r4, #0x20 (MNT_UNION)
    0x4f, 0xf0, 0x01, 0x0a, // mov.w r10, #0x1 (error = EPERM)
    0x08, 0xbf, // it eq
    0x44, 0xf4, 0x80, 0x35, // orreq r5, r4, #0x10000
    0x15, 0xf0, 0x01, 0x0f, // tst.w r5, #0x1 (MNT_RDONLY)
];

/// The branch slot's stock bytes: `beq.w out`, taken when a read-write mount was asked for.
pub const BRANCH: [u8; 4] = [0x01, 0xf0, 0x22, 0x80];

/// `mov.w r10, #0` (was #1): the branch no longer exits, so it must not leave EPERM behind.
pub const CLEAR_ERROR: [u8; 4] = [0x4f, 0xf0, 0x00, 0x0a];

/// Two NOPs over the exit branch: a read-write remount falls through as a read-only one did.
pub const SKIP_EXIT: [u8; 4] = [0x00, 0xbf, 0x00, 0xbf];

/// Where [`CLEAR_ERROR`] goes, from the start of the pattern.
pub const ERROR_SLOT: usize = 8;

/// Where [`SKIP_EXIT`] goes, from the start of the pattern (just past it).
pub const BRANCH_SLOT: usize = PATTERN.len();

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// The virtual address of the rootfs branch (the first patched instruction).
    pub address: u32,
    /// The bytes there before the patch (pattern and branch), for the record.
    pub original: Vec<u8>,
}

/// Why the patch site was not found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchError {
    /// Not a 32-bit Mach-O kernelcache (no segment maps the site).
    NotMachO,
    /// No rootfs branch: an iOS before 7, whose kernel does not refuse the remount.
    NotFound,
    /// The pattern occurs more than once, so the patch would be a guess.
    Ambiguous,
    /// The pattern is there but the branch after it is not `beq.w out`.
    BadBranch,
}

impl fmt::Display for PatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            PatchError::NotMachO => "not a 32-bit Mach-O kernelcache",
            PatchError::NotFound => "no rootfs remount check (iOS before 7)",
            PatchError::Ambiguous => "the rootfs remount pattern occurs more than once",
            PatchError::BadBranch => "the rootfs remount branch is not beq.w out",
        })
    }
}

impl std::error::Error for PatchError {}

impl From<PatchError> for crate::Error {
    fn from(e: PatchError) -> Self {
        crate::Error::unsupported(e.to_string())
    }
}

/// Locate the rootfs remount check in a decrypted, decompressed kernelcache Mach-O.
pub fn locate(kernel: &[u8]) -> std::result::Result<Found, PatchError> {
    site(kernel).map(|(_, found)| found)
}

/// Apply the patch in place, returning the patched kernelcache and where it landed.
pub fn apply(kernel: &[u8]) -> std::result::Result<(Vec<u8>, Found), PatchError> {
    let (offset, found) = site(kernel)?;
    let mut out = kernel.to_vec();
    out[offset + ERROR_SLOT..offset + ERROR_SLOT + CLEAR_ERROR.len()].copy_from_slice(&CLEAR_ERROR);
    out[offset + BRANCH_SLOT..offset + BRANCH_SLOT + SKIP_EXIT.len()].copy_from_slice(&SKIP_EXIT);
    Ok((out, found))
}

/// The pattern's file offset and the [`Found`] record for it.
fn site(kernel: &[u8]) -> std::result::Result<(usize, Found), PatchError> {
    let mut hits = kernel.windows(PATTERN.len()).enumerate().filter(|(_, w)| *w == PATTERN).map(|(i, _)| i);
    let offset = hits.next().ok_or(PatchError::NotFound)?;
    if hits.next().is_some() {
        return Err(PatchError::Ambiguous);
    }
    let branch = offset + BRANCH_SLOT;
    if kernel.get(branch..branch + BRANCH.len()) != Some(&BRANCH) {
        return Err(PatchError::BadBranch);
    }
    let address = vaddr(kernel, offset).ok_or(PatchError::NotMachO)?;
    Ok((offset, Found { address, original: kernel[offset..branch + BRANCH.len()].to_vec() }))
}

fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// The virtual address of a file offset, from the Mach-O's LC_SEGMENT map (the log line's address).
fn vaddr(kernel: &[u8], offset: usize) -> Option<u32> {
    if kernel.len() < 28 || le32(kernel, 0) != 0xFEED_FACE {
        return None;
    }
    let mut o = 28;
    for _ in 0..le32(kernel, 16) {
        if o + 8 > kernel.len() {
            break;
        }
        let cmd = le32(kernel, o);
        let size = le32(kernel, o + 4) as usize;
        if size < 8 || o + size > kernel.len() {
            break;
        }
        if cmd == 1 {
            // LC_SEGMENT: vmaddr, vmsize, fileoff, filesize
            let vmaddr = le32(kernel, o + 24);
            let fileoff = le32(kernel, o + 32) as usize;
            let filesize = le32(kernel, o + 36) as usize;
            if offset >= fileoff && offset < fileoff.saturating_add(filesize) {
                return Some(vmaddr.wrapping_add((offset - fileoff) as u32));
            }
        }
        o += size;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-segment Mach-O mapping file offset 0x1000 to virtual address 0x80002000.
    fn synthetic(pattern_at: usize, branch: [u8; 4]) -> Vec<u8> {
        let mut k = vec![0u8; 0x2000];
        k[0..4].copy_from_slice(&0xFEED_FACEu32.to_le_bytes());
        k[4..8].copy_from_slice(&12u32.to_le_bytes());
        k[16..20].copy_from_slice(&1u32.to_le_bytes()); // ncmds
        k[20..24].copy_from_slice(&56u32.to_le_bytes()); // sizeofcmds
        k[28..32].copy_from_slice(&1u32.to_le_bytes()); // LC_SEGMENT
        k[32..36].copy_from_slice(&56u32.to_le_bytes());
        k[36..42].copy_from_slice(b"__TEXT");
        k[52..56].copy_from_slice(&0x80002000u32.to_le_bytes()); // vmaddr
        k[56..60].copy_from_slice(&0x1000u32.to_le_bytes()); // vmsize
        k[60..64].copy_from_slice(&0x1000u32.to_le_bytes()); // fileoff
        k[64..68].copy_from_slice(&0x1000u32.to_le_bytes()); // filesize
        k[pattern_at..pattern_at + PATTERN.len()].copy_from_slice(&PATTERN);
        k[pattern_at + BRANCH_SLOT..pattern_at + BRANCH_SLOT + 4].copy_from_slice(&branch);
        k
    }

    #[test]
    fn locates_and_patches() {
        let kernel = synthetic(0x1010, BRANCH);
        let found = locate(&kernel).unwrap();
        assert_eq!(found.address, 0x80002010);
        assert_eq!(found.original.len(), PATTERN.len() + BRANCH.len());

        let (patched, again) = apply(&kernel).unwrap();
        assert_eq!(again, found);
        assert_eq!(&patched[0x1010 + ERROR_SLOT..0x1010 + ERROR_SLOT + 4], &CLEAR_ERROR);
        assert_eq!(&patched[0x1010 + BRANCH_SLOT..0x1010 + BRANCH_SLOT + 4], &SKIP_EXIT);
        let changed: Vec<usize> = kernel.iter().zip(&patched).enumerate().filter(|(_, (a, b))| a != b).map(|(i, _)| i).collect();
        assert_eq!(changed.len(), 5);
        assert!(changed.iter().all(|&i| (0x1010 + ERROR_SLOT..0x1010 + ERROR_SLOT + 4).contains(&i) || (0x1010 + BRANCH_SLOT..0x1010 + BRANCH_SLOT + 4).contains(&i)));
    }

    #[test]
    fn missing_pattern_is_not_found() {
        assert_eq!(locate(&[]), Err(PatchError::NotFound));
        assert_eq!(locate(&synthetic(0x1010, BRANCH)[..0x1000]), Err(PatchError::NotFound));
    }

    #[test]
    fn a_second_pattern_is_ambiguous() {
        let mut kernel = synthetic(0x1010, BRANCH);
        kernel[0x1100..0x1100 + PATTERN.len()].copy_from_slice(&PATTERN);
        assert_eq!(locate(&kernel), Err(PatchError::Ambiguous));
    }

    #[test]
    fn a_different_branch_is_refused() {
        let kernel = synthetic(0x1010, [0x00, 0xbf, 0x00, 0xbf]);
        assert_eq!(locate(&kernel), Err(PatchError::BadBranch));
    }

    /// The decrypted 7.1.2 kernel (set LTM_ROOTFS_KERNEL to it): the branch resolves once, and applying changes only the
    /// five bytes of the two slots.
    #[test]
    #[ignore = "set LTM_ROOTFS_KERNEL to a decrypted 7.1.2 kernelcache.mach"]
    fn patches_the_shipped_kernel() {
        let Ok(path) = std::env::var("LTM_ROOTFS_KERNEL") else { return };
        let kernel = std::fs::read(path).unwrap();
        let (patched, found) = apply(&kernel).unwrap();
        assert_eq!(found.address, 0x8013252e);
        let changed: Vec<usize> = kernel.iter().zip(&patched).enumerate().filter(|(_, (a, b))| a != b).map(|(i, _)| i).collect();
        assert_eq!(changed.len(), 5);
    }
}
