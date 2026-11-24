//! Embeds assets/shotclip.ico as the exe icon (group icon ID 1).
//!
//! The GNU toolchain here has no windres, so instead of a .rc file this writes a minimal
//! COFF object with a `.rsrc` section (what windres/cvtres would produce) and links it.
//! Works with both GNU ld and MSVC link.exe.

use std::{env, fs, path::PathBuf};

const ICO: &str = "assets/shotclip.ico";
const RT_ICON: u32 = 3;
const RT_GROUP_ICON: u32 = 14;
const LANG: u32 = 0x0409;

fn u16le(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_le_bytes());
}
fn u32le(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}

struct Res {
    ty: u32,
    id: u32,
    data: Vec<u8>,
}

/// Splits an .ico into RT_ICON images + one RT_GROUP_ICON directory.
fn icon_resources(ico: &[u8]) -> Vec<Res> {
    let rd16 = |o: usize| u16::from_le_bytes([ico[o], ico[o + 1]]);
    let rd32 = |o: usize| u32::from_le_bytes(ico[o..o + 4].try_into().unwrap());
    assert_eq!((rd16(0), rd16(2)), (0, 1), "not an .ico file");
    let n = rd16(4) as usize;
    let mut out = Vec::new();
    let mut grp = Vec::new();
    u16le(&mut grp, 0);
    u16le(&mut grp, 1);
    u16le(&mut grp, n as u16);
    for i in 0..n {
        let e = 6 + 16 * i;
        let (size, off) = (rd32(e + 8) as usize, rd32(e + 12) as usize);
        let id = (i + 1) as u16;
        grp.extend_from_slice(&ico[e..e + 12]); // w, h, colors, reserved, planes, bpp, bytes
        u16le(&mut grp, id);
        out.push(Res { ty: RT_ICON, id: id as u32, data: ico[off..off + size].to_vec() });
    }
    out.push(Res { ty: RT_GROUP_ICON, id: 1, data: grp });
    out
}

/// Builds the .rsrc section bytes and the offsets of the data-entry RVA fields to relocate.
fn rsrc_section(res: &[Res]) -> (Vec<u8>, Vec<u32>) {
    let mut types: Vec<u32> = res.iter().map(|r| r.ty).collect();
    types.sort();
    types.dedup();
    let dir_len = |entries: usize| 16 + 8 * entries;

    // Layout: root dir | type dirs | lang dirs | data entries | raw data
    let root_len = dir_len(types.len());
    let type_dirs_len: usize = types.iter().map(|t| dir_len(res.iter().filter(|r| r.ty == *t).count())).sum();
    let lang_dirs_start = root_len + type_dirs_len;
    let data_entries_start = lang_dirs_start + dir_len(1) * res.len();
    let raw_start = data_entries_start + 16 * res.len();

    // order resources by (type, id) so directory entries are sorted
    let mut order: Vec<usize> = (0..res.len()).collect();
    order.sort_by_key(|&i| (res[i].ty, res[i].id));

    let mut s = Vec::new();
    let dir_hdr = |s: &mut Vec<u8>, n: usize| {
        s.extend_from_slice(&[0; 12]); // characteristics, timestamp, major/minor version
        u16le(s, 0); // named entries
        u16le(s, n as u16); // id entries
    };

    // root
    dir_hdr(&mut s, types.len());
    let mut tdir = root_len;
    for t in &types {
        u32le(&mut s, *t);
        u32le(&mut s, 0x8000_0000 | tdir as u32);
        tdir += dir_len(res.iter().filter(|r| r.ty == *t).count());
    }
    // type dirs -> lang dirs (one per resource, in `order`)
    let mut k = 0;
    for t in &types {
        let items: Vec<usize> = order.iter().copied().filter(|&i| res[i].ty == *t).collect();
        dir_hdr(&mut s, items.len());
        for i in items {
            u32le(&mut s, res[i].id);
            u32le(&mut s, 0x8000_0000 | (lang_dirs_start + dir_len(1) * k) as u32);
            k += 1;
        }
    }
    // lang dirs -> data entries
    for k in 0..order.len() {
        dir_hdr(&mut s, 1);
        u32le(&mut s, LANG);
        u32le(&mut s, (data_entries_start + 16 * k) as u32);
    }
    // data entries (RVA fields get ADDR32NB relocations)
    let mut relocs = Vec::new();
    let mut raw = raw_start;
    let mut placed = Vec::new();
    for &i in &order {
        relocs.push(s.len() as u32);
        u32le(&mut s, raw as u32);
        u32le(&mut s, res[i].data.len() as u32);
        u32le(&mut s, 0);
        u32le(&mut s, 0);
        placed.push(raw);
        raw = (raw + res[i].data.len() + 7) & !7;
    }
    assert_eq!(s.len(), raw_start);
    for (&i, &at) in order.iter().zip(&placed) {
        s.resize(at, 0);
        s.extend_from_slice(&res[i].data);
    }
    s.resize((s.len() + 7) & !7, 0);
    (s, relocs)
}

fn coff(section: &[u8], relocs: &[u32]) -> Vec<u8> {
    let raw_ptr = 20 + 40;
    let reloc_ptr = raw_ptr + section.len();
    let sym_ptr = reloc_ptr + 10 * relocs.len();
    let mut o = Vec::new();
    // file header
    u16le(&mut o, 0x8664); // AMD64
    u16le(&mut o, 1);
    u32le(&mut o, 0);
    u32le(&mut o, sym_ptr as u32);
    u32le(&mut o, 1);
    u16le(&mut o, 0);
    u16le(&mut o, 0);
    // section header
    o.extend_from_slice(b".rsrc\0\0\0");
    u32le(&mut o, 0);
    u32le(&mut o, 0);
    u32le(&mut o, section.len() as u32);
    u32le(&mut o, raw_ptr as u32);
    u32le(&mut o, reloc_ptr as u32);
    u32le(&mut o, 0);
    u16le(&mut o, relocs.len() as u16);
    u16le(&mut o, 0);
    u32le(&mut o, 0x4000_0040); // INITIALIZED_DATA | MEM_READ
    o.extend_from_slice(section);
    for &r in relocs {
        u32le(&mut o, r);
        u32le(&mut o, 0); // symbol 0 = .rsrc
        u16le(&mut o, 3); // IMAGE_REL_AMD64_ADDR32NB
    }
    // symbol table: section symbol
    o.extend_from_slice(b".rsrc\0\0\0");
    u32le(&mut o, 0);
    u16le(&mut o, 1);
    u16le(&mut o, 0);
    o.push(3); // IMAGE_SYM_CLASS_STATIC
    o.push(0);
    u32le(&mut o, 4); // empty string table
    o
}

fn main() {
    println!("cargo:rerun-if-changed={ICO}");
    println!("cargo:rerun-if-changed=build.rs");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows")
        || env::var("CARGO_CFG_TARGET_ARCH").as_deref() != Ok("x86_64")
    {
        return;
    }
    let ico = fs::read(ICO).expect("read icon");
    let (sec, relocs) = rsrc_section(&icon_resources(&ico));
    let obj = coff(&sec, &relocs);
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("shotclip_res.o");
    fs::write(&out, obj).unwrap();
    println!("cargo:rustc-link-arg-bins={}", out.display());
}
