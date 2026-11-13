//! Embeds the application manifest and the exe icon. No resource compiler
//! and no build dependency: this writes a `.res` file by hand, and link.exe
//! accepts those as ordinary inputs.

use std::path::Path;
use std::{env, fs};

const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const RT_MANIFEST: u16 = 24;
const LANG_EN_US: u16 = 0x0409;
const ICO: &str = "assets/clawd.ico";
const MANIFEST: &str = "assets/clawd.manifest";
const DELAY_LOADED: &[&str] = &["user32.dll", "gdi32.dll", "shell32.dll", "dwmapi.dll", "advapi32.dll", "imm32.dll", "api-ms-win-shcore-scaling-l1-1-1.dll"];

/// One resource: a 32-byte header with numeric type and id, then the data,
/// padded to a 4-byte boundary.
fn resource(res: &mut Vec<u8>, kind: u16, id: u16, flags: u16, lang: u16, data: &[u8]) {
    res.extend((data.len() as u32).to_le_bytes());
    res.extend(32u32.to_le_bytes());
    for ordinal in [0xFFFF, kind, 0xFFFF, id] {
        res.extend(ordinal.to_le_bytes());
    }
    res.extend(0u32.to_le_bytes()); // DataVersion
    res.extend(flags.to_le_bytes());
    res.extend(lang.to_le_bytes());
    res.extend([0u8; 8]); // Version, Characteristics
    res.extend(data);
    res.resize(res.len().next_multiple_of(4), 0);
}

fn u32_at(b: &[u8], at: usize) -> usize {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]) as usize
}

/// An `.ico` becomes one RT_ICON per image plus an RT_GROUP_ICON directory
/// that refers to them by id instead of by file offset.
fn add_icon(res: &mut Vec<u8>, ico: &[u8]) {
    let count = u16::from_le_bytes([ico[4], ico[5]]);
    let mut group = ico[..6].to_vec();
    for i in 0..count {
        let entry = &ico[6 + 16 * i as usize..][..16];
        let (size, offset) = (u32_at(entry, 8), u32_at(entry, 12));
        resource(res, RT_ICON, i + 1, 0x1010, LANG_EN_US, &ico[offset..offset + size]);
        group.extend(&entry[..12]);
        group.extend((i + 1).to_le_bytes());
    }
    resource(res, RT_GROUP_ICON, 1, 0x1030, LANG_EN_US, &group);
}

fn main() {
    for input in ["build.rs", ICO, MANIFEST] {
        println!("cargo:rerun-if-changed={input}");
    }
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows")
        || env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc")
    {
        return;
    }
    let mut res = Vec::new();
    resource(&mut res, 0, 0, 0, 0, &[]); // a .res file starts with an empty entry
    // Missing until the first `--export-ico`; the exe is just iconless then.
    if let Ok(ico) = fs::read(ICO) {
        add_icon(&mut res, &ico);
    }
    let manifest = fs::read(MANIFEST).expect(MANIFEST);
    resource(&mut res, RT_MANIFEST, 1, 0x0030, LANG_EN_US, &manifest);

    let out = Path::new(&env::var("OUT_DIR").unwrap()).join("clawd.res");
    fs::write(&out, res).unwrap();
    println!("cargo:rustc-link-arg-bins={}", out.display());

    // The hook starts once per Claude event and needs none of these, and
    // load-time imports cost every run (user32 alone: 2.8 ms of a 9 ms hook,
    // see ROADMAP Phase 6). The pet still loads them, on first use.
    for dll in DELAY_LOADED {
        println!("cargo:rustc-link-arg-bins=/DELAYLOAD:{dll}");
    }
    println!("cargo:rustc-link-arg-bins=delayimp.lib");
}
