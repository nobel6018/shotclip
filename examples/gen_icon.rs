//! Renders assets/shrinkclip.ico (and a 256px PNG preview).
//! Run: `cargo run --example gen_icon`
//!
//! Design: rounded teal tile, white clipboard, two inward-pointing arrows (= shrink).

use image::{Rgba, RgbaImage};
use std::{fs, io::Cursor};

const SIZES: [u32; 7] = [16, 20, 24, 32, 48, 64, 256];
const SS: u32 = 8; // supersampling per axis

fn rrect(x: f32, y: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> bool {
    if x < x0 || x > x1 || y < y0 || y > y1 {
        return false;
    }
    let cx = x.clamp(x0 + r, x1 - r);
    let cy = y.clamp(y0 + r, y1 - r);
    (x - cx).powi(2) + (y - cy).powi(2) <= r * r
}

/// Triangle with tip at (tx, cy), base at x = bx, half-height h.
fn tri(x: f32, y: f32, tx: f32, bx: f32, cy: f32, h: f32) -> bool {
    let (lo, hi) = if tx < bx { (tx, bx) } else { (bx, tx) };
    if x < lo || x > hi {
        return false;
    }
    let t = (x - tx).abs() / (bx - tx).abs(); // 0 at tip, 1 at base
    (y - cy).abs() <= h * t
}

fn lerp(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

/// Colour (rgb, alpha) of a point in unit coordinates.
fn shade(x: f32, y: f32) -> Option<[f32; 3]> {
    let top = [0x2d as f32, 0xd4 as f32, 0xbf as f32];
    let bot = [0x0e as f32, 0x74 as f32, 0x90 as f32];
    let ink = [0x0b as f32, 0x4f as f32, 0x63 as f32];
    let white = [255.0, 255.0, 255.0];

    // clip (on top of everything)
    if rrect(x, y, 0.36, 0.13, 0.64, 0.29, 0.05) {
        return Some(ink);
    }
    // clipboard board
    if rrect(x, y, 0.21, 0.20, 0.79, 0.88, 0.08) {
        // inward arrows
        let cy = 0.58;
        if tri(x, y, 0.47, 0.28, cy, 0.15) || tri(x, y, 0.53, 0.72, cy, 0.15) {
            return Some(lerp(top, bot, 0.75));
        }
        return Some(white);
    }
    // background tile
    if rrect(x, y, 0.02, 0.02, 0.98, 0.98, 0.22) {
        return Some(lerp(top, bot, y));
    }
    None
}

fn render(size: u32) -> RgbaImage {
    let mut img = RgbaImage::new(size, size);
    let n = (SS * SS) as f32;
    for py in 0..size {
        for px in 0..size {
            let (mut r, mut g, mut b, mut a) = (0.0, 0.0, 0.0, 0.0);
            for sy in 0..SS {
                for sx in 0..SS {
                    let x = (px as f32 + (sx as f32 + 0.5) / SS as f32) / size as f32;
                    let y = (py as f32 + (sy as f32 + 0.5) / SS as f32) / size as f32;
                    if let Some(c) = shade(x, y) {
                        r += c[0];
                        g += c[1];
                        b += c[2];
                        a += 1.0;
                    }
                }
            }
            if a > 0.0 {
                img.put_pixel(px, py, Rgba([(r / a) as u8, (g / a) as u8, (b / a) as u8, (a / n * 255.0).round() as u8]));
            }
        }
    }
    img
}

fn png_bytes(img: &RgbaImage) -> Vec<u8> {
    let mut v = Vec::new();
    img.write_to(&mut Cursor::new(&mut v), image::ImageFormat::Png).unwrap();
    v
}

fn main() {
    let imgs: Vec<(u32, Vec<u8>)> = SIZES.iter().map(|&s| (s, png_bytes(&render(s)))).collect();

    // ICO with PNG-compressed entries (supported since Windows Vista).
    let mut ico = Vec::new();
    ico.extend_from_slice(&[0, 0, 1, 0]);
    ico.extend_from_slice(&(imgs.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * imgs.len() as u32;
    for (s, data) in &imgs {
        let d = if *s >= 256 { 0 } else { *s as u8 };
        ico.extend_from_slice(&[d, d, 0, 0]);
        ico.extend_from_slice(&1u16.to_le_bytes()); // planes
        ico.extend_from_slice(&32u16.to_le_bytes()); // bpp
        ico.extend_from_slice(&(data.len() as u32).to_le_bytes());
        ico.extend_from_slice(&offset.to_le_bytes());
        offset += data.len() as u32;
    }
    for (_, data) in &imgs {
        ico.extend_from_slice(data);
    }
    fs::create_dir_all("assets").unwrap();
    fs::write("assets/shrinkclip.ico", ico).unwrap();
    fs::write("assets/shrinkclip.png", &imgs.last().unwrap().1).unwrap();
    println!("wrote assets/shrinkclip.ico ({} sizes) and assets/shrinkclip.png", imgs.len());
}
