//! The iPhone 5's flat PPN media geometry (Swift `A6StorageLayout`).
//!
//! Native 4096-byte sectors. System starts at 16 MiB and is 3 GiB; Data occupies everything after it.
//! The 4096-byte MBR is a **preparation artifact** for new media, not an edit of an existing disk.
use crate::error::Result;

pub const MEDIA_BYTES: u64 = 16_000_000_000;
pub const SECTOR_BYTES: u64 = 4096;
pub const SYSTEM_OFFSET: u64 = 16 << 20;
pub const SYSTEM_BYTES: u64 = 3 << 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct A6StorageLayout {
    pub media_bytes: u64,
    pub system_offset: u64,
    pub system_bytes: u64,
    pub data_offset: u64,
    pub data_bytes: u64,
}

impl A6StorageLayout {
    pub fn standard() -> A6StorageLayout {
        Self::new(MEDIA_BYTES, SYSTEM_BYTES).expect("the catalog's 16g geometry is valid")
    }

    /// A6StorageLayout.swift:14-36, including the validation order.
    pub fn new(media_bytes: u64, system_bytes: u64) -> Result<A6StorageLayout> {
        let start = SYSTEM_OFFSET;
        let end = start
            .checked_add(system_bytes)
            .ok_or_else(|| crate::unsupported!("Invalid A6 System/Data media geometry"))?;
        if system_bytes == 0
            || system_bytes % SECTOR_BYTES != 0
            || media_bytes % SECTOR_BYTES != 0
            || media_bytes / SECTOR_BYTES > u64::from(u32::MAX)
            || end >= media_bytes
        {
            return Err(crate::unsupported!("Invalid A6 System/Data media geometry"));
        }
        Ok(A6StorageLayout {
            media_bytes,
            system_offset: start,
            system_bytes,
            data_offset: end,
            data_bytes: media_bytes - end,
        })
    }

    /// A new native-sector MBR (A6StorageLayout.swift:38-56). 4096 bytes, two 0xAF entries,
    /// CHS 254/255/255 and 0x55AA at 510. The first byte of each entry and the boot signature
    /// bytes 446..<510 stay zero.
    pub fn partition_table(&self) -> Vec<u8> {
        let mut table = vec![0u8; SECTOR_BYTES as usize];
        for (index, (start, count)) in
            [(self.system_offset, self.system_bytes), (self.data_offset, self.data_bytes)].iter().enumerate()
        {
            let e = 446 + index * 16;
            table[e + 1] = 254;
            table[e + 2] = 255;
            table[e + 3] = 255;
            table[e + 4] = 0xaf;
            table[e + 5] = 254;
            table[e + 6] = 255;
            table[e + 7] = 255;
            let lba = (start / SECTOR_BYTES) as u32;
            let blocks = (count / SECTOR_BYTES) as u32;
            table[e + 8..e + 12].copy_from_slice(&lba.to_le_bytes());
            table[e + 12..e + 16].copy_from_slice(&blocks.to_le_bytes());
        }
        table[510] = 0x55;
        table[511] = 0xaa;
        table
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_geometry() {
        let l = A6StorageLayout::standard();
        assert_eq!(l.media_bytes, 16_000_000_000);
        assert_eq!(l.system_offset, 16_777_216);
        assert_eq!(l.system_bytes, 3_221_225_472);
        assert_eq!(l.data_offset, 3_238_002_688);
        assert_eq!(l.data_bytes, 12_761_997_312);
        assert!(l.data_bytes % SECTOR_BYTES == 0);
    }

    #[test]
    fn mbr_entries() {
        let t = A6StorageLayout::standard().partition_table();
        assert_eq!(t.len(), 4096);
        assert_eq!(&t[446..462], &[0, 254, 255, 255, 0xaf, 254, 255, 255, 0, 16, 0, 0, 0, 0, 12, 0]);
        assert_eq!(&t[462..478], &[0, 254, 255, 255, 0xaf, 254, 255, 255, 0, 16, 12, 0, 0xca, 0x8a, 0x2f, 0]);
        assert_eq!(&t[510..512], &[0x55, 0xaa]);
    }

    #[test]
    fn rejects_bad_geometry() {
        assert!(A6StorageLayout::new(MEDIA_BYTES, 3 << 30).is_ok());
        assert!(A6StorageLayout::new(MEDIA_BYTES + 1, SYSTEM_BYTES).is_err());
        assert!(A6StorageLayout::new(MEDIA_BYTES, SYSTEM_BYTES + 1).is_err());
        assert!(A6StorageLayout::new(MEDIA_BYTES, 0).is_err());
        assert!(A6StorageLayout::new(16 << 20, SYSTEM_BYTES).is_err());
    }
}
