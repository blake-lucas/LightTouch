//! KBoot: the A4/S5L8920 machines' direct-kernel boot bundle, kboot.bin: what iBoot-817.29 does before it jumps to
//! xnu. A port of FirmwareKit's KBoot (ipad1_kboot.py: build, fill_dt, logo, main); the byte layout is documented there.
//!
//! ```ignore
//! let ident = UnitIdentity::synthesize(seed, "16g", None)?;
//! kboot::write(dec, &out, &ident, kboot::DEFAULT_BOOT_ARGS, None, None)?;        // normal boot
//! kboot::write(dec, &out, &ident, kboot::DEFAULT_BOOT_ARGS, Some(rd), None)?;    // md0 RAM-disk root
//! let img = kboot::build(&mach, &dt, kboot::DEFAULT_BOOT_ARGS, &ident, iboot, None)?;
//! kboot::bundle(&img, &logo)              // image + K48SEG segments + K48KBOOT trailer
//! kboot::iboot_version(Some(&iboot))      // chosen/firmware-version: "iBoot-N.N" out of the decrypted iBoot
//! MachO::new(&kernel)?.segments / .entry()?
//! ```
//! `decrypted`: a decrypt output directory (kernelcache.mach, DeviceTree.bin, iBoot.bin, AppleLogo.bin). The kernel
//! virtual base is the kernelcache's own link base (0xC0000000 on 3.x, 0x80000000 on 4.x). RAM-disk mode puts the image
//! after the kernel, adds chosen/memory-map RAMDisk, appends rd=md0 and empties chosen/root-matching; the DT's own
//! secure-root-prefix is left as the IPSW has it, so md0 is a SecureRoot.
//!
//! What differs per board is a `Board`, picked from the DT's own compatible (ipad1_kboot.BOARDS): K48's landscape panel
//! (display-rotation 270, 4.x lays its UI out by it) or N81's portrait Retina one. A board without the SPI NOR (N81:
//! boot-from-nand) gets K48's 4.x nor-flash subtree grafted in (`graft_nor`). The S5L8920 family (s5l8920_kboot.py) is
//! Boards too: the iPhone 3GS (N88) and the S5L8922 iPod touch 3G (N18), each with its own platform-name/chip-id, a
//! 320x480 panel and the iPad's clock table cut to its DT's 32 slots; N18 gets the NOR graft, N88 keeps its own NOR and
//! has its baseband node's identity filled with test values.
use crate::devicetree::{self, DeviceTree, Value};
use crate::error::Result;
use crate::identity::UnitIdentity;
use std::path::Path;

pub const PHYS_BASE: u32 = 0x4000_0000;
pub const DRAM_SIZE: u32 = 0x1000_0000;
pub const PRAM_SIZE: u32 = 0x4000;
pub const VRAM_SIZE: u32 = 0x90_0000 - 0x4000;
pub const MEM_SIZE: u32 = DRAM_SIZE - PRAM_SIZE - VRAM_SIZE;
pub const VRAM_PA: u32 = PHYS_BASE + MEM_SIZE;
pub const PRAM_PA: u32 = PHYS_BASE + DRAM_SIZE - PRAM_SIZE;
pub const FB_WIDTH: usize = 1024;
pub const FB_HEIGHT: usize = 768;
pub const FB_DEPTH: usize = 32;
/// ipad1_kboot.DEFAULT_BOOT_ARGS. enable-hsic=1: 4.x's AppleS5L8930XUSBArbitrator::handleStart publishes the USB host
/// nubs for the DT's hsic-enabled only when this boot-arg is 1 (no USB keyboard without it); 3.x ignores it. serial=3
/// debug=0x8 stay, though a real unit boots without them: they are the whole serial log, which prepare's one-shots read
/// their markers from (FTL_Open, it_seal, it_keybag), the app's boot stages and Device Console show, and diagnostics
/// carry a panic in. Dropping them gains nothing measurable.
pub const DEFAULT_BOOT_ARGS: &str = "serial=3 debug=0x8 amfi_allow_any_signature=1 cs_enforcement_disable=1 enable-hsic=1";
pub const DEFAULT_IBOOT_VERSION: &str = "iBoot-817.29";
pub const ROOT_MATCHING: &str = "<dict><key>IOProviderClass</key><string>IOMedia</string><key>IOPropertyMatch</key>\
<dict><key>Partition ID</key><integer>1</integer></dict></dict>";

// Measured on a real iPad 1 running 7B500: cpu/memory 0, bus/peripheral 100 MHz, fixed/timebase 24 MHz.
const CPU_HZ: u32 = 0;
const MEM_HZ: u32 = 0;
const BUS_HZ: u32 = 100_000_000;
const PERIPH_HZ: u32 = 100_000_000;
const FIXED_HZ: u32 = 24_000_000;
const TIMEBASE_HZ: u32 = 24_000_000;
const USBPHY_HZ: u32 = 24_000_000;
/// /arm-io/audio-complex ncoref-frequency (S5L8920/8922 only): PLL1's 162 MHz, as iBoot fills it (HostRuntime's
/// PreparedDeviceBoot.ncorefHz).
pub const NCOREF_HZ: u32 = 162_000_000;

/// The iPad's measured cpu0 rates in every slot.
fn default_clocks() -> Vec<u32> {
    let mut c = vec![PERIPH_HZ; 55];
    for (i, hz) in [(0, TIMEBASE_HZ), (5, CPU_HZ), (6, PERIPH_HZ), (27, MEM_HZ), (32, BUS_HZ), (33, FIXED_HZ)] {
        c[i] = hz;
    }
    c
}

/// A real iPad's clock-frequencies on 3.2.2 (iBoot-817), slot for slot: the PLLs (1 APLL 1 GHz, 3 EPLL 1026 MHz), the
/// performance clocks, FMI's 102.6 MHz (33), the panel's 68.4 MHz pixel clock (39), the SPI (47-51) and I2C (52-54)
/// references; but slot 44, which 3.x's iBoot-817 sets to 24 MHz and 4.x's iBoots leave 0. iBoot on the emulated PMGR
/// fills the same table. The slots are the same in every A4 DeviceTree (the nodes' clock-ids), so one table serves all
/// releases.
const K48_CLOCKS: [u32; 55] = [
    24_000_000, 1_000_000_000, 0, 1_026_000_000, 0, 0, 0, 1_000_000_000, 0, 1_000_000_000, 200_000_000, 102_600_000,
    1_026_000_000, 166_666_666, 68_400_000, 200_000_000, 100_000_000, 0, 102_600_000, 51_300_000, 200_000_000,
    48_857_142, 0, 2_000_000, 342_000_000, 0, 200_000_000, 100_000_000, 200_000_000, 166_666_666, 200_000_000,
    171_000_000, 100_000_000, 102_600_000, 100_000_000, 100_000_000, 68_400_000, 54_000_000, 0, 68_400_000,
    342_000_000, 51_300_000, 0, 51_300_000, 0, 0, 12_000_000, 24_000_000, 24_000_000, 24_000_000, 24_000_000,
    24_000_000, 1_000_000, 1_000_000, 1_000_000,
];

/// A real iPhone 4's on 4.3 (iBoot-1072.58): the iPad's but for its 800 MHz APLL (1, 7, 9), MPLL at 500 MHz (2),
/// 160 MHz for 166.67 (13, 29), 51.3 MHz for 68.4 (14, 36) and 48.86 MHz in 49.
fn n90_clocks() -> Vec<u32> {
    let mut c = K48_CLOCKS.to_vec();
    for (i, hz) in [
        (1, 800_000_000),
        (2, 500_000_000),
        (7, 800_000_000),
        (9, 800_000_000),
        (13, 160_000_000),
        (14, 51_300_000),
        (29, 160_000_000),
        (36, 51_300_000),
        (49, 48_857_142),
    ] {
        c[i] = hz;
    }
    c
}

/// What iBoot fills the same on every A4 board: arm-io/pmgr's bridge-settings (the fabric bridges' settings, the first
/// 168 of its 256 bytes) and otgphyctrl's phy-tuning-val. arm-io/pmgr/device-clocks is not one of them: iBoot hands the
/// IPSW's candidate parents over and the kernel settles them.
const A4_BRIDGE_SETTINGS: &str = concat!(
    "000000000c0f0000040000000c0f00000c0401000101000100000000000000000000020008020000",
    "040002000206000008000200020600000c000200020600000c040300000100000c04030001010001",
    "0c040300020100020c040300040100040c040300050100050c040300060100060c04030007010007",
    "0804030003010003000000000000000000000400000c000004000400001000000800040004040000",
    "0804050002010002"
);
const A4_PHY_TUNING: u32 = 0x733;

/// NAND geometry and timings as iBoot probes the emulated flash (16 GB, eight Hynix dies; iBoot-817's and iBoot-931's
/// values for its read ID); only the keys the DT has are written.
const NAND: [(&str, u32); 30] = [
    ("#ce", 8),
    ("#die-ce", 1),
    ("#ce-blocks", 0x1000),
    ("#block-pages", 128),
    ("#page-bytes", 4096),
    ("#spare-bytes", 0x80),
    ("device-readid", 0xB614_D5AD),
    ("vendor-type", 0x10_0014),
    ("#databus", 2),
    ("ecc-correctable", 16),
    ("ecc-threshold", 12),
    ("bbt-format", 3),
    ("read-cycle-ns", 25),
    ("read-setup-ns", 12),
    ("read-hold-ns", 10),
    ("read-delay-ns", 20),
    ("read-valid-ns", 15),
    ("write-cycle-ns", 25),
    ("write-setup-ns", 12),
    ("write-hold-ns", 10),
    // 4.x DTs time the bus in clocks.
    ("read-setup-clks", 1),
    ("read-hold-clks", 1),
    ("read-dccycle-clks", 1),
    ("write-setup-clks", 1),
    ("write-hold-clks", 1),
    ("meta-per-logical-page", 12),
    ("valid-meta-per-logical-page", 10),
    ("logical-page-size", 4096),
    ("ppn-device", 0),
    // iBoot-1219 (5.x): the populated CEs numbered across the buses (bus b's at 8b + n); AppleIOPFMI-49's
    // _fmiInitVirtToPhysMap loops forever on an empty one. 4 CEs on each of 2 buses.
    ("ce-bitmap", 0x0F0F),
];
/// The nodes the geometry goes in: iBoot-1219 DTs carry it on flash-controller0 itself as well as on its disk.
const NAND_NODES: [&str; 2] = ["arm-io/flash-controller0", "arm-io/flash-controller0/disk"];
const MODEL: [(&str, &str); 2] = [("model-number", "MB292"), ("region-info", "LL/A")];

/// One kboot board: what iBoot would put in its DT. (The machine that runs it is the emulator's.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Board {
    pub fb_width: usize,
    pub fb_height: usize,
    pub rotation: u32,
    pub scale: u32,
    pub board_id: u32,
    pub model_number: &'static str,
    /// DRAM bytes: memSize, vram and pram sit at its top, as iBoot puts them.
    pub dram: u32,
    /// A radio board (N90) keeps its baseband node; the machine's `baseband` property unmatches it at boot.
    pub radio: bool,
    /// The machine answers the Bluetooth HCI on the UART the DT hangs the board's bluetooth node off (qemu-ios bt_uart),
    /// so the preparation keeps BTServer. Without it, BTServer is disabled: BlueTool's HCI_Reset would never complete.
    /// The A4 boards' UART is silent: their serial driver reads it through CDMA v2's pause-and-resume, which qemu-ios
    /// doesn't model yet.
    pub bluetooth_hci: bool,
    /// The SoC as iBoot names it (root platform-name, chosen/chip-id).
    pub platform_name: &'static str,
    pub chip_id: u32,
    /// /product/product-id, which iBoot fills and the IPSW DT reserves zeroed (6.x MobileGestalt's product hash). 6.x
    /// GraphicsServices picks the font cache by it: the 3GS's selects CGFontCacheUR.plist, the only set its rootfs ships
    /// fonts for; zeroed, the default plist names fonts the 3GS IPSW lacks and every UIFont and bitmap context comes back
    /// nil. None: leave the slot alone.
    pub product_id: Option<&'static [u8]>,
    /// arm-io/clock-frequencies (cut to the DT's slots): the iPad's measured cpu0 rates in every slot, unless the
    /// board's own were read off a real unit.
    pub clocks: Vec<u32>,
    /// arm-io/i2c0/pmu swi-vcores, the CPU's two core voltages in mV, and arm-io/clcd dot-pitch, the panel's pixels per
    /// inch, as iBoot fills them (None: the IPSW's zeros).
    pub swi_vcores: Option<[u32; 2]>,
    pub dot_pitch: Option<u32>,
    /// arm-io/chip-revision (the A4 stepping: 0x11, B1, on the iPad; 0x20 on the iPhone 4).
    pub chip_revision: u32,
    /// The panel as iBoot's pinot_init reads it: lcd-panel-id, raw-panel-id's bytes, and the backlight-id that picks
    /// AppleARMBacklight's device-specific brightness table (None: the IPSW's 0xffffffff, the default table).
    pub panel_id: u32,
    pub raw_panel_id: Option<&'static [u8]>,
    pub backlight_id: Option<u32>,
}

const N88_PRODUCT_ID: [u8; 20] =
    [0x87, 0x84, 0xae, 0x8d, 0x70, 0x66, 0xb0, 0xf0, 0x13, 0x6b, 0xe9, 0x1d, 0xcf, 0xe6, 0x32, 0xa4, 0x36, 0xff, 0xd6, 0xfb];

impl Board {
    fn base(fb_width: usize, fb_height: usize, rotation: u32, scale: u32, board_id: u32, model_number: &'static str) -> Board {
        Board {
            fb_width,
            fb_height,
            rotation,
            scale,
            board_id,
            model_number,
            dram: DRAM_SIZE,
            radio: false,
            bluetooth_hci: false,
            platform_name: "s5l8930x",
            chip_id: 0x8930,
            product_id: None,
            clocks: default_clocks(),
            swi_vcores: None,
            dot_pitch: None,
            chip_revision: 0x11,
            panel_id: 0x00A1_D13C,
            raw_panel_id: None,
            backlight_id: None,
        }
    }

    pub fn k48() -> Board {
        Board {
            clocks: K48_CLOCKS.to_vec(),
            swi_vcores: Some([950, 1150]),
            dot_pitch: Some(132),
            panel_id: 0xA1E5_06C9,
            raw_panel_id: Some(&[0xA1, 0xE5, 0x69, 0x09, 0, 0, 0, 0, 0, 0, 0, 0, 0x64]),
            backlight_id: Some(6),
            ..Board::base(1024, 768, 270, 1, 0x02, "MB292")
        }
    }

    pub fn n81() -> Board {
        Board::base(640, 960, 0, 2, 0x08, "MC540")
    }

    pub fn n90() -> Board {
        Board {
            dram: 0x2000_0000,
            radio: true,
            clocks: n90_clocks(),
            swi_vcores: Some([950, 1050]),
            dot_pitch: Some(326),
            chip_revision: 0x20,
            panel_id: 0xA18A_05C8,
            raw_panel_id: Some(&[0xA1, 0x8A, 0x58, 0x09, 0x07]),
            backlight_id: Some(5),
            ..Board::base(640, 960, 0, 2, 0x00, "MC603")
        }
    }

    /// iPhone 3GS (S5L8920): -M n88, model MB715. Its baseband node is unmatched (no modem model yet).
    pub fn n88() -> Board {
        Board {
            bluetooth_hci: true,
            platform_name: "s5l8920x",
            chip_id: 0x8920,
            product_id: Some(&N88_PRODUCT_ID),
            ..Board::base(320, 480, 0, 1, 0x00, "MB715")
        }
    }

    /// iPod touch 3G (S5L8922): -M n18, model MC008; NOR-less, so it takes the graft.
    pub fn n18() -> Board {
        Board { bluetooth_hci: true, platform_name: "s5l8922x", chip_id: 0x8922, ..Board::base(320, 480, 0, 1, 0x02, "MC008") }
    }

    /// By the catalog's board name (K48Recipe.a4): K48 for anything else.
    pub fn for_board(board: &str) -> Board {
        match board {
            "n81ap" => Board::n81(),
            "n90ap" => Board::n90(),
            "n88ap" => Board::n88(),
            "n18ap" => Board::n18(),
            _ => Board::k48(),
        }
    }

    /// The S5L8920 family (-M n18, n88): no metadata whitening in its DTs, the IPSW's NAND epoch, no USB host.
    pub fn is_s5l8920(&self) -> bool {
        self.platform_name != "s5l8930x"
    }

    /// From the DT's compatible ("N81AP\0iPod4,1\0AppleARM" -> n81); K48 otherwise.
    pub fn of(dt: &DeviceTree) -> Board {
        let first = dt
            .value("", "compatible")
            .map(|v| String::from_utf8_lossy(&v[..v.iter().position(|&c| c == 0).unwrap_or(v.len())]).into_owned())
            .unwrap_or_default();
        match first.as_str() {
            "N81AP" => Board::n81(),
            "N90AP" => Board::n90(),
            "N88AP" => Board::n88(),
            "N18AP" => Board::n18(),
            _ => Board::k48(),
        }
    }

    pub fn mem_size(&self) -> u32 {
        self.dram - PRAM_SIZE - VRAM_SIZE
    }

    pub fn vram_pa(&self) -> u32 {
        PHYS_BASE + self.mem_size()
    }

    /// iBoot puts the A4's PRAM at the top of its 512 MiB DRAM window whatever the DRAM (0x5fffc000 on the 256 MiB iPad
    /// too, where the window mirrors DRAM); the S5L8920's at the top of its DRAM.
    pub fn pram_pa(&self) -> u32 {
        PHYS_BASE + if self.is_s5l8920() { self.dram } else { 0x2000_0000 } - PRAM_SIZE
    }
}

/// K48's 4.x (8C148) spi0/nor-flash subtree: diagnostics, nvram, the image area and effaceable storage
/// (ipad1_kboot.NOR_GRAFT). Phandles are K48's.
fn nor_graft() -> Vec<(&'static str, &'static str, Vec<(&'static str, Value)>)> {
    let st = |s: &str| Value::Str(s.to_string());
    vec![
        (
            "arm-io/spi0",
            "nor-flash",
            vec![
                ("compatible", st("nor-flash,spi")),
                ("#address-cells", Value::U32(1)),
                ("device_type", st("nor-flash")),
                ("#size-cells", Value::U32(1)),
                ("ranges", Value::Words(vec![0, 0, 0x10_0000])),
                ("reg", Value::Words(vec![0, 0x53, 0x0801_0000, 0, 0, 0, 0, 0])),
                ("AAPL,phandle", Value::U32(0x0091_70E0)),
            ],
        ),
        (
            "arm-io/spi0/nor-flash",
            "diagnostic-data",
            vec![
                ("compatible", st("diagnostic-data,format1")),
                ("device_type", st("diagnostic-data")),
                ("reg", Value::Words(vec![0x6000, 0x2000, 0x4000, 0x2000])),
                ("AAPL,phandle", Value::U32(0x0091_74F0)),
            ],
        ),
        (
            "arm-io/spi0/nor-flash",
            "nvram",
            vec![
                ("compatible", st("nvram,chrp")),
                ("device_type", st("nvram")),
                ("reg", Value::Words(vec![0xF_C000, 0x2000, 0xF_E000, 0x2000])),
                ("AAPL,phandle", Value::U32(0x0091_7880)),
            ],
        ),
        (
            "arm-io/spi0/nor-flash",
            "raw-device",
            vec![
                ("compatible", st("raw-device,non-nvram")),
                ("device_type", st("raw-device")),
                ("reg", Value::Words(vec![0x8000, 0xF_2000, 0, 0x1000])),
                ("AAPL,phandle", Value::U32(0x0091_7860)),
            ],
        ),
        (
            "arm-io/spi0/nor-flash",
            "effaceable",
            vec![
                ("compatible", st("effaceable,nor")),
                ("device_type", st("effaceable")),
                ("reg", Value::Words(vec![0xF_A000, 0x1000, 0xF_B000, 0x1000])),
                ("AAPL,phandle", Value::U32(0x0091_7F70)),
            ],
        ),
    ]
}

/// A NOR-less board: graft the NOR in. Either way take the NAND off boot duty (the N88 has its own NOR and the
/// property too): the kernel keys off boot-from-nand's presence, not its value (IOFlashStorageDevice then hunts NAND
/// boot blocks and the FTL never finds root); the editor cannot delete, so rename it.
pub fn graft_nor(dt: &mut DeviceTree) -> Result<()> {
    if !dt.contains("arm-io/spi0") {
        return Ok(());
    }
    if !dt.contains("arm-io/spi0/nor-flash") {
        for (parent, name, props) in nor_graft() {
            dt.add_node(parent, name, &props)?;
        }
    }
    landing_map(dt)?;
    if dt.has("arm-io/flash-controller0/disk", "boot-from-nand") {
        dt.rename("arm-io/flash-controller0/disk", "boot-from-nand", "boot-from-nor")?;
    }
    Ok(())
}

/// 3.x's FMI groups the CEs into buses by the disk node's landing-map words, one CE mask per bus (3.1.3 findNandInfo);
/// the IPSW's DT has a single word, so every CE folds into one bus and its board table has no match ("2-bus not
/// supported"). One word per populated bus from the disk's CE bitmap (reg), as s5l8920_kboot.py does; DTs without
/// landing-map (4.x on) are untouched.
pub fn landing_map(dt: &mut DeviceTree) -> Result<()> {
    let disk = "arm-io/flash-controller0/disk";
    if !dt.has(disk, "landing-map") {
        return Ok(());
    }
    let Some(reg) = dt.value(disk, "reg").filter(|r| r.len() >= 4) else { return Ok(()) };
    let ces = u32le(&reg, 0);
    let words: Vec<u32> = (0..4).map(|i| ces & (0xFF << (8 * i))).filter(|&w| w != 0).collect();
    dt.rename(disk, "landing-map", "landing-map-dt")?;
    dt.add(disk, "landing-map", &devicetree::le(&words))
}

/// A physical range the bundle loads: `data` None zero-fills it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub pa: u32,
    pub length: u32,
    pub data: Option<Vec<u8>>,
}

/// The flat physical image, its load PA, entry PA and boot_args PA.
#[derive(Debug, Clone)]
pub struct Image {
    pub image: Vec<u8>,
    pub load_pa: u32,
    pub entry_pa: u32,
    pub boot_args_pa: u32,
}

fn u32le(d: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([d[at], d[at + 1], d[at + 2], d[at + 3]])
}

/// The first offset of `needle` in `hay`.
pub(crate) fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    let first = needle[0];
    let last = hay.len() - needle.len();
    let mut i = 0;
    while i <= last {
        match hay[i..=last].iter().position(|&b| b == first) {
            None => return None,
            Some(p) => {
                i += p;
                if &hay[i..i + needle.len()] == needle {
                    return Some(i);
                }
                i += 1;
            }
        }
    }
    None
}

/// The first "iBoot-N(.N)*" in a decrypted iBoot, else the default.
pub fn iboot_version(iboot: Option<&[u8]>) -> String {
    let Some(b) = iboot else { return DEFAULT_IBOOT_VERSION.into() };
    let tag = b"iBoot-";
    let digit = |c: u8| c.is_ascii_digit();
    let mut i = 0;
    while i + tag.len() < b.len() {
        if &b[i..i + tag.len()] == tag && digit(b[i + tag.len()]) {
            let mut j = i + tag.len();
            while j < b.len() && digit(b[j]) {
                j += 1;
            }
            while j + 1 < b.len() && b[j] == b'.' && digit(b[j + 1]) {
                j += 1;
                while j < b.len() && digit(b[j]) {
                    j += 1;
                }
            }
            return String::from_utf8_lossy(&b[i..j]).into_owned();
        }
        i += 1;
    }
    DEFAULT_IBOOT_VERSION.into()
}

type Props = Vec<(&'static str, Value)>;

/// (root props, chosen props, [(node, local-mac-address)]) for an identity.
fn identity_dt(id: &UnitIdentity, board: &Board) -> Result<(Props, Props, Vec<(&'static str, Vec<u8>)>)> {
    let need = |k: &str| id.get(k).map(str::to_string).ok_or_else(|| crate::unsupported!("identity: missing {k}"));
    let hex = |s: &str| -> Result<u64> {
        let t = if s.to_lowercase().starts_with("0x") { &s[2..] } else { s };
        u64::from_str_radix(t, 16).map_err(|_| crate::unsupported!("identity: {s} is not hex"))
    };
    let mac = |s: &str| hex::decode(s.replace(':', "")).map_err(|_| crate::unsupported!("identity: bad MAC {s}"));
    let ecid = hex(&need("unique-chip-id")?)?;
    let die = id.die_id().filter(|d| d.len() == 2).ok_or_else(|| crate::unsupported!("identity: missing die-id"))?;
    let mut die_words = Vec::new();
    for w in die {
        let v = hex(w)?;
        if v > u32::MAX as u64 {
            return Err(crate::unsupported!("identity: die-id word {w} is over 32 bits"));
        }
        die_words.push(v as u32);
    }
    let mut root: Props = vec![
        ("serial-number", Value::Str(need("serial-number")?)),
        ("mlb-serial-number", Value::Str(need("mlb-serial-number")?)),
    ];
    for (k, v) in MODEL {
        let fallback = if k == "model-number" { board.model_number } else { v };
        root.push((k, Value::Str(id.get(k).unwrap_or(fallback).to_string())));
    }
    let chosen: Props = vec![
        ("unique-chip-id", Value::Words(vec![(ecid & 0xFFFF_FFFF) as u32, (ecid >> 32) as u32])),
        ("die-id", Value::Words(die_words)),
    ];
    // Bluetooth hangs off whichever UART the board wires it to (K48 uart3, N81 uart1): fill_dt finds it.
    Ok((root, chosen, vec![("arm-io/sdio", mac(&need("wifi-mac")?)?), ("bluetooth", mac(&need("bt-mac")?)?)]))
}

/// An empty NVRAM image as IODTNVRAM parses it (ipad1_kboot.nvram_image): CHRP partitions, a 2 KiB "common" (0x70),
/// the rest "free" (0x7f). Each 16-byte header is sig, checksum, length in 16-byte units, 12-byte name; the checksum
/// adds byte 0 and bytes 2-15 with end-around carry. A zeroed image is a zero-length partition, and initNVRAMImage
/// loops on it forever.
pub fn nvram_image(size: usize) -> Vec<u8> {
    fn part(sig: u8, name: &str, units: usize) -> Vec<u8> {
        let mut h = vec![sig, 0, (units & 0xFF) as u8, (units >> 8) as u8];
        h.extend_from_slice(name.as_bytes());
        h.resize(16, 0);
        let mut c = h[0] as u32;
        for &x in &h[2..] {
            c += x as u32;
            if c > 0xFF {
                c = (c & 0xFF) + 1;
            }
        }
        h[1] = c as u8;
        h.resize(units * 16, 0);
        h
    }
    let common = 0x80;
    let mut out = part(0x70, "common", common);
    out.extend(part(0x7F, "free", size / 16 - common));
    out
}

fn fill_dt(dt: &mut DeviceTree, memory_map: &[(String, u32, u32)], identity: &UnitIdentity, iboot: &str, root_matching: &str) -> Result<()> {
    let board = Board::of(dt);
    let (root, chosen, macs) = identity_dt(identity, &board)?;
    dt.set("", "platform-name", Value::Str(board.platform_name.into()))?;
    for (k, v) in root {
        dt.set("", k, v)?;
    }
    // debug-enabled 1, where a real unit has 0: the kernel honors the AMFI boot-args (amfi_allow_any_signature,
    // cs_enforcement_disable) only with it, and an n90ap-8C148 boot never lit with it 0.
    let mut chosen_props: Props = ["debug-enabled", "production-cert", "secure-boot", "gid-aes-key", "uid-aes-key", "system-trusted"]
        .into_iter()
        .map(|k| (k, Value::U32(1)))
        .collect();
    chosen_props.push(("board-id", Value::U32(board.board_id)));
    chosen_props.push(("chip-id", Value::U32(board.chip_id)));
    chosen_props.extend(chosen);
    chosen_props.push(("firmware-version", Value::Str(iboot.into())));
    chosen_props.push(("display-rotation", Value::U32(board.rotation)));
    chosen_props.push(("display-scale", Value::U32(board.scale)));
    chosen_props.push(("root-matching", Value::Str(root_matching.into())));
    for (k, v) in chosen_props {
        // 3.1.3's DTs (N88 7E18) have none of these: nothing to fill.
        if ["die-id", "display-rotation", "display-scale"].contains(&k) && !dt.has("chosen", k) {
            continue;
        }
        dt.set("chosen", k, v)?;
    }
    // iBoot-1537/1940 (6.x/7.x) hand NVRAM over as /chosen/nvram-proxy-data; the IPSW DT reserves it zeroed.
    if let Some(slot) = dt.slot("chosen", "nvram-proxy-data") {
        dt.set("chosen", "nvram-proxy-data", Value::Bytes(nvram_image(slot.length)))?;
    }
    // Every iBoot read so far (817, 931, the iPhone 4's 1072) leaves a BOOTREPLY header there, the rest zero.
    if dt.slot("chosen", "bootp-response").is_some_and(|s| s.length > 0) {
        dt.set("chosen", "bootp-response", Value::Bytes(vec![2]))?;
    }
    if let Some(id) = board.product_id
        && dt.slot("product", "product-id").is_some_and(|s| s.length == id.len())
    {
        dt.set("product", "product-id", Value::Bytes(id.to_vec()))?;
    }
    // iBoot-1940 also copies syscfg's MACs to /chosen; 7.x's MobileGestalt reads them there (and hashes them into the UDID).
    for (k, node) in [("mac-address-wifi0", "arm-io/sdio"), ("mac-address-bluetooth0", "bluetooth")] {
        if dt.has("chosen", k)
            && let Some((_, mac)) = macs.iter().find(|(n, _)| *n == node)
        {
            dt.set("chosen", k, Value::Bytes(mac.clone()))?;
        }
    }
    for (k, hz) in [
        ("clock-frequency", CPU_HZ),
        ("memory-frequency", MEM_HZ),
        ("bus-frequency", BUS_HZ),
        ("peripheral-frequency", PERIPH_HZ),
        ("fixed-frequency", FIXED_HZ),
        ("timebase-frequency", TIMEBASE_HZ),
    ] {
        dt.set("cpus/cpu0", k, Value::U32(hz))?;
    }
    let slots = dt.slot("arm-io", "clock-frequencies").map_or(board.clocks.len() * 4, |s| s.length) / 4;
    dt.set("arm-io", "clock-frequencies", Value::Words(board.clocks.iter().copied().take(slots).collect()))?;
    dt.set("arm-io", "usbphy-frequency", Value::U32(USBPHY_HZ))?;
    if !board.is_s5l8920() {
        let bridge = hex::decode(A4_BRIDGE_SETTINGS).unwrap_or_default();
        if dt.slot("arm-io/pmgr", "bridge-settings").is_some_and(|s| s.length >= bridge.len()) {
            dt.set("arm-io/pmgr", "bridge-settings", Value::Bytes(bridge))?;
        }
        if dt.has("arm-io/otgphyctrl", "phy-tuning-val") {
            dt.set("arm-io/otgphyctrl", "phy-tuning-val", Value::U32(A4_PHY_TUNING))?;
        }
    }
    if let Some(mv) = board.swi_vcores
        && dt.has("arm-io/i2c0/pmu", "swi-vcores")
    {
        dt.set("arm-io/i2c0/pmu", "swi-vcores", Value::Words(mv.to_vec()))?;
    }
    if let Some(ppi) = board.dot_pitch
        && dt.has("arm-io/clcd", "dot-pitch")
    {
        dt.set("arm-io/clcd", "dot-pitch", Value::U32(ppi))?;
    }
    if dt.has("arm-io/audio-complex", "ncoref-frequency") {
        dt.set("arm-io/audio-complex", "ncoref-frequency", Value::U32(NCOREF_HZ))?;
    }
    if dt.contains("arm-io/sgx") {
        dt.set("arm-io/sgx", "compatible", Value::Str("none".into()))?; // no SGX model
    }
    for (want, mac) in &macs {
        let suffix = format!("/{want}");
        let path = dt.props.keys().find(|p| p.as_str() == *want || p.ends_with(&suffix)).cloned().unwrap_or_else(|| want.to_string());
        if dt.contains(&path) {
            dt.set(&path, "local-mac-address", Value::Bytes(mac.clone()))?;
        }
    }
    let lcd = "arm-io/mipi-dsim/lcd";
    if dt.contains(lcd) {
        // the panel id iBoot's pinot_init writes; the DSI model's reply. 3.0's DTs (N88 7A341) have no raw-panel-id slot
        if dt.has(lcd, "lcd-panel-id") {
            dt.set(lcd, "lcd-panel-id", Value::U32(board.panel_id))?;
        }
        if dt.has(lcd, "raw-panel-id") {
            let v = board.raw_panel_id.map_or(Value::U32(board.panel_id), |b| Value::Bytes(b.to_vec()));
            dt.set(lcd, "raw-panel-id", v)?;
        }
    }
    if let Some(id) = board.backlight_id
        && dt.has("backlight", "backlight-id")
    {
        dt.set("backlight", "backlight-id", Value::U32(id))?;
    }
    if dt.contains("baseband") && !board.radio {
        // Wi-Fi iPad: no radio, so unmatch and unname the N82 baseband node
        for (k, v) in [("compatible", "none"), ("device_type", "none"), ("name", "nobb")] {
            dt.set("baseband", k, Value::Str(v.into()))?;
        }
        // lockdownd still reads the node's identity (N88): GSMA's test IMEI and a placeholder serial, so nothing passes
        // for a real unit (s5l8920_kboot.py).
        if dt.has("baseband", "device-imei") {
            dt.set("baseband", "device-imei", Value::Str("004999010640000".into()))?;
            if dt.has("baseband", "snum") {
                // 3.0's DT has none
                dt.set("baseband", "snum", Value::Bytes(b"TESTSNUM0000".to_vec()))?;
            }
        }
    }
    if dt.has("arm-io", "chip-revision") {
        dt.set("arm-io", "chip-revision", Value::U32(board.chip_revision))?;
    }
    for node in NAND_NODES {
        let Some(props) = dt.props.get(node).cloned() else { continue };
        for (k, v) in &NAND {
            if props.contains_key(*k) {
                dt.set(node, k, Value::U32(*v))?;
            }
        }
    }
    dt.set("pram", "reg", Value::Words(vec![board.pram_pa(), PRAM_SIZE]))?;
    dt.set("vram", "reg", Value::Words(vec![board.vram_pa(), VRAM_SIZE]))?;
    for (i, (name, pa, size)) in memory_map.iter().enumerate() {
        dt.rename("chosen/memory-map", &format!("MemoryMapReserved-{i}"), name)?;
        dt.set("chosen/memory-map", name, Value::Words(vec![*pa, *size]))?;
    }
    Ok(())
}

/// The flat physical image, its load PA, entry PA and boot_args PA. `ramdisk`: raw HFS to boot as md0.
pub fn build(kernel: &[u8], device_tree: &[u8], boot_args: &str, identity: &UnitIdentity, iboot: &str, ramdisk: Option<&[u8]>) -> Result<Image> {
    let page = |n: usize| (n + 0xFFF) & !0xFFF;
    let m = MachO::new(kernel)?;
    let segs: Vec<&MachSegment> = m.segments.iter().filter(|s| s.name != "__PAGEZERO").collect();
    let (Some(lowest), Some(end)) =
        (segs.iter().map(|s| s.vmaddr).min(), segs.iter().map(|s| s.vmaddr as usize + s.vmsize as usize).max())
    else {
        return Err(crate::unsupported!("kernelcache has no segments"));
    };
    let vbase = (lowest & 0xF000_0000) as usize;
    let pa = |va: usize| (va as i64 - vbase as i64 + PHYS_BASE as i64) as u32;
    let mut dt = DeviceTree::new(device_tree)?;
    let board = Board::of(&dt);
    graft_nor(&mut dt)?;
    // Host nubs (EHCI, OHCI0) up at arbitrator start, next to device mode (qemu-ios docs/ipad1/usb-keyboard.md).
    if dt.contains("arm-io/usb-complex") {
        dt.add("arm-io/usb-complex", "hsic-enabled", &[])?;
    }
    let dt_len = dt.data().len();
    let mut top = page(end);
    let rd_va = top;
    let mut args = boot_args.to_string();
    if let Some(rd) = ramdisk {
        top += page(rd.len());
        args.push_str(" rd=md0");
    }
    let dt_va = top;
    let args_va = top + page(dt_len);
    let end_va = args_va + 0x1000;
    let top_of_kernel = pa((end_va + 0x3FFF) & !0x3FFF);

    let mut image = vec![0u8; end_va - vbase];
    let mut memory_map: Vec<(String, u32, u32)> = Vec::new();
    for s in &segs {
        let n = s.filesize.min(s.vmsize) as usize;
        let at = s.vmaddr as usize - vbase;
        if s.fileoff as usize + n > kernel.len() {
            return Err(crate::unsupported!("kernelcache segment {} runs past the file", s.name));
        }
        let from = s.fileoff as usize;
        image[at..at + n].copy_from_slice(&kernel[from..from + n]);
        memory_map.push((format!("Kernel-{}", s.name), pa(s.vmaddr as usize), s.vmsize));
    }
    if let Some(rd) = ramdisk {
        image[rd_va - vbase..rd_va - vbase + rd.len()].copy_from_slice(rd);
        memory_map.push(("RAMDisk".into(), pa(rd_va), rd.len() as u32));
    }
    memory_map.push(("DeviceTree".into(), pa(dt_va), dt_len as u32));
    memory_map.push(("BootArgs".into(), pa(args_va), 0x1000));
    fill_dt(&mut dt, &memory_map, identity, iboot, if ramdisk.is_none() { ROOT_MATCHING } else { "" })?;
    let d = dt.data();
    image[dt_va - vbase..dt_va - vbase + d.len()].copy_from_slice(d);

    // boot_args rev 1 / the version the kernel checks for (2, or 3 from xnu-1735.47). Video: base, display (0 = text
    // console for -v/-s), rowbytes, w, h, depth.
    let verbose = args.split(' ').any(|a| a == "-v" || a == "-s");
    let cmdline = args.as_bytes();
    if cmdline.len() >= 256 {
        return Err(crate::unsupported!("boot-args longer than BOOT_LINE_LENGTH"));
    }
    let mut ba = vec![1, 0, m.boot_args_version(), 0];
    ba.extend(devicetree::le(&[
        vbase as u32,
        PHYS_BASE,
        board.mem_size(),
        top_of_kernel,
        board.vram_pa(),
        if verbose { 0 } else { 1 },
        (board.fb_width * FB_DEPTH / 8) as u32,
        board.fb_width as u32,
        board.fb_height as u32,
        FB_DEPTH as u32 | (board.scale - 1) << 16,
        0,
        dt_va as u32,
        dt_len as u32,
    ]));
    ba.extend_from_slice(cmdline);
    ba.resize(ba.len() + 256 - cmdline.len(), 0);
    image[args_va - vbase..args_va - vbase + ba.len()].copy_from_slice(&ba);
    Ok(Image { image, load_pa: PHYS_BASE, entry_pa: pa(m.entry()? as usize), boot_args_pa: pa(args_va) })
}

/// image + segments ("K48SEG\0\0", pa, len, flags bit 0 = zero-fill, data) + the 24-byte trailer.
pub fn bundle(img: &Image, segments: &[Segment]) -> Vec<u8> {
    let mut out = img.image.clone();
    for s in segments {
        out.extend_from_slice(b"K48SEG\0\0");
        out.extend(devicetree::le(&[s.pa, s.length, if s.data.is_none() { 1 } else { 0 }]));
        if let Some(d) = &s.data {
            out.extend_from_slice(d);
        }
    }
    out.extend_from_slice(b"K48KBOOT");
    out.extend(devicetree::le(&[img.load_pa, img.entry_pa, img.boot_args_pa, img.image.len() as u32]));
    out
}

/// ipad1_kboot.main: kboot.bin's bytes from a decrypted-firmware directory. `kernel_override`: the kernel to boot in
/// place of kernelcache.mach (a jailbroken device's sandbox-patched one).
pub fn make(decrypted: &Path, identity: &UnitIdentity, boot_args: &str, ramdisk: Option<&Path>, kernel_override: Option<&[u8]>) -> Result<Vec<u8>> {
    let file = |n: &str| decrypted.join(n);
    let kernel = match kernel_override {
        Some(k) => k.to_vec(),
        None => std::fs::read(file("kernelcache.mach"))?,
    };
    let dt = std::fs::read(file("DeviceTree.bin"))?;
    let iboot = if file("iBoot.bin").exists() { Some(std::fs::read(file("iBoot.bin"))?) } else { None };
    let rd = match ramdisk {
        Some(p) => Some(std::fs::read(p)?),
        None => None,
    };
    let img = build(&kernel, &dt, boot_args, identity, &iboot_version(iboot.as_deref()), rd.as_deref())?;
    let board = Board::of(&DeviceTree::new(&dt)?);
    let logo = if file("AppleLogo.bin").exists() {
        logo_segments(&std::fs::read(file("AppleLogo.bin"))?, board.vram_pa(), board.fb_width, board.fb_height, board.rotation == 270)?
    } else {
        Vec::new()
    };
    Ok(bundle(&img, &logo))
}

/// `make`, written to `out`.
pub fn write(decrypted: &Path, out: &Path, identity: &UnitIdentity, boot_args: &str, ramdisk: Option<&Path>, kernel_override: Option<&[u8]>) -> Result<()> {
    std::fs::write(out, make(decrypted, identity, boot_args, ramdisk, kernel_override)?)?;
    Ok(())
}

/// Framebuffer segments that put an iBootIm logo where iBoot puts it: centered on black (FirmwareKit's BootLogo).
///
/// iBootIm: "iBootIm\0", adler32, "lzss", format tag (only "grey": gray + inverted alpha, composited over black), u16
/// width, height; LZSS data at 0x40. On a landscape panel (`turn`, K48) the portrait UI arrives turned a quarter
/// counter-clockwise into it (its top along the panel's left edge; the app turns the panel a quarter clockwise to stand
/// it up), so the logo is turned the same way.
pub fn logo_segments(blob: &[u8], framebuffer_pa: u32, fb_w: usize, fb_h: usize, turn: bool) -> Result<Vec<Segment>> {
    let b = blob;
    if b.len() < 0x40 || &b[0..8] != b"iBootIm\0" || &b[12..16] != b"sszl" {
        return Err(crate::unsupported!("not an LZSS iBootIm"));
    }
    if &b[16..20] != b"yerg" {
        return Err(crate::unsupported!("only the grey iBootIm format is handled"));
    }
    let w = b[20] as usize | (b[21] as usize) << 8;
    let h = b[22] as usize | (b[23] as usize) << 8;
    let px = crate::lzss::decompress(&b[0x40..], 0);
    if px.len() < w * h * 2 {
        return Err(crate::unsupported!("short iBootIm"));
    }
    let (lw, lh) = if turn { (h, w) } else { (w, h) }; // the logo as it lands on the panel
    let x0 = (fb_w as i64 - lw as i64) / 2;
    let y0 = (fb_h as i64 - lh as i64) / 2;
    let stride = fb_w * 4;
    if x0 < 0 || y0 < 0 {
        return Err(crate::unsupported!("iBootIm larger than the framebuffer"));
    }
    let (x0, y0) = (x0 as usize, y0 as usize);
    let mut rows = vec![0u8; stride * lh];
    for ly in 0..h {
        for lx in 0..w {
            let gray = px[(ly * w + lx) * 2] as u32;
            let clear = px[(ly * w + lx) * 2 + 1] as u32;
            let v = gray * (255 - clear) / 255;
            let pixel = 0xFF00_0000 | v * 0x010101;
            let at = if turn { (w - 1 - lx) * stride + (x0 + ly) * 4 } else { ly * stride + (x0 + lx) * 4 };
            rows[at] = (pixel & 0xFF) as u8;
            rows[at + 1] = (pixel >> 8 & 0xFF) as u8;
            rows[at + 2] = (pixel >> 16 & 0xFF) as u8;
            rows[at + 3] = 0xFF;
        }
    }
    Ok(vec![
        Segment { pa: framebuffer_pa, length: (stride * fb_h) as u32, data: None },
        Segment { pa: framebuffer_pa + (y0 * stride) as u32, length: rows.len() as u32, data: Some(rows) },
    ])
}

/// An LC_SEGMENT of a 32-bit Mach-O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachSegment {
    pub name: String,
    pub vmaddr: u32,
    pub vmsize: u32,
    pub fileoff: u32,
    pub filesize: u32,
}

/// A 32-bit Mach-O's segments and entry point (imgtools/macho.py, as much as kboot needs).
pub struct MachO<'a> {
    pub segments: Vec<MachSegment>,
    data: &'a [u8],
}

impl<'a> MachO<'a> {
    pub fn new(data: &'a [u8]) -> Result<MachO<'a>> {
        if data.len() < 28 || u32le(data, 0) != 0xFEED_FACE {
            return Err(crate::unsupported!("not a 32-bit Mach-O"));
        }
        let past = || crate::unsupported!("Mach-O load commands run past the file");
        let mut segments = Vec::new();
        let mut off = 28;
        for _ in 0..u32le(data, 16) {
            if off + 8 > data.len() {
                return Err(past());
            }
            let cmd = u32le(data, off);
            let size = u32le(data, off + 4) as usize;
            if cmd == 1 {
                // LC_SEGMENT
                if off + 40 > data.len() {
                    return Err(past());
                }
                let name = &data[off + 8..off + 24];
                let name = &name[..name.iter().position(|&c| c == 0).unwrap_or(16)];
                segments.push(MachSegment {
                    name: String::from_utf8_lossy(name).into_owned(),
                    vmaddr: u32le(data, off + 24),
                    vmsize: u32le(data, off + 28),
                    fileoff: u32le(data, off + 32),
                    filesize: u32le(data, off + 36),
                });
            }
            if size == 0 {
                break;
            }
            off += size;
        }
        Ok(MachO { segments, data })
    }

    /// The boot_args.Version pe_identify_machine demands, read off the kernel's own check (ipad1_kboot.py
    /// boot_args_version): the Thumb pair `ldrh rN, [r0, #2]` (0x8840|N) … `cmp rN, #V` (0x28|N<<8|V) just before the
    /// literal naming "pe_identify_machine: Epoch Mismatch". 2 when the shape is not found (3.2.x, 4.2.1 and 4.3.0 boot
    /// with 2); 4.3.5's xnu-1735.47 and iOS 5's xnu-1878 say 3.
    pub fn boot_args_version(&self) -> u8 {
        let data = self.data;
        let Some(so) = find(data, b"pe_identify_machine: Epoch Mismatch") else { return 2 };
        let Some(seg) =
            self.segments.iter().find(|s| s.fileoff as usize <= so && (so as u64) < s.fileoff as u64 + s.filesize as u64)
        else {
            return 2;
        };
        let sva = seg.vmaddr.wrapping_add(so as u32).wrapping_sub(seg.fileoff);
        if let Some(lit) = find(data, &sva.to_le_bytes()) {
            let window = &data[lit.saturating_sub(0x400)..lit];
            for n in 0..8u8 {
                if window.len() <= 2 {
                    continue;
                }
                let Some(i) = (0..window.len() - 1).rev().find(|&k| window[k] == 0x40 | n && window[k + 1] == 0x88) else {
                    continue;
                };
                for j in (i + 2)..(i + 10).min(window.len()) {
                    if window[j] == 0x28 | n {
                        return window[j - 1];
                    }
                }
            }
        }
        // iOS 6's xnu-2107 reaches the string through movw/movt/add rX, pc, so no literal names it: the pair is
        // `ldrh rN, [r0, #2]; cmp rN, #V` with that sequence within the next 24 bytes (ipad1_kboot 08a698c2f1).
        let bytes = data;
        let h16 = |o: usize| bytes[o] as u32 | (bytes[o + 1] as u32) << 8;
        let imm16 = |hw1: u32, hw2: u32| (hw1 & 0xF) << 12 | ((hw1 >> 10) & 1) << 11 | ((hw2 >> 12) & 7) << 8 | (hw2 & 0xFF);
        let names_string = |at: usize, seg: &MachSegment| -> bool {
            for o in (at..(at + 24).min(bytes.len().saturating_sub(10))).step_by(2) {
                let hw1 = h16(o);
                let hw2 = h16(o + 2);
                if hw1 & 0xFBF0 != 0xF240 {
                    continue; // movw
                }
                let rd = (hw2 >> 8) & 0xF;
                let lo = imm16(hw1, hw2);
                let t1 = h16(o + 4);
                let t2 = h16(o + 6);
                if t1 & 0xFBF0 != 0xF2C0 || (t2 >> 8) & 0xF != rd {
                    continue; // movt, same register
                }
                if h16(o + 8) != 0x4478 | (rd & 7) | ((rd & 8) << 4) {
                    continue; // add rd, pc
                }
                let pc = seg.vmaddr.wrapping_add((o + 8) as u32).wrapping_sub(seg.fileoff).wrapping_add(4);
                return (imm16(t1, t2) << 16 | lo).wrapping_add(pc) == sva;
            }
            false
        };
        for seg in self.segments.iter().filter(|s| s.filesize > 0) {
            let lo = seg.fileoff as usize;
            let hi = ((seg.fileoff as u64 + seg.filesize as u64) as usize).min(bytes.len().saturating_sub(4));
            let mut i = lo;
            while i < hi {
                if bytes[i + 1] == 0x88 && bytes[i] & 0xF8 == 0x40 {
                    let n = bytes[i] & 7;
                    if bytes[i + 3] == 0x28 | n && names_string(i + 6, seg) {
                        return bytes[i + 2];
                    }
                }
                i += 2;
            }
        }
        2
    }

    /// LC_UNIXTHREAD's ARM_THREAD_STATE pc (r15).
    pub fn entry(&self) -> Result<u32> {
        let data = self.data;
        let mut off = 28;
        for _ in 0..u32le(data, 16) {
            if off + 8 > data.len() {
                break;
            }
            let cmd = u32le(data, off);
            let size = u32le(data, off + 4) as usize;
            if cmd == 5 && off + 80 <= data.len() {
                return Ok(u32le(data, off + 16 + 15 * 4));
            }
            if size == 0 {
                break;
            }
            off += size;
        }
        Err(crate::unsupported!("no LC_UNIXTHREAD"))
    }
}

#[cfg(test)]
#[path = "kboot_tests.rs"]
mod tests;
