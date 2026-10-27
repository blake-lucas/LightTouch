//! KBootTests and LZSSTests' logo test, ported: synthetic kernels and DTs through `build`, and the n81 oracle.
use super::*;
use crate::devicetree::tests::{n, node};
use crate::identity::Value as IdValue;

fn z(k: usize) -> Vec<u8> {
    vec![0; k]
}

/// ipad1_kboot.selfcheck's minimal DT, with extra arm-io children and top-level nodes.
fn device_tree(arm_io: &[Vec<u8>], top: &[Vec<u8>]) -> Vec<u8> {
    let mut root: Vec<(&str, Vec<u8>)> = vec![("name", n("device-tree"))];
    for k in ["platform-name", "model-number", "region-info", "serial-number", "mlb-serial-number"] {
        root.push((k, z(32)));
    }
    let mut chosen: Vec<(&str, Vec<u8>)> = vec![
        ("name", n("chosen")),
        ("firmware-version", z(256)),
        ("root-matching", z(256)),
        ("unique-chip-id", z(8)),
        ("die-id", z(8)),
    ];
    for k in [
        "debug-enabled",
        "production-cert",
        "secure-boot",
        "gid-aes-key",
        "uid-aes-key",
        "system-trusted",
        "board-id",
        "chip-id",
        "display-rotation",
        "display-scale",
    ] {
        chosen.push((k, z(4)));
    }
    let names: Vec<String> = (0..16).map(|i| format!("MemoryMapReserved-{i}")).collect();
    let mut mm: Vec<(&str, Vec<u8>)> = vec![("name", n("memory-map"))];
    for k in &names {
        mm.push((k, z(8)));
    }
    let mut cpu0: Vec<(&str, Vec<u8>)> = vec![("name", n("cpu0"))];
    for k in ["clock-frequency", "memory-frequency", "bus-frequency", "peripheral-frequency", "fixed-frequency", "timebase-frequency"] {
        cpu0.push((k, z(4)));
    }
    let mut io = vec![node(&[("name", n("usb-complex"))], &[node(&[("name", n("usb-ehci"))], &[])])];
    io.extend_from_slice(arm_io);
    let mut children = vec![
        node(&chosen, &[node(&mm, &[])]),
        node(&[("name", n("cpus"))], &[node(&cpu0, &[])]),
        node(&[("name", n("arm-io")), ("clock-frequencies", z(256)), ("usbphy-frequency", z(4))], &io),
        node(&[("name", n("pram")), ("reg", z(8))], &[]),
        node(&[("name", n("vram")), ("reg", z(8))], &[]),
    ];
    children.extend_from_slice(top);
    node(&root, &children)
}

fn placeholder() -> UnitIdentity {
    let s = |k: &str, v: &str| (k.to_string(), IdValue::Str(v.to_string()));
    UnitIdentity::new(vec![
        s("serial-number", "EMU000000000"),
        s("mlb-serial-number", "EMU0000000000"),
        s("unique-chip-id", "0x0000000001"),
        ("die-id".into(), IdValue::List(vec!["0x0".into(), "0x0".into()])),
        s("wifi-mac", "02:00:00:00:00:01"),
        s("bt-mac", "02:00:00:00:00:02"),
    ])
}

/// A two-segment Mach-O: __TEXT at base + 0x1000, __DATA at base + 0x3000 (16 bytes of 0x44), entry base + 0x1040.
pub(crate) fn kernel(base: u32) -> Vec<u8> {
    let le = devicetree::le;
    let seg = |name: &str, vm: u32, vs: u32, fo: u32, fs: u32| {
        let mut v = le(&[1, 56]);
        v.extend_from_slice(&devicetree::name32(name)[..16]);
        v.extend(le(&[vm, vs, fo, fs, 7, 7, 0, 0]));
        v
    };
    let mut thread = vec![5, 16 + 64, 1, 16];
    thread.extend([0u32; 15]);
    thread.push(base + 0x1040);
    let mut cmds = seg("__TEXT", base + 0x1000, 0x2000, 0, 0x2000);
    cmds.extend(seg("__DATA", base + 0x3000, 0x1800, 0x2000, 0x10));
    cmds.extend(le(&thread));
    let mut k = le(&[0xFEED_FACE, 12, 9, 2, 3, cmds.len() as u32, 0]);
    k.extend(cmds);
    k.resize(0x2000, 0);
    k.extend([0x44; 16]);
    k
}

fn add(blob: &[u8], path: &str, prop: &str, value: &[u8]) -> Vec<u8> {
    let mut dt = DeviceTree::new(blob).unwrap();
    dt.add(path, prop, value).unwrap();
    dt.into_data()
}

/// The DT inside a built image.
fn built_dt(img: &Image) -> DeviceTree {
    let r0 = (img.boot_args_pa - img.load_pa) as usize;
    let vbase = u32le(&img.image, r0 + 4);
    let dtp = (u32le(&img.image, r0 + 0x30) - vbase) as usize;
    let dtlen = u32le(&img.image, r0 + 0x34) as usize;
    DeviceTree::new(&img.image[dtp..dtp + dtlen]).unwrap()
}

fn word(dt: &DeviceTree, p: &str, k: &str) -> Option<u32> {
    dt.value(p, k).map(|v| u32le(&v, 0))
}

fn words(dt: &DeviceTree, p: &str, k: &str) -> Vec<u32> {
    dt.value(p, k).map(|d| d.chunks(4).map(|c| u32le(c, 0)).collect()).unwrap_or_default()
}

fn video(img: &Image) -> Vec<u32> {
    let r0 = (img.boot_args_pa - img.load_pa) as usize;
    [0x1C, 0x20, 0x24, 0x28].iter().map(|o| u32le(&img.image, r0 + o)).collect()
}

fn build_default(k: &[u8], dt: &[u8], rd: Option<&[u8]>) -> Image {
    build(k, dt, DEFAULT_BOOT_ARGS, &placeholder(), DEFAULT_IBOOT_VERSION, rd).unwrap()
}

fn flash_disk() -> Vec<u8> {
    node(&[("name", n("flash-controller0"))], &[node(&[("name", n("disk")), ("boot-from-nand", devicetree::le(&[1]))], &[])])
}

/// N81: the board comes from the DT's compatible; its NOR is grafted in and boot-from-nand renamed away.
#[test]
fn n81_board() {
    let arm_io = [
        node(&[("name", n("spi0"))], &[]),
        flash_disk(),
        node(&[("name", n("uart1"))], &[node(&[("name", n("bluetooth")), ("local-mac-address", z(6))], &[])]),
    ];
    let tree = add(&device_tree(&arm_io, &[]), "", "compatible", b"N81AP\0iPod4,1\0AppleARM\0");
    assert_eq!(Board::of(&DeviceTree::new(&tree).unwrap()), Board::n81());
    assert_eq!(Board::of(&DeviceTree::new(&device_tree(&[], &[])).unwrap()), Board::k48());
    let img = build_default(&kernel(0x8000_0000), &tree, None);
    assert_eq!(video(&img), [640 * 4, 640, 960, 32 | 1 << 16]); // rowbytes, w, h, depth | scale-1
    let dt = built_dt(&img);
    assert_eq!(word(&dt, "chosen", "board-id"), Some(8));
    assert_eq!(word(&dt, "chosen", "display-rotation"), Some(0));
    assert_eq!(word(&dt, "chosen", "display-scale"), Some(2));
    for p in ["nor-flash", "nor-flash/nvram", "nor-flash/effaceable", "nor-flash/raw-device", "nor-flash/diagnostic-data"] {
        assert!(dt.contains(&format!("arm-io/spi0/{p}")), "{p}");
    }
    assert_eq!(dt.value("arm-io/spi0/nor-flash/effaceable", "compatible").unwrap(), b"effaceable,nor\0");
    assert!(!dt.has("arm-io/flash-controller0/disk", "boot-from-nand"));
    assert_eq!(word(&dt, "arm-io/flash-controller0/disk", "boot-from-nor"), Some(1));
    assert_eq!(dt.value("arm-io/uart1/bluetooth", "local-mac-address").unwrap(), [2, 0, 0, 0, 0, 2]);
    assert_eq!(dt.value("", "model-number").unwrap()[..5], *b"MC540");
}

/// The IPSW's device-clocks entries for BASE1, SPI2-CLK, DP and ETHERNET (candidate parent lists, flags).
fn device_clocks() -> Vec<u8> {
    let entry = |head: &str, name: &str| {
        let mut v = hex::decode(head).unwrap();
        v.extend([0; 8]);
        v.extend_from_slice(name.as_bytes());
        v.resize(v.len() + 16 - name.len(), 0);
        v
    };
    let mut v = entry("000080090000000001020304", "BASE1");
    v.extend(entry("00008020000000000c0d0000", "SPI2-CLK"));
    v.extend(entry("00010176000000001a190000", "DP"));
    v.extend(entry("000000590000000016000000", "ETHERNET"));
    v
}

fn pmgr() -> Vec<u8> {
    node(&[("name", n("pmgr")), ("device-clocks", device_clocks())], &[])
}

fn panel() -> Vec<u8> {
    node(&[("name", n("mipi-dsim"))], &[node(&[("name", n("lcd")), ("lcd-panel-id", z(4)), ("raw-panel-id", z(16))], &[])])
}

fn backlight() -> Vec<u8> {
    node(&[("name", n("backlight")), ("backlight-id", vec![0xFF; 4])], &[])
}

/// N90: 512 MiB (memSize, vram, pram at its top) and the baseband node kept matchable for the machine.
#[test]
fn n90_board() {
    let bb = node(&[("name", n("baseband")), ("compatible", n("baseband,n90")), ("device_type", n("baseband"))], &[]);
    let tree = device_tree(&[node(&[("name", n("spi0"))], &[]), pmgr(), panel()], &[bb, backlight()]);
    let tree = add(&tree, "", "compatible", b"N90AP\0iPhone3,1\0AppleARM\0");
    let tree = add(&tree, "arm-io", "chip-revision", &z(4));
    assert_eq!(Board::of(&DeviceTree::new(&tree).unwrap()), Board::n90());
    let img = build_default(&kernel(0x8000_0000), &tree, None);
    let r0 = (img.boot_args_pa - img.load_pa) as usize;
    assert_eq!([u32le(&img.image, r0 + 0x0C), u32le(&img.image, r0 + 0x14)], [0x1F70_0000, 0x5F70_0000]);
    let out = built_dt(&img);
    assert_eq!(out.value("baseband", "compatible").unwrap(), n("baseband,n90"));
    assert_eq!(words(&out, "vram", "reg"), [0x5F70_0000, 0x8F_C000]);
    assert_eq!(words(&out, "chosen", "board-id"), [0]);
    // The real iPhone 4's clock table (4.3): APLL, EPLL, FMI, the pixel clock, SPI and I2C, not 100 MHz everywhere.
    let clocks = words(&out, "arm-io", "clock-frequencies");
    let pick: Vec<u32> = [1, 2, 3, 13, 14, 27, 33, 39, 44, 49, 52, 55].iter().map(|&i| clocks[i]).collect();
    assert_eq!(
        pick,
        [800_000_000, 500_000_000, 1_026_000_000, 160_000_000, 51_300_000, 100_000_000, 102_600_000, 68_400_000, 0, 48_857_142, 1_000_000, 0]
    );
    // The IPSW's candidate parents, as iBoot hands them over: the kernel settles them.
    assert_eq!(out.value("arm-io/pmgr", "device-clocks").unwrap(), device_clocks());
    assert_eq!(words(&out, "arm-io", "chip-revision"), [0x20]);
    assert_eq!(words(&out, "arm-io/mipi-dsim/lcd", "lcd-panel-id"), [0xA18A_05C8]);
    let mut raw = vec![0xA1, 0x8A, 0x58, 0x09, 7];
    raw.extend(z(11));
    assert_eq!(out.value("arm-io/mipi-dsim/lcd", "raw-panel-id").unwrap(), raw);
    assert_eq!(words(&out, "backlight", "backlight-id"), [5]);

    let k48 = add(&device_tree(&[pmgr(), panel()], &[backlight()]), "arm-io", "chip-revision", &z(4));
    let kdt = built_dt(&build_default(&kernel(0x8000_0000), &k48, None));
    // The real iPad's (3.2.2): a 1 GHz APLL, PLL1 off, 166.67 and 68.4 MHz where the iPhone 4 has 160 and 51.3.
    let c = words(&kdt, "arm-io", "clock-frequencies");
    let pick: Vec<u32> = [1, 2, 13, 14, 33, 44, 49].iter().map(|&i| c[i]).collect();
    assert_eq!(pick, [1_000_000_000, 0, 166_666_666, 68_400_000, 102_600_000, 0, 24_000_000]);
    assert_eq!(words(&kdt, "arm-io", "chip-revision"), [0x11]);
    assert_eq!(words(&kdt, "arm-io/mipi-dsim/lcd", "lcd-panel-id"), [0xA1E5_06C9]);
    assert_eq!(words(&kdt, "backlight", "backlight-id"), [6]);
    assert_eq!(kdt.value("arm-io/pmgr", "device-clocks").unwrap(), device_clocks());
}

/// N88 (S5L8920): its own platform-name/chip-id, a 320x480 panel, its own NOR kept (nothing grafted) with
/// boot-from-nand renamed away, the baseband node unmatched (renamed nobb) with test identity in it.
#[test]
fn n88_board() {
    let nor = node(&[("name", n("nor-flash")), ("compatible", n("nor-flash,spi"))], &[]);
    let arm_io = [
        node(&[("name", n("spi0"))], &[nor]),
        flash_disk(),
        node(&[("name", n("audio-complex")), ("ncoref-frequency", z(4))], &[]),
    ];
    let bb = node(
        &[
            ("name", n("baseband")),
            ("compatible", n("baseband,n88")),
            ("device_type", n("baseband")),
            ("device-imei", z(16)),
            ("snum", z(12)),
        ],
        &[],
    );
    let tree = add(&device_tree(&arm_io, &[bb]), "", "compatible", b"N88AP\0iPhone2,1\0AppleARM\0");
    assert_eq!(Board::of(&DeviceTree::new(&tree).unwrap()), Board::n88());
    let img = build_default(&kernel(0x8000_0000), &tree, None);
    assert_eq!(video(&img), [320 * 4, 320, 480, 32]);
    let dt = built_dt(&img);
    assert_eq!(word(&dt, "chosen", "chip-id"), Some(0x8920));
    assert_eq!(word(&dt, "chosen", "board-id"), Some(0));
    assert_eq!(word(&dt, "chosen", "display-rotation"), Some(0));
    assert_eq!(dt.value("", "platform-name").unwrap()[..9], *b"s5l8920x\0");
    assert!(!dt.contains("arm-io/spi0/nor-flash/effaceable")); // its own NOR: no graft
    assert!(!dt.has("arm-io/flash-controller0/disk", "boot-from-nand"));
    assert_eq!(dt.value("nobb", "compatible").unwrap()[..5], *b"none\0");
    assert_eq!(dt.value("nobb", "device-imei").unwrap()[..16], *b"004999010640000\0");
    assert_eq!(dt.value("nobb", "snum").unwrap(), b"TESTSNUM0000");
    assert_eq!(dt.value("", "model-number").unwrap()[..5], *b"MB715");
    assert_eq!(word(&dt, "arm-io/audio-complex", "ncoref-frequency"), Some(162_000_000)); // PLL1, as iBoot fills it
}

/// N18 (S5L8922): its platform-name and chip-id, the 320x480 portrait panel at scale 1, and the NOR graft as on N81.
#[test]
fn n18_board() {
    let arm_io = [node(&[("name", n("spi0"))], &[]), flash_disk()];
    let tree = add(&device_tree(&arm_io, &[]), "", "compatible", b"N18AP\0iPod3,1\0AppleARM\0");
    assert_eq!(Board::of(&DeviceTree::new(&tree).unwrap()), Board::n18());
    let img = build_default(&kernel(0x8000_0000), &tree, None);
    assert_eq!(video(&img), [320 * 4, 320, 480, 32]);
    let dt = built_dt(&img);
    assert_eq!(dt.value("", "platform-name").unwrap()[..9], n("s5l8922x")[..]);
    assert_eq!(word(&dt, "chosen", "chip-id"), Some(0x8922));
    assert_eq!(word(&dt, "chosen", "board-id"), Some(2));
    assert_eq!(word(&dt, "chosen", "display-scale"), Some(1));
    assert!(dt.contains("arm-io/spi0/nor-flash/effaceable"));
    assert_eq!(word(&dt, "arm-io/flash-controller0/disk", "boot-from-nor"), Some(1));
    assert_eq!(dt.value("", "model-number").unwrap()[..5], *b"MC008");
}

#[test]
fn selfcheck() {
    let blob = device_tree(&[], &[]);
    let img = build_default(&kernel(0xC000_0000), &blob, None);
    assert_eq!((img.load_pa, img.entry_pa, img.boot_args_pa), (0x4000_0000, 0x4000_1040, 0x4000_6000));
    let image = &img.image;
    let r0 = (img.boot_args_pa - img.load_pa) as usize;
    assert_eq!(image[0x1000..0x1004], [0xCE, 0xFA, 0xED, 0xFE]);
    assert_eq!(image[0x3000..0x3010], [0x44; 16]);
    assert!(image[0x3010..0x4800].iter().all(|&b| b == 0)); // __DATA zero fill past filesize
    assert_eq!(image[r0..r0 + 4], [1, 0, 2, 0]);
    let w: Vec<u32> = [4, 8, 12, 16].iter().map(|o| u32le(image, r0 + o)).collect();
    assert_eq!(w, [0xC000_0000, 0x4000_0000, 0x0F70_0000, 0x4000_8000]);
    let dtp = u32le(image, r0 + 0x30) as usize;
    let dtlen = u32le(image, r0 + 0x34) as usize;
    assert_eq!((dtp, dtlen), (0xC000_5000, blob.len() + 36)); // + hsic-enabled
    let line: Vec<u8> = image[r0 + 0x38..].iter().copied().take_while(|&b| b != 0).collect();
    assert_eq!(line, DEFAULT_BOOT_ARGS.as_bytes());
    assert_eq!(u32le(image, r0 + 0x18), 1); // no -v: graphics (the logo) stays up
    let dt = built_dt(&img);
    assert_eq!(words(&dt, "chosen/memory-map", "Kernel-__TEXT"), [0x4000_1000, 0x2000]);
    assert_eq!(words(&dt, "chosen/memory-map", "DeviceTree"), [0x4000_5000, dtlen as u32]);
    assert_eq!(words(&dt, "chosen/memory-map", "BootArgs"), [0x4000_6000, 0x1000]);
    assert!(dt.has("chosen/memory-map", "MemoryMapReserved-4"));
    assert_eq!(dt.slot("arm-io/usb-complex", "hsic-enabled").unwrap().length, 0);
    assert!(dt.contains("arm-io/usb-complex/usb-ehci"));
    assert_eq!(words(&dt, "chosen", "unique-chip-id"), [1, 0]);
    assert_eq!(words(&dt, "chosen", "die-id"), [0, 0]);
    assert_eq!(words(&dt, "chosen", "chip-id"), [0x8930]);
    assert_eq!(words(&dt, "cpus/cpu0", "timebase-frequency"), [24_000_000]);
    assert_eq!(words(&dt, "vram", "reg"), [0x4F70_0000, 0x8F_C000]);
    assert_eq!(words(&dt, "pram", "reg"), [0x5FFF_C000, 0x4000]);
    assert_eq!(words(&dt, "chosen", "display-rotation"), [270]);

    // 4.x link base; RAM-disk mode puts the disk after the kernel (0x80004800 -> page 0x80005000), DT after it.
    let img4 = build_default(&kernel(0x8000_0000), &blob, None);
    assert_eq!((img4.entry_pa, img4.boot_args_pa), (0x4000_1040, 0x4000_6000));
    assert_eq!(u32le(&img4.image, 0x6000 + 4), 0x8000_0000);
    let rd: Vec<u8> = b"H+".repeat(0x900);
    let imgr = build_default(&kernel(0x8000_0000), &blob, Some(&rd));
    assert_eq!(imgr.image[0x5000..0x5000 + rd.len()], rd[..]);
    assert_eq!(imgr.boot_args_pa, 0x4000_8000);
    let line: Vec<u8> = imgr.image[0x8000 + 0x38..].iter().copied().take_while(|&b| b != 0).collect();
    assert_eq!(line, format!("{DEFAULT_BOOT_ARGS} rd=md0").as_bytes());
    let dtr = DeviceTree::new(&imgr.image[0x7000..0x7000 + blob.len() + 36]).unwrap();
    assert_eq!(words(&dtr, "chosen/memory-map", "RAMDisk"), [0x4000_5000, rd.len() as u32]);
    assert_eq!(dtr.value("chosen", "root-matching").unwrap()[..4], [0; 4]);
}

/// ipad1_kboot.fill_dt's NAND geometry: every key a node has, on flash-controller0 (iBoot-1219, 5.x: with ce-bitmap,
/// which AppleIOPFMI-49 spins on when empty) and on its disk child (4.x).
#[test]
fn nand_geometry_on_5x_nodes() {
    let blob = device_tree(
        &[node(
            &[("name", n("flash-controller0")), ("#ce", z(4)), ("ce-bitmap", z(4))],
            &[node(&[("name", n("disk")), ("#ce", z(4)), ("#databus", z(4))], &[])],
        )],
        &[],
    );
    let built = built_dt(&build_default(&kernel(0x8000_0000), &blob, None));
    assert_eq!(word(&built, "arm-io/flash-controller0", "ce-bitmap"), Some(0x0F0F));
    assert_eq!(word(&built, "arm-io/flash-controller0", "#ce"), Some(8));
    assert_eq!(word(&built, "arm-io/flash-controller0/disk", "#ce"), Some(8));
    assert_eq!(word(&built, "arm-io/flash-controller0/disk", "#databus"), Some(2));
    assert!(built.value("arm-io/flash-controller0/disk", "ce-bitmap").is_none());
}

/// 3.x's single landing-map word becomes one CE mask per populated bus.
#[test]
fn landing_map_splits_buses() {
    let blob = device_tree(
        &[node(
            &[("name", n("flash-controller0"))],
            &[node(&[("name", n("disk")), ("reg", devicetree::le(&[0x0F0F])), ("landing-map", devicetree::le(&[0x0F0F]))], &[])],
        )],
        &[],
    );
    let mut dt = DeviceTree::new(&blob).unwrap();
    landing_map(&mut dt).unwrap();
    assert_eq!(words(&dt, "arm-io/flash-controller0/disk", "landing-map"), [0x0F, 0x0F00]);
    assert_eq!(words(&dt, "arm-io/flash-controller0/disk", "landing-map-dt"), [0x0F0F]);
}

/// ipad1_kboot.boot_args_version: the kernel's own `ldrh r3, [r0, #2]; cmp r3, #3` before the Epoch Mismatch literal.
#[test]
fn boot_args_version_literal() {
    let mut k = kernel(0x8000_0000);
    assert_eq!(MachO::new(&k).unwrap().boot_args_version(), 2); // no check: the 3.2.x / 4.2.1 / 4.3.0 shape
    let s = b"pe_identify_machine: Epoch Mismatch\0";
    k[0x100..0x100 + s.len()].copy_from_slice(s);
    k[0x200..0x204].copy_from_slice(&[0x43, 0x88, 0x03, 0x2B]); // ldrh r3, [r0, #2]; cmp r3, #3
    k[0x210..0x214].copy_from_slice(&0x8000_1100u32.to_le_bytes()); // the string's VA in the literal pool
    assert_eq!(MachO::new(&k).unwrap().boot_args_version(), 3);
    let img = build(&k, &device_tree(&[], &[]), DEFAULT_BOOT_ARGS, &placeholder(), DEFAULT_IBOOT_VERSION, None).unwrap();
    let r0 = (img.boot_args_pa - img.load_pa) as usize;
    assert_eq!(img.image[r0..r0 + 4], [1, 0, 3, 0]);
}

/// iOS 6 (xnu-2107): the string named by movw/movt + add pc instead of a literal.
#[test]
fn boot_args_version_pc_relative() {
    let mut k = kernel(0x8000_0000);
    let s = b"pe_identify_machine: Epoch Mismatch\0";
    k[0x100..0x100 + s.len()].copy_from_slice(s);
    // ldrh r3, [r0, #2]; cmp r3, #3; beq; movw r1, #0xfeee; movt r1, #0xffff; add r1, pc
    k[0x200..0x210].copy_from_slice(&[0x43, 0x88, 0x03, 0x2B, 0x00, 0xD0, 0x4F, 0xF6, 0xEE, 0x61, 0xCF, 0xF6, 0xFF, 0x71, 0x79, 0x44]);
    assert_eq!(MachO::new(&k).unwrap().boot_args_version(), 3);
    k[0x20E] = 0x7A; // add r2, pc: lands nowhere
    assert_eq!(MachO::new(&k).unwrap().boot_args_version(), 2);
}

#[test]
fn boot_args_version_movw_movt() {
    let mut k = kernel(0x8000_0000);
    let s = b"pe_identify_machine: Epoch Mismatch\0";
    k[0x100..0x100 + s.len()].copy_from_slice(s); // VA 0x8000_1100
    let movw = |top: bool, rd: u16, imm: u16| {
        let hw1 = (if top { 0xF2C0 } else { 0xF240 }) | ((imm >> 11) & 1) << 10 | (imm >> 12);
        let hw2 = ((imm >> 8) & 7) << 12 | rd << 8 | (imm & 0xFF);
        [hw1 as u8, (hw1 >> 8) as u8, hw2 as u8, (hw2 >> 8) as u8]
    };
    let add_at = 0x20E;
    let pc = 0x8000_1000u32 + add_at + 4;
    let delta = 0x8000_1100u32.wrapping_sub(pc);
    let mut code = vec![0x43, 0x88, 0x03, 0x2B, 0x00, 0xBF]; // ldrh r3, [r0, #2]; cmp r3, #3; nop
    code.extend(movw(false, 0, delta as u16));
    code.extend(movw(true, 0, (delta >> 16) as u16));
    code.extend([0x78, 0x44]); // add r0, pc
    k[0x200..0x200 + code.len()].copy_from_slice(&code);
    assert_eq!(MachO::new(&k).unwrap().boot_args_version(), 3);
    k[0x203] = 0x2C; // cmp r4: not the loaded register
    assert_eq!(MachO::new(&k).unwrap().boot_args_version(), 2);
}

/// chosen/nvram-proxy-data: the empty image's headers match the K48 NOR's common/free banks.
#[test]
fn nvram_image_headers() {
    let img = nvram_image(8192);
    assert_eq!(img.len(), 8192);
    let mut head = vec![0x70, 0x7C, 0x80, 0x00];
    head.extend_from_slice(b"common");
    head.extend([0; 6]);
    assert_eq!(img[..16], head[..]);
    assert_eq!(img[0x800..0x804], [0x7F, 0xA4, 0x80, 0x01]);
}

#[test]
fn iboot_versions() {
    assert_eq!(iboot_version(Some(b"xxiBoot-xiBoot-817.29.\0")), "iBoot-817.29");
    assert_eq!(iboot_version(Some(b"iBoot-931.71.16 ")), "iBoot-931.71.16");
    assert_eq!(iboot_version(None), "iBoot-817.29");
    assert_eq!(iboot_version(Some(b"none")), "iBoot-817.29");
}

/// ipad1_kboot.selfcheck's logo: a 2x1 iBootIm, left pixel opaque white, right transparent.
#[test]
fn logo_is_centered_and_turned() {
    let mut blob = b"iBootIm\0".to_vec();
    blob.extend([0; 4]);
    blob.extend_from_slice(b"sszlyerg");
    blob.extend([2, 0, 1, 0]);
    blob.resize(0x40, 0);
    blob.extend([0xFF, 255, 0, 255, 255]);
    let segs = logo_segments(&blob, 0x4F70_0000, FB_WIDTH, FB_HEIGHT, true).unwrap();
    assert_eq!(segs.len(), 2);
    assert_eq!(segs[0], Segment { pa: 0x4F70_0000, length: 1024 * 768 * 4, data: None });
    let x0 = (1024 - 1) / 2;
    let y0 = (768 - 2) / 2;
    assert_eq!((segs[1].pa, segs[1].length), (0x4F70_0000 + (y0 * 4096) as u32, 1024 * 4 * 2));
    let rows = segs[1].data.as_ref().unwrap();
    // turned a quarter counter-clockwise: the logo's top row becomes its left column, its left end the bottom
    assert_eq!(rows[x0 * 4..x0 * 4 + 4], [0x00, 0x00, 0x00, 0xFF]); // row 0 <- logo x 1 (clear)
    assert_eq!(rows[4096 + x0 * 4..4096 + x0 * 4 + 4], [0xFF; 4]); // row 1 <- logo x 0 (white)
}

#[test]
fn bundle_layout() {
    let img = Image { image: vec![7; 16], load_pa: 1, entry_pa: 2, boot_args_pa: 3 };
    let b = bundle(&img, &[Segment { pa: 9, length: 4, data: None }, Segment { pa: 10, length: 2, data: Some(vec![5, 6]) }]);
    assert_eq!(b.len(), 16 + 20 + 22 + 24);
    assert_eq!(&b[16..24], b"K48SEG\0\0");
    assert_eq!(u32le(&b, 32), 1);
    assert_eq!(&b[b.len() - 24..b.len() - 16], b"K48KBOOT");
    assert_eq!(u32le(&b, b.len() - 4), 16);
}

/// The Swift create of n81ap-10B500 (fwkit-oracle n81-a): kboot.bin from its decrypt cache and identity.json.
#[test]
#[ignore = "needs ~/Documents/LightTouch-Development/desktop/fwkit-oracle/n81-a and n81-a.cache"]
fn n81_oracle() {
    use sha2::Digest;
    let d = Path::new(env!("HOME")).join("Documents/LightTouch-Development/desktop/fwkit-oracle");
    let dec = d.join(
        "n81-a.cache/decrypted-v2/693bb78c8f6baf4186f7797bf3a0ef7d3b18255e/a86e44e5db00d130da9a4f97c04e27a5a084f7d1f322cb2f706d2fc67283e152",
    );
    let id = UnitIdentity::load(&d.join("n81-a/identity.json")).unwrap();
    let bin = make(&dec, &id, DEFAULT_BOOT_ARGS, None, None).unwrap();
    assert_eq!(hex::encode(sha2::Sha256::digest(&bin)), "48ccaea2883a2bee1634fca476a8b5306869229d59809912f19e52f8fe6a5b9e");
    // iOS 6's xnu-2107: the movw/movt shape, version 3
    let r0 = (u32le(&bin, bin.len() - 8) - u32le(&bin, bin.len() - 16)) as usize; // boot_args PA - load PA
    assert_eq!(bin[r0 + 2], 3);
}
