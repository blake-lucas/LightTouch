//! firmwarekit in Rust: prepares a device's storage from an IPSW, as the Swift FirmwareKit does, for every
//! platform the app runs on (Linux, Windows, macOS, Android). One module per FirmwareKit area:
//!
//!   entry      the catalog entry (keys, recipe)
//!   ipsw       IPSW members, Restore.plist, component paths
//!   img3       IMG3 / 8900 / IMG2 containers and AES
//!   lzss       kernelcache decompression
//!   vfdecrypt  the encrypted root filesystem image
//!   decrypt    the decrypt step and its cache
//!   udif       DMG images, the rootfs volume out of its partition map
//!   hfs        HFS+/HFSX volumes: read, edit and commit without a mount; grow; format
//!   plistedit  property lists edited inside a volume, format kept
//!   fitcheck   proof that a guest piece fits the firmware (Mach-O, the shared cache);  shared_cache  dyld's cache
//!   macho      32-bit ARM Mach-O: slices, load commands, symbols;  sandbox_patch  sb_evaluate located (jailbreak)
//!   activation lockdownd activated, ad hoc re-signing, the helpers' signature checks
//!   guest_package  the it_boot loader and the seed package;  guest_archive  the .itpack reader and offer text
//!   keyboards, setup, assets, cydia  the data-volume seeds (keyboards, language, Setup finished, 6.x assets) and
//!              the jailbreak option;  prefs1x, truststore1x  the 1.x preference keys and trust anchors
//!   page_nand  the page NAND image store (n72, s5l8900 boards)
//!   n72nand    the iPod touch 2G NAND writer;  n72nor  its NOR
//!   n45nand    the iPod touch 1G / iPhone NAND writer;  n45ftl  its FTL read back;  n45nor  its NOR
//!   k48nand    the K48-family NAND store (iPad 1, iPod touch 3G/4G, iPhone 3GS/4): writer, reader, check
//!   identity   the synthetic unit identity (identity.json);  devicetree  Apple's flattened device tree
//!   kboot      the direct-kernel boot bundle (kboot.bin);  k48iboot  the iPad's real-iBoot chain (gid blobs, NOR)
//!   sandbox_patch  the jailbroken kernel's sandbox patch and the kernelcache re-wrap
pub mod activation;
pub mod assets;
pub mod boot;
pub mod cache;
pub mod create;
pub mod cydia;
pub mod decrypt;
pub mod devicetree;
pub mod entry;
pub mod error;
pub mod files;
pub mod fitcheck;
pub mod guest_archive;
pub mod guest_package;
pub mod hfs;
pub mod identity;
pub mod img3;
pub mod ipsw;
pub mod k48_recipe;
pub mod k48iboot;
pub mod k48nand;
pub mod k48nand_check;
pub mod k48nand_image;
pub mod kboot;
pub mod keyboards;
pub mod lock;
pub mod lzss;
pub mod macho;
pub mod n45_recipe;
pub mod n45ftl;
pub mod n45nand;
pub mod n45nor;
pub mod n72_recipe;
pub mod n72keybag;
pub mod n72nand;
pub mod n72nor;
pub mod nand_disk;
pub mod oneshot;
pub mod page_nand;
pub mod plistedit;
pub mod posio;
pub mod prefs1x;
pub mod progress;
pub mod sandbox_patch;
pub mod setup;
pub mod shared_cache;
pub mod system_edits;
pub mod truststore1x;
pub mod udif;
pub mod vfdecrypt;

pub use error::{Code, Error, Result};

/// The tool version recorded in locks and the decrypt cache identity.
pub const VERSION: &str = concat!("rust-", env!("CARGO_PKG_VERSION"));
