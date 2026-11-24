//! Image decoding / quantisation / encoding.
//!
//! Mirrors Clop's default clipboard behaviour: pngquant-style lossy palette quantisation
//! (quality 0-100, speed 4 = Clop's "normal" compression factor 30), metadata stripped,
//! and the original is kept if the result would be larger.

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;

/// Straight (non-premultiplied) RGBA8 pixels.
pub struct Rgba {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u8>,
}

pub fn decode_png(bytes: &[u8]) -> Result<Rgba> {
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)?.into_rgba8();
    let (w, h) = img.dimensions();
    Ok(Rgba { w, h, px: img.into_raw() })
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// Decodes a CF_DIB / CF_DIBV5 payload (24 or 32 bpp, uncompressed or BI_BITFIELDS).
pub fn decode_dib(b: &[u8]) -> Result<Rgba> {
    if b.len() < 40 {
        return Err("DIB too short".into());
    }
    let hsize = u32_at(b, 0) as usize;
    let w = u32_at(b, 4) as i32;
    let h = u32_at(b, 8) as i32;
    let bpp = u16_at(b, 14);
    let comp = u32_at(b, 16);
    let clr_used = u32_at(b, 32) as usize;
    if w <= 0 || h == 0 || hsize < 40 {
        return Err("bad DIB size".into());
    }
    if bpp != 24 && bpp != 32 {
        return Err(format!("unsupported DIB bit depth {bpp}").into());
    }
    if comp != 0 && comp != 3 {
        return Err(format!("unsupported DIB compression {comp}").into());
    }
    let mut off = hsize;
    if comp == 3 && hsize == 40 {
        off += 12; // colour masks follow a plain BITMAPINFOHEADER
    }
    off += clr_used * 4;

    let (w, hh, top_down) = (w as usize, h.unsigned_abs() as usize, h < 0);
    let stride = (w * bpp as usize).div_ceil(32) * 4;
    if b.len() < off + stride * hh {
        return Err("DIB pixel data truncated".into());
    }
    // Alpha is only meaningful with a V4/V5 header that declares an alpha mask.
    let use_alpha = bpp == 32 && comp == 3 && hsize >= 56 && u32_at(b, 52) != 0;

    let mut px = vec![0u8; w * hh * 4];
    let mut any_alpha = false;
    for y in 0..hh {
        let sy = if top_down { y } else { hh - 1 - y };
        let row = &b[off + sy * stride..];
        for x in 0..w {
            let d = &mut px[(y * w + x) * 4..][..4];
            if bpp == 24 {
                let s = &row[x * 3..];
                d.copy_from_slice(&[s[2], s[1], s[0], 255]);
            } else {
                let s = &row[x * 4..];
                any_alpha |= s[3] != 0;
                d.copy_from_slice(&[s[2], s[1], s[0], if use_alpha { s[3] } else { 255 }]);
            }
        }
    }
    if use_alpha && !any_alpha {
        // All-zero alpha means the producer didn't actually fill it in.
        px.iter_mut().skip(3).step_by(4).for_each(|a| *a = 255);
    }
    Ok(Rgba { w: w as u32, h: hh as u32, px })
}

/// Lossless PNG (RGB when fully opaque). Used for the backup of the original.
pub fn encode_png(img: &Rgba) -> Result<Vec<u8>> {
    let opaque = img.px.chunks_exact(4).all(|c| c[3] == 255);
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, img.w, img.h);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Default);
        let mut wr;
        if opaque {
            enc.set_color(png::ColorType::Rgb);
            wr = enc.write_header()?;
            let rgb: Vec<u8> = img.px.chunks_exact(4).flat_map(|c| [c[0], c[1], c[2]]).collect();
            wr.write_image_data(&rgb)?;
        } else {
            enc.set_color(png::ColorType::Rgba);
            wr = enc.write_header()?;
            wr.write_image_data(&img.px)?;
        }
        wr.finish()?;
    }
    Ok(out)
}

/// Lossy palette quantisation (libimagequant = the engine behind pngquant).
/// Returns the PNG bytes and the remapped pixels (so CF_DIB matches the PNG exactly).
pub fn quantize_png(img: &Rgba) -> Result<(Vec<u8>, Rgba)> {
    // Clop's default ("normal", factor 30): pngquant --force --speed 4 --quality 0-100, 256 colours.
    quantize_png_with(img, 4, 256)
}

/// pngquant with an explicit speed and palette size (Clop's downscale fallback uses
/// `pngquant <colors> --speed 1`).
pub fn quantize_png_with(img: &Rgba, speed: i32, colors: u32) -> Result<(Vec<u8>, Rgba)> {
    let mut liq = imagequant::new();
    liq.set_speed(speed)?;
    liq.set_quality(0, 100)?;
    liq.set_max_colors(colors.clamp(2, 256))?;
    let pixels: Vec<imagequant::RGBA> = img
        .px
        .chunks_exact(4)
        .map(|c| imagequant::RGBA::new(c[0], c[1], c[2], c[3]))
        .collect();
    let mut qimg = liq.new_image(pixels, img.w as usize, img.h as usize, 0.0)?;
    let mut res = liq.quantize(&mut qimg)?;
    res.set_dithering_level(1.0)?;
    let (pal, idx) = res.remapped(&mut qimg)?;

    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, img.w, img.h);
        enc.set_color(png::ColorType::Indexed);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Best);
        enc.set_palette(pal.iter().flat_map(|c| [c.r, c.g, c.b]).collect::<Vec<u8>>());
        let alpha: Vec<u8> = pal.iter().map(|c| c.a).collect();
        if let Some(last) = alpha.iter().rposition(|&a| a != 255) {
            enc.set_trns(alpha[..=last].to_vec());
        }
        let mut wr = enc.write_header()?;
        wr.write_image_data(&idx)?;
        wr.finish()?;
    }
    let px = idx
        .iter()
        .flat_map(|&i| {
            let c = pal[i as usize];
            [c.r, c.g, c.b, c.a]
        })
        .collect();
    Ok((out, Rgba { w: img.w, h: img.h, px }))
}

/// Distinct colours up to `cap` (None = more than `cap`). Like Clop, large images are sampled
/// nearest-neighbour to at most ~1 M pixels, which never invents colours.
pub fn unique_colors(img: &Rgba, cap: usize) -> Option<usize> {
    let n = img.w as usize * img.h as usize;
    let step = ((n as f64 / 1_000_000.0).sqrt().ceil() as usize).max(1);
    let mut seen = std::collections::HashSet::new();
    for y in (0..img.h as usize).step_by(step) {
        for x in (0..img.w as usize).step_by(step) {
            let i = (y * img.w as usize + x) * 4;
            seen.insert(u32::from_le_bytes([img.px[i], img.px[i + 1], img.px[i + 2], img.px[i + 3]]));
            if seen.len() > cap {
                return None;
            }
        }
    }
    Some(seen.len())
}

/// Clop's fix for downscales that grow the file (anti-aliasing turns a few flat colours into many):
/// re-quantise toward the original's colour count, then 128/64/32/16, at speed 1; the first one
/// smaller than `limit` bytes wins. None = keep the original.
pub fn shrink_below(img: &Rgba, original: &Rgba, limit: usize) -> Option<(Vec<u8>, Rgba)> {
    let mut candidates: Vec<u32> = Vec::new();
    if let Some(n) = unique_colors(original, 256) {
        candidates.push(n.clamp(2, 256) as u32);
    }
    for c in [128, 64, 32, 16] {
        if !candidates.contains(&c) {
            candidates.push(c);
        }
    }
    candidates
        .into_iter()
        .filter_map(|c| quantize_png_with(img, 1, c).ok())
        .find(|(png, _)| png.len() < limit)
}

pub fn resize(img: &Rgba, factor: f64) -> Result<Rgba> {
    let nw = ((img.w as f64 * factor).round() as u32).max(1);
    let nh = ((img.h as f64 * factor).round() as u32).max(1);
    let src = image::RgbaImage::from_raw(img.w, img.h, img.px.clone()).ok_or("bad buffer")?;
    let out = image::imageops::resize(&src, nw, nh, image::imageops::FilterType::Lanczos3);
    Ok(Rgba { w: nw, h: nh, px: out.into_raw() })
}

/// CF_DIB payload: BITMAPINFOHEADER + bottom-up 32bpp BGRA.
pub fn encode_dib(img: &Rgba) -> Vec<u8> {
    let (w, h) = (img.w as usize, img.h as usize);
    let mut out = Vec::with_capacity(40 + w * h * 4);
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(h as i32).to_le_bytes()); // positive = bottom-up
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    out.extend_from_slice(&((w * h * 4) as u32).to_le_bytes());
    out.extend_from_slice(&[0u8; 16]); // ppm x/y, clrUsed, clrImportant
    for y in (0..h).rev() {
        for p in img.px[y * w * 4..(y + 1) * w * 4].chunks_exact(4) {
            out.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
        }
    }
    out
}

/// Clop's downscale steps: 100 → 75 → 50 → 40 → 30 → 20 → 10 %.
pub fn next_factor(f: f64) -> f64 {
    let n = if f > 0.5 { f - 0.25 } else { f - 0.1 };
    (n.max(0.1) * 100.0).round() / 100.0
}

pub fn human(bytes: usize) -> String {
    match bytes {
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1 << 20) as f64),
        b if b >= 1 << 10 => format!("{} KB", b >> 10),
        b => format!("{b} B"),
    }
}

#[cfg(test)]
mod clop_tests {
    use super::*;

    /// Flat 3-colour "UI" image: the downscale re-quantised toward 3 colours must beat the original.
    #[test]
    fn downscale_of_flat_image_does_not_grow() {
        let (w, h) = (400u32, 300u32);
        let mut px = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let c: [u8; 4] = if (x / 7 + y / 5) % 3 == 0 { [20, 20, 20, 255] } else if x % 11 < 2 { [30, 120, 220, 255] } else { [250, 250, 250, 255] };
                px.extend_from_slice(&c);
            }
        }
        let src = Rgba { w, h, px };
        assert_eq!(unique_colors(&src, 256), Some(3));
        let (orig, _) = quantize_png(&src).unwrap();
        let small = resize(&src, 0.75).unwrap();
        if let Some((png, pix)) = shrink_below(&small, &src, orig.len()) {
            assert!(png.len() < orig.len());
            assert_eq!((pix.w, pix.h), (300, 225));
        }
    }
}
