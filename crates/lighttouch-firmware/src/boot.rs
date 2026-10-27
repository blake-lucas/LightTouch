//! What a preparation boot needs to know about a board and its machine, and the boot's argv (Swift's HostRuntime
//! Board, DeviceInfo and BootRecipe, the parts preparation uses). What the hardware is (the -M machine, its modem and
//! USB host) is the emulator's: DeviceInfo, listed by the oneshot helper.
use serde::Deserialize;

/// The SoC family, which decides the prepared base's boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Soc {
    S5l8900,
    S5l8720,
    S5l8920,
    S5l8930,
}

/// The boards Light Touch runs and the facts preparation reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Board {
    N45,
    M68,
    N72,
    N18,
    N88,
    K48,
    N81,
    N90,
}

impl Board {
    pub fn parse(board: &str) -> Option<Board> {
        Some(match board {
            "n45ap" => Board::N45,
            "m68ap" => Board::M68,
            "n72ap" => Board::N72,
            "n18ap" => Board::N18,
            "n88ap" => Board::N88,
            "k48ap" => Board::K48,
            "n81ap" => Board::N81,
            "n90ap" => Board::N90,
            _ => return None,
        })
    }
    pub fn name(self) -> &'static str {
        match self {
            Board::N45 => "n45ap",
            Board::M68 => "m68ap",
            Board::N72 => "n72ap",
            Board::N18 => "n18ap",
            Board::N88 => "n88ap",
            Board::K48 => "k48ap",
            Board::N81 => "n81ap",
            Board::N90 => "n90ap",
        }
    }
    pub fn soc(self) -> Soc {
        match self {
            Board::N45 | Board::M68 => Soc::S5l8900,
            Board::N72 => Soc::S5l8720,
            Board::N18 | Board::N88 => Soc::S5l8920,
            Board::K48 | Board::N81 | Board::N90 => Soc::S5l8930,
        }
    }
    pub fn product_type(self) -> &'static str {
        match self {
            Board::N45 => "iPod1,1",
            Board::M68 => "iPhone1,1",
            Board::N72 => "iPod2,1",
            Board::N18 => "iPod3,1",
            Board::N88 => "iPhone2,1",
            Board::K48 => "iPad1,1",
            Board::N81 => "iPod4,1",
            Board::N90 => "iPhone3,1",
        }
    }
    pub fn marketing_name(self) -> &'static str {
        match self {
            Board::N45 => "iPod touch",
            Board::M68 => "iPhone",
            Board::N72 => "iPod touch (2nd generation)",
            Board::N18 => "iPod touch (3rd generation)",
            Board::N88 => "iPhone 3GS",
            Board::K48 => "iPad",
            Board::N81 => "iPod touch (4th generation)",
            Board::N90 => "iPhone 4",
        }
    }
    pub fn is_phone(self) -> bool {
        matches!(self, Board::M68 | Board::N88 | Board::N90)
    }
    /// The S5L8920/S5L8930 boards: prepared alike, booted by kboot (iBoot too on K48).
    pub fn is_kboot(self) -> bool {
        matches!(self.soc(), Soc::S5l8920 | Soc::S5l8930)
    }
    /// The guest packages' architecture.
    pub fn arch(self) -> &'static str {
        if matches!(self.soc(), Soc::S5l8900 | Soc::S5l8720) { "armv6" } else { "armv7" }
    }
    /// iPhones prepared through the kboot pipeline (iPhone 3GS, iPhone 4): their identity carries the modem's IMEI.
    pub fn kboot_phone(self) -> bool {
        self.is_kboot() && self.is_phone()
    }
}

/// A machine as the emulator describes it (qemu_ios_device_info).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    /// The -M name.
    pub machine: String,
    pub board: String,
    #[serde(default)]
    pub screen_width: i64,
    #[serde(default)]
    pub screen_height: i64,
    #[serde(default)]
    pub has_cellular: bool,
    #[serde(rename = "hasUSBHost", default)]
    pub has_usb_host: bool,
    #[serde(default)]
    pub has_compass: bool,
}

pub fn escape(value: &str) -> String {
    value.replace(',', ",,")
}

/// The -machine options the device was made for (device.lock.json "machine"), sorted by key.
pub fn options(machine: &std::collections::BTreeMap<String, String>) -> String {
    machine.iter().map(|(k, v)| format!(",{k}={}", escape(v))).collect()
}

/// argv for one boot, and the -M machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootConfig {
    pub argv: Vec<String>,
    pub machine: String,
}

/// How an S5L8920/S5L8930 board enters its kernel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IPadBoot {
    Kernel { image: String, writable_nor: Option<String> },
    IBoot { image: String, writable_nor: String, gid_blobs: String },
}

/// One preparation boot of an S5L8920/S5L8930 board (BootRecipe.iPad with oneShot: no keyboard, no monitor, and QEMU
/// exits when the guest halts or restarts instead of stopping). `overlay` None writes the store itself.
pub struct IPad {
    pub boot: IPadBoot,
    pub nand: String,
    pub overlay: Option<String>,
    pub die_id: Option<String>,
    pub wifi: bool,
    pub machine_options: std::collections::BTreeMap<String, String>,
}

pub fn ipad(d: &IPad, hardware: &DeviceInfo, serial: &str) -> BootConfig {
    let board = &hardware.machine;
    let mut machine = match &d.boot {
        IPadBoot::Kernel { image, writable_nor } => {
            let mut m = format!("{board},kboot={}", escape(image));
            if let Some(nor) = writable_nor {
                m += &format!(",nor-rw={}", escape(nor));
            }
            m
        }
        IPadBoot::IBoot { image, writable_nor, gid_blobs } => {
            format!("{board},iboot={},nor-rw={},gid-blobs={}", escape(image), escape(writable_nor), escape(gid_blobs))
        }
    };
    machine += &format!(",nand={}", escape(&d.nand));
    if let Some(o) = &d.overlay {
        machine += &format!(",nand-overlay={}", escape(o));
    }
    if let Some(die) = &d.die_id {
        machine += &format!(",die-id={}", escape(die));
    }
    if !d.wifi {
        machine += ",wifi=off";
    }
    if hardware.has_cellular {
        machine += ",baseband=on";
    }
    machine += &options(&d.machine_options);
    let mut argv: Vec<String> = ["LightTouchMac", "-M", &machine, "-display", "none", "-monitor", "none", "-no-reboot"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    argv.extend(["-audio", "driver=none", "-serial", serial].iter().map(|s| s.to_string()));
    BootConfig { argv, machine: board.clone() }
}

/// One preparation boot of the iPod touch 2G (BootRecipe.iPod).
pub struct IPod {
    pub boot_args: String,
    /// The machine's direct-iboot; "" boots the SecureROM -> NOR LLB -> iBoot chain (2.x).
    pub iboot: String,
    pub bootrom: String,
    pub nand: String,
    pub nor: String,
    pub writable_nor: String,
    /// The NAND page overlay; empty for an image store, which the emulator writes in place.
    pub overlay: String,
    pub wifi: bool,
    pub memory: String,
    pub gid_blobs: Option<String>,
    pub machine_options: std::collections::BTreeMap<String, String>,
}

pub fn ipod(d: &IPod, hardware: &DeviceInfo, serial: &str, extra: &[String]) -> BootConfig {
    let mut machine = format!(
        "{},boot-args={},direct-iboot={},direct-llb=,bootrom={},nand={},nor={},nor-rw={}",
        hardware.machine,
        escape(&d.boot_args),
        escape(&d.iboot),
        escape(&d.bootrom),
        escape(&d.nand),
        escape(&d.nor),
        escape(&d.writable_nor)
    );
    if !d.overlay.is_empty() {
        machine += &format!(",nandrw={}", escape(&d.overlay));
    }
    if d.wifi {
        machine += ",wifi=on";
    }
    if let Some(b) = &d.gid_blobs {
        machine += &format!(",gid-blobs={}", escape(b));
    }
    machine += &options(&d.machine_options);
    let mut argv: Vec<String> =
        ["LightTouchMac", "-M", &machine, "-m", &d.memory, "-display", "none", "-no-shutdown"].iter().map(|s| s.to_string()).collect();
    argv.extend(["-audio", "driver=none", "-serial", serial].iter().map(|s| s.to_string()));
    argv.extend(extra.iter().cloned());
    BootConfig { argv, machine: hardware.machine.clone() }
}

/// One preparation boot of the S5L8900 boards (BootRecipe.iPod1G): no -m, the NOR a pflash drive.
pub struct IPod1G {
    pub bootrom: String,
    pub iboot: String,
    pub nand: String,
    pub writable_nor: String,
    pub overlay: String,
    pub wifi: bool,
    pub machine_options: std::collections::BTreeMap<String, String>,
}

pub fn ipod1g(d: &IPod1G, hardware: &DeviceInfo, serial: &str) -> BootConfig {
    let mut machine = format!("{},bootrom={},iboot={},nand={}", hardware.machine, escape(&d.bootrom), escape(&d.iboot), escape(&d.nand));
    if !d.overlay.is_empty() {
        machine += &format!(",nand-overlay={}", escape(&d.overlay));
    }
    if !d.wifi {
        machine += ",wifi=off";
    }
    machine += &options(&d.machine_options);
    let mut argv: Vec<String> = vec![
        "LightTouchMac".into(),
        "-M".into(),
        machine,
        "-drive".into(),
        format!("if=pflash,format=raw,file={}", escape(&d.writable_nor)),
        "-display".into(),
        "none".into(),
        "-no-shutdown".into(),
    ];
    argv.extend(["-audio", "driver=none", "-serial", serial].iter().map(|s| s.to_string()));
    if hardware.has_cellular {
        argv.extend(["-netdev".to_string(), "user,id=cell0,lan=off".to_string()]);
    }
    BootConfig { argv, machine: hardware.machine.clone() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipad_argv_as_swift_builds_it() {
        let hw = DeviceInfo {
            machine: "iPod-Touch-4G".into(),
            board: "n81ap".into(),
            screen_width: 640,
            screen_height: 960,
            has_cellular: false,
            has_usb_host: true,
            has_compass: true,
        };
        let c = ipad(
            &IPad {
                boot: IPadBoot::Kernel { image: "/a/kboot.bin".into(), writable_nor: Some("/a/nor,1.bin".into()) },
                nand: "/a/nand".into(),
                overlay: None,
                die_id: Some("0x1:0x2".into()),
                wifi: true,
                machine_options: Default::default(),
            },
            &hw,
            "file:/w/seal.log",
        );
        assert_eq!(
            c.argv,
            [
                "LightTouchMac",
                "-M",
                "iPod-Touch-4G,kboot=/a/kboot.bin,nor-rw=/a/nor,,1.bin,nand=/a/nand,die-id=0x1:0x2",
                "-display",
                "none",
                "-monitor",
                "none",
                "-no-reboot",
                "-audio",
                "driver=none",
                "-serial",
                "file:/w/seal.log"
            ]
        );
    }
}
