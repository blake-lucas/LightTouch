//! A page-directory board's NAND as an image store (record format 2), which qemu-ios writes in place
//! (hw/arm/nand-image.c): geometry.json and cs0.pages ... cs<N-1>.pages, the record of page p of a chip at p * stride,
//! data then spare, stored XOR the board's blank record, so a hole reads as the blank page.
//!
//!   n72           the iPod touch 2G (ipod_touch_fmss.c): 4 chip selects of 4096 + 64-byte pages, the generated
//!                 layout's blank (zero data, spare bytes 8 and 10 0xFF: fmss_blank_page)
//!   s5l8900(n)    the iPod touch 1G (8 banks) and the original iPhone (4) (s5l8900_fmc.c): 2048 + 64-byte pages,
//!                 blank all 0xFF, as the chip erases
//!
//! ```ignore
//! let store = Store::create(&dir, &Image::n72())?;      // geometry.json and sized sparse chip files
//! store.write(chip, page, &record)?; store.sync()?;
//! let read = reader(&nand, overlay.as_deref())?;        // the live clone (overlay/nand) when there is one
//! ```
//! The format 1 page directories (cs<N>/, bank<N>/) and their migration stay in the Swift tool.
use crate::error::Result;
use crate::posio;
use std::fs::File;
use std::path::{Path, PathBuf};

pub const FORMAT: &str = "nand-xor-blank-v2";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    /// geometry.json's "board", "blank" and (n72) "layout".
    pub board: &'static str,
    pub blank: &'static str,
    pub layout: Option<&'static str>,
    pub chips: usize,
    pub page: usize,
    pub spare: usize,
    pub pages_per_block: usize,
    pub blocks_per_chip: usize,
    /// What a hole reads as, and the XOR key of every stored record.
    pub blank_record: Vec<u8>,
}

impl Image {
    pub fn n72() -> Image {
        use crate::n72nand::{BLANK_SPARE, PAGE, PAGES_PER_BLOCK, SPARE};
        let mut blank_record = vec![0u8; PAGE];
        blank_record.extend_from_slice(&BLANK_SPARE);
        Image {
            board: "n72ap",
            blank: "n72-generated",
            layout: Some("n72-generated-v1"),
            chips: 4,
            page: PAGE,
            spare: SPARE,
            pages_per_block: PAGES_PER_BLOCK,
            blocks_per_chip: 4096,
            blank_record,
        }
    }

    pub fn s5l8900(banks: usize) -> Image {
        use crate::n45nand::{BLOCKS_PER_BANK, PAGE, PAGES_PER_BLOCK, SPARE};
        Image {
            board: "s5l8900",
            blank: "ff",
            layout: None,
            chips: banks,
            page: PAGE,
            spare: SPARE,
            pages_per_block: PAGES_PER_BLOCK,
            blocks_per_chip: BLOCKS_PER_BANK,
            blank_record: vec![0xFF; PAGE + SPARE],
        }
    }

    pub fn stride(&self) -> usize {
        self.page + self.spare
    }
    pub fn block_bytes(&self) -> usize {
        self.pages_per_block * self.stride()
    }
    pub fn chip_bytes(&self) -> u64 {
        (self.blocks_per_chip * self.block_bytes()) as u64
    }
    pub fn pages_per_chip(&self) -> usize {
        self.blocks_per_chip * self.pages_per_block
    }

    pub fn chip_name(chip: usize) -> String {
        format!("cs{chip}.pages")
    }

    /// geometry.json as the Swift tool writes it (JSONSerialization, sorted keys, pretty: `"key" : value`, two-space
    /// indent, no final newline), so a device's files hash the same whichever tool made it.
    pub fn geometry(&self) -> String {
        let mut fields: Vec<(&str, String)> = vec![
            ("blank", format!("\"{}\"", self.blank)),
            ("blocks_per_ce", self.blocks_per_chip.to_string()),
            ("board", format!("\"{}\"", self.board)),
            ("chip_selects", self.chips.to_string()),
        ];
        if let Some(layout) = self.layout {
            fields.push(("layout", format!("\"{layout}\"")));
        }
        fields.extend([
            ("page_bytes", self.page.to_string()),
            ("pages_per_block", self.pages_per_block.to_string()),
            ("spare_bytes", self.spare.to_string()),
            ("storage_format", format!("\"{FORMAT}\"")),
        ]);
        let body: Vec<String> = fields.iter().map(|(k, v)| format!("  \"{k}\" : {v}")).collect();
        format!("{{\n{}\n}}", body.join(",\n"))
    }

    /// The image store `nand` holds, by its geometry.json; None for anything else.
    pub fn of(nand: &Path) -> Option<Image> {
        let text = std::fs::read(nand.join("geometry.json")).ok()?;
        let g: serde_json::Value = serde_json::from_slice(&text).ok()?;
        let int = |k: &str| g.get(k).and_then(serde_json::Value::as_u64).map(|v| v as usize);
        if g.get("storage_format")?.as_str()? != FORMAT {
            return None;
        }
        let image = match (g.get("board")?.as_str()?, g.get("blank")?.as_str()?) {
            ("n72ap", "n72-generated") => Image::n72(),
            ("s5l8900", "ff") => match int("chip_selects")? {
                n @ (4 | 8) => Image::s5l8900(n),
                _ => return None,
            },
            _ => return None,
        };
        let same = int("chip_selects") == Some(image.chips)
            && int("page_bytes") == Some(image.page)
            && int("spare_bytes") == Some(image.spare)
            && int("pages_per_block") == Some(image.pages_per_block)
            && int("blocks_per_ce") == Some(image.blocks_per_chip);
        same.then_some(image)
    }

    /// The nonzero 8-byte words of the blank record, by index: what XOR touches.
    fn blank_words(&self) -> Vec<(usize, u64)> {
        (0..self.stride() / 8)
            .filter_map(|i| {
                let w = u64::from_ne_bytes(self.blank_record[8 * i..8 * i + 8].try_into().ok()?);
                (w != 0).then_some((i, w))
            })
            .collect()
    }
}

/// Whole records in `buffer` XOR the blank record: stored to read, or back.
fn xor(buffer: &mut [u8], stride: usize, words: &[(usize, u64)]) {
    for record in buffer.chunks_exact_mut(stride) {
        for &(i, w) in words {
            let at = &mut record[8 * i..8 * i + 8];
            let v = u64::from_ne_bytes([at[0], at[1], at[2], at[3], at[4], at[5], at[6], at[7]]) ^ w;
            at.copy_from_slice(&v.to_ne_bytes());
        }
    }
}

/// One store's chip files, open. Reads and writes are positional, so threads may share it at distinct pages.
pub struct Store {
    pub image: Image,
    pub dir: PathBuf,
    files: Vec<File>,
    words: Vec<(usize, u64)>,
}

impl Store {
    /// `dir` (created, not its parents: a stale store is never written into) with its geometry and sized, empty
    /// chip files, open for writing.
    pub fn create(dir: &Path, image: &Image) -> Result<Store> {
        std::fs::create_dir(dir).map_err(|e| crate::internal!("{}: {e}", dir.display()))?;
        std::fs::write(dir.join("geometry.json"), image.geometry())?;
        let mut files = Vec::with_capacity(image.chips);
        for chip in 0..image.chips {
            let path = dir.join(Image::chip_name(chip));
            if path.exists() {
                return Err(crate::internal!("{}: exists", path.display()));
            }
            let f = posio::create_sparse(&path).map_err(|e| crate::internal!("{}: {e}", path.display()))?;
            f.set_len(image.chip_bytes()).map_err(|e| crate::internal!("{}: {e}", path.display()))?;
            files.push(f);
        }
        Ok(Store { words: image.blank_words(), image: image.clone(), dir: dir.to_path_buf(), files })
    }

    pub fn open(dir: &Path, writable: bool) -> Result<Store> {
        let Some(image) = Image::of(dir) else {
            return Err(crate::unsupported!("{}: not a NAND image store", dir.display()));
        };
        let mut files = Vec::with_capacity(image.chips);
        for chip in 0..image.chips {
            let path = dir.join(Image::chip_name(chip));
            let f = File::options()
                .read(true)
                .write(writable)
                .open(&path)
                .map_err(|e| crate::internal!("{}: {e}", path.display()))?;
            if f.metadata()?.len() != image.chip_bytes() {
                return Err(crate::unsupported!("{}: NAND file size differs from geometry", path.display()));
            }
            files.push(f);
        }
        Ok(Store { words: image.blank_words(), image, dir: dir.to_path_buf(), files })
    }

    fn offset(&self, page: usize) -> u64 {
        (page * self.image.stride()) as u64
    }

    fn check(&self, chip: usize, page: usize) -> Result<()> {
        if chip >= self.image.chips || page >= self.image.pages_per_chip() {
            return Err(crate::internal!("{}: no page {page} on chip {chip}", self.dir.display()));
        }
        Ok(())
    }

    /// The page's record as the emulator reads it.
    pub fn read(&self, chip: usize, page: usize) -> Result<Vec<u8>> {
        self.check(chip, page)?;
        let mut record = vec![0u8; self.image.stride()];
        posio::read_exact_at(&self.files[chip], &mut record, self.offset(page))?;
        xor(&mut record, self.image.stride(), &self.words);
        Ok(record)
    }

    /// The records of one erase block as they read.
    pub fn read_block(&self, chip: usize, block: usize, buffer: &mut [u8]) -> Result<()> {
        self.check(chip, block * self.image.pages_per_block)?;
        if buffer.len() != self.image.block_bytes() {
            return Err(crate::internal!("read_block: a {}-byte buffer", buffer.len()));
        }
        posio::read_exact_at(&self.files[chip], buffer, (block * self.image.block_bytes()) as u64)?;
        xor(buffer, self.image.stride(), &self.words);
        Ok(())
    }

    pub fn write(&self, chip: usize, page: usize, record: &[u8]) -> Result<()> {
        self.check(chip, page)?;
        if record.len() != self.image.stride() {
            return Err(crate::internal!("a {}-byte NAND record, not {}", record.len(), self.image.stride()));
        }
        let mut stored = record.to_vec();
        xor(&mut stored, self.image.stride(), &self.words);
        posio::write_all_at(&self.files[chip], &stored, self.offset(page))?;
        Ok(())
    }

    pub fn sync(&self) -> Result<()> {
        for f in &self.files {
            f.sync_all().map_err(|e| crate::internal!("{}: {e}", self.dir.display()))?;
        }
        Ok(())
    }
}

/// A page of a chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Address {
    pub chip: usize,
    pub page: usize,
}

/// An image store read as the guest reads it: the device's live clone (`overlay/nand`) when the overlay holds one of
/// the same geometry, else `base`. A page in a hole reads blank (zeros XOR the blank record), so no extent map is
/// needed. Format 1 page directories are refused (the Swift tool migrates them).
pub fn reader(base: &Path, overlay: Option<&Path>) -> Result<Store> {
    let Some(image) = Image::of(base) else {
        return Err(crate::unsupported!("{}: not a NAND image store (a format 1 page directory?)", base.display()));
    };
    let live = overlay.map(|o| o.join("nand")).filter(|l| Image::of(l).as_ref() == Some(&image));
    Store::open(live.as_deref().unwrap_or(base), false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lt-page-nand-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn geometry_is_the_swift_tools() {
        // the n72-a oracle device's geometry.json (sha256 2b553cbf...)
        let want = "{\n  \"blank\" : \"n72-generated\",\n  \"blocks_per_ce\" : 4096,\n  \"board\" : \"n72ap\",\n  \
                    \"chip_selects\" : 4,\n  \"layout\" : \"n72-generated-v1\",\n  \"page_bytes\" : 4096,\n  \
                    \"pages_per_block\" : 128,\n  \"spare_bytes\" : 64,\n  \"storage_format\" : \"nand-xor-blank-v2\"\n}";
        assert_eq!(Image::n72().geometry(), want);
        use sha2::{Digest, Sha256};
        assert_eq!(
            hex::encode(Sha256::digest(want)),
            "2b553cbfebc7a1615374c25f0c87446532cfd3ca23f9cc60eeac73741ab2bc03"
        );
        assert!(!Image::s5l8900(8).geometry().contains("layout"));
    }

    #[test]
    fn store_round_trip_and_blank_holes() {
        let dir = temp("rt");
        for image in [Image::n72(), Image::s5l8900(4)] {
            let _ = std::fs::remove_dir_all(&dir);
            let store = Store::create(&dir, &image).unwrap();
            assert!(Store::create(&dir, &image).is_err());
            let record: Vec<u8> = (0..image.stride()).map(|i| (i * 7) as u8).collect();
            store.write(1, 300, &record).unwrap();
            store.sync().unwrap();
            drop(store);
            assert_eq!(Image::of(&dir), Some(image.clone()));
            let store = reader(&dir, None).unwrap();
            assert_eq!(store.read(1, 300).unwrap(), record);
            assert_eq!(store.read(0, 300).unwrap(), image.blank_record);
            assert_eq!(std::fs::metadata(dir.join("cs3.pages")).unwrap().len(), image.chip_bytes());
            // stored XOR the blank: a blank page is all zeros on disk
            store.write(2, 5, &image.blank_record).unwrap_err(); // read-only
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
