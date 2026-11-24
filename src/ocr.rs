//! Text recognition with the OCR engine built into Windows (Windows.Media.Ocr), using the
//! user's profile languages (e.g. Korean + English). No download or cloud service involved.

use windows::{
    Graphics::Imaging::{BitmapAlphaMode, BitmapPixelFormat, SoftwareBitmap},
    Media::Ocr::OcrEngine,
    Storage::Streams::DataWriter,
};

/// Recognises the text in a BGRA image captured on a display of `dpi`; lines are joined with newlines.
pub fn recognize(w: i32, h: i32, bgra: &[u8], dpi: u32) -> Result<String, String> {
    // The engine wants glyphs ~20px+ tall: upscale 100-125 % screens, leave hi-DPI ones alone
    // (upscaling already-large text measurably hurts: l->1, u->lJ).
    recognize_k(w, h, bgra, if dpi >= 144 { 1 } else { 2 })
}

fn recognize_k(w: i32, h: i32, bgra: &[u8], k: i32) -> Result<String, String> {
    let err = |e: windows::core::Error| e.message().to_string();
    let engine = OcrEngine::TryCreateFromUserProfileLanguages()
        .map_err(|_| "OCR 언어 팩이 없습니다 (설정 > 언어에서 설치)".to_string())?;

    let max = OcrEngine::MaxImageDimension().map_err(err)? as i32;
    let k = if w.max(h) * k > max { 1 } else { k };
    if w * k > max || h * k > max {
        return Err(format!("영역이 너무 큽니다 (최대 {max}px)"));
    }
    let (sw, sh) = (w * k, h * k);
    let mut px = vec![0u8; (sw * sh * 4) as usize];
    for y in 0..sh {
        for x in 0..sw {
            // Bilinear: nearest-neighbour blocks confuse the recogniser (l/I, 0/o).
            let fx = ((x as f32 + 0.5) / k as f32 - 0.5).clamp(0.0, (w - 1) as f32);
            let fy = ((y as f32 + 0.5) / k as f32 - 0.5).clamp(0.0, (h - 1) as f32);
            let (x0, y0) = (fx as i32, fy as i32);
            let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
            let (ax, ay) = (fx - x0 as f32, fy - y0 as f32);
            let at = |xx: i32, yy: i32, c: usize| bgra[((yy * w + xx) * 4) as usize + c] as f32;
            let d = ((y * sw + x) * 4) as usize;
            for c in 0..3 {
                let top = at(x0, y0, c) * (1.0 - ax) + at(x1, y0, c) * ax;
                let bot = at(x0, y1, c) * (1.0 - ax) + at(x1, y1, c) * ax;
                px[d + c] = (top * (1.0 - ay) + bot * ay + 0.5) as u8;
            }
            px[d + 3] = 255;
        }
    }

    let writer = DataWriter::new().map_err(err)?;
    writer.WriteBytes(&px).map_err(err)?;
    let buf = writer.DetachBuffer().map_err(err)?;
    let bmp = SoftwareBitmap::CreateCopyWithAlphaFromBuffer(&buf, BitmapPixelFormat::Bgra8, sw, sh, BitmapAlphaMode::Premultiplied)
        .map_err(err)?;
    let result = engine.RecognizeAsync(&bmp).map_err(err)?.get().map_err(err)?;
    let mut lines = Vec::new();
    for line in result.Lines().map_err(err)? {
        lines.push(line.Text().map_err(err)?.to_string_lossy());
    }
    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    /// Reads one line of an existing screenshot: SHOTCLIP_OCR_PNG=... cargo test -- --ignored
    #[test]
    #[ignore]
    fn ocr_screenshot() {
        let path = std::env::var("SHOTCLIP_OCR_PNG").expect("SHOTCLIP_OCR_PNG");
        let img = crate::optimize::decode_png(&std::fs::read(path).unwrap()).unwrap();
        let crop = |x0: u32, y0: u32, x1: u32, y1: u32| {
            let mut bgra = Vec::new();
            for y in y0..y1 {
                for x in x0..x1 {
                    let p = &img.px[((y * img.w + x) * 4) as usize..][..4];
                    bgra.extend_from_slice(&[p[2], p[1], p[0], 255]);
                }
            }
            super::recognize((x1 - x0) as i32, (y1 - y0) as i32, &bgra, 144).unwrap()
        };
        let en = crop(450, 1245, 1500, 1295);
        let ko = crop(560, 490, 1600, 540);
        println!("OCR: {en:?}\nOCR: {ko:?}");
        assert!(en.contains("autostart migration"), "{en}");
        assert!(ko.contains("자동"), "{ko}");
    }
}
