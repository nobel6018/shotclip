//! Annotation model and software rasteriser for the post-capture editor (editor.rs).
//!
//! Annotations stay vector objects in image coordinates; `render` draws them onto a copy of the
//! original, so undo / move / re-edit never touch pixels and blur/erase always sample the
//! untouched capture. Strokes are antialiased with a distance-based coverage estimate; text
//! uses a GDI greyscale-antialiased glyph mask.

use crate::optimize::Rgba;
use std::{
    mem::{size_of, zeroed},
    ptr::null_mut,
};
use windows_sys::Win32::{Foundation::RECT, Graphics::Gdi::*};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    Arrow,
    Rect,
    Oval,
    Line,
    Pen,
    Text,
    Highlight,
    Blur,
    Erase,
    Counter,
    Crop,
}

#[derive(Clone, Debug)]
pub enum Shape {
    Arrow,
    Rect,
    Oval,
    Line,
    Pen(Vec<(f32, f32)>),
    Text(String),
    Highlight,
    Blur,
    Erase,
    Counter(u32),
}

#[derive(Clone, Debug)]
pub struct Obj {
    pub shape: Shape,
    /// Start / end point in image pixels. Counter: `a` is the centre. Text: `a` is the top-left
    /// of the glyphs and `b` the tip of the speech-bubble tail (`b` inside the box = no tail).
    pub a: (f32, f32),
    pub b: (f32, f32),
    /// 0xRRGGBB
    pub color: u32,
    /// 0 = thin, 1 = medium, 2 = thick
    pub size: u8,
}

/// Physical-pixel scale for stroke widths/fonts: captures of HiDPI screens get thicker strokes.
#[derive(Clone, Copy)]
pub struct Scale(pub f32);

impl Scale {
    pub fn stroke(self, size: u8) -> f32 {
        [3.0, 5.0, 9.0][size.min(2) as usize] * self.0
    }
    pub fn font_px(self, size: u8) -> i32 {
        ([18.0, 26.0, 40.0][size.min(2) as usize] * self.0).round() as i32
    }
    pub fn counter_r(self, size: u8) -> f32 {
        [12.0, 16.0, 22.0][size.min(2) as usize] * self.0
    }
    pub fn blur_r(self) -> usize {
        (10.0 * self.0).round().max(4.0) as usize
    }
}

impl Obj {
    pub fn norm(&self) -> (f32, f32, f32, f32) {
        let (x0, x1) = if self.a.0 <= self.b.0 { (self.a.0, self.b.0) } else { (self.b.0, self.a.0) };
        let (y0, y1) = if self.a.1 <= self.b.1 { (self.a.1, self.b.1) } else { (self.b.1, self.a.1) };
        (x0, y0, x1, y1)
    }

    /// Bounding box in image pixels, including stroke width.
    pub fn bounds(&self, s: Scale) -> (f32, f32, f32, f32) {
        let pad = s.stroke(self.size) * 2.0 + 2.0;
        match &self.shape {
            Shape::Pen(pts) => {
                let mut r = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
                for &(x, y) in pts {
                    r = (r.0.min(x), r.1.min(y), r.2.max(x), r.3.max(y));
                }
                (r.0 - pad, r.1 - pad, r.2 + pad, r.3 + pad)
            }
            Shape::Text(_) => {
                let bb = self.bubble(s).unwrap();
                let (mut x0, mut y0, mut x1, mut y1) = (bb.x0, bb.y0, bb.x1, bb.y1);
                if bb.tail.is_some() {
                    (x0, y0, x1, y1) = (x0.min(self.b.0), y0.min(self.b.1), x1.max(self.b.0), y1.max(self.b.1));
                }
                (x0 - 2.0, y0 - 2.0, x1 + 2.0, y1 + 2.0)
            }
            Shape::Counter(_) => {
                let r = s.counter_r(self.size) + 2.0;
                (self.a.0 - r, self.a.1 - r, self.a.0 + r, self.a.1 + r)
            }
            _ => {
                let (x0, y0, x1, y1) = self.norm();
                (x0 - pad, y0 - pad, x1 + pad, y1 + pad)
            }
        }
    }

    pub fn translate(&mut self, dx: f32, dy: f32) {
        self.a = (self.a.0 + dx, self.a.1 + dy);
        self.b = (self.b.0 + dx, self.b.1 + dy);
        if let Shape::Pen(pts) = &mut self.shape {
            for p in pts {
                *p = (p.0 + dx, p.1 + dy);
            }
        }
    }

    /// Hit test in image pixels with `tol` slack.
    pub fn hit(&self, s: Scale, x: f32, y: f32, tol: f32) -> bool {
        let hw = s.stroke(self.size) / 2.0 + tol;
        let (x0, y0, x1, y1) = self.norm();
        match &self.shape {
            Shape::Arrow | Shape::Line => seg_dist(x, y, self.a, self.b) <= hw,
            Shape::Pen(pts) => pts.windows(2).any(|w| seg_dist(x, y, w[0], w[1]) <= hw),
            Shape::Rect => rect_dist(x, y, x0, y0, x1, y1).abs() <= hw,
            Shape::Oval => ellipse_dist(x, y, x0, y0, x1, y1).abs() <= hw,
            Shape::Text(_) => {
                let bb = self.bubble(s).unwrap();
                rr_dist(x, y, &bb) <= tol || bb.tail.is_some_and(|t| tri_dist(x, y, t) <= tol)
            }
            Shape::Highlight | Shape::Blur | Shape::Erase | Shape::Counter(_) => {
                let b = self.bounds(s);
                x >= b.0 - tol && x <= b.2 + tol && y >= b.1 - tol && y <= b.3 + tol
            }
        }
    }

    /// Hit only near the outline (area objects: so you can draw inside a blur/highlight).
    pub fn hit_edge(&self, s: Scale, x: f32, y: f32, tol: f32) -> bool {
        match self.shape {
            Shape::Highlight | Shape::Blur | Shape::Erase => {
                let (x0, y0, x1, y1) = self.norm();
                rect_dist(x, y, x0, y0, x1, y1).abs() <= tol + 2.0
            }
            _ => self.hit(s, x, y, tol),
        }
    }

    /// Shottr-style text box (ObjText): a rounded box filled with the annotation colour, with an
    /// optional tail toward `b`. Corner radius = font size × 0.25 (Shottr: `fontsize·scale·0.25`);
    /// the tail's base widens with its length (Shottr: `fontsize·(√dist/35 + 0.9)`), and it sits
    /// on the side facing the tip, kept clear of the rounded corners.
    pub fn bubble(&self, s: Scale) -> Option<Bubble> {
        let Shape::Text(t) = &self.shape else { return None };
        let px = s.font_px(self.size) as f32;
        let (w, h) = text_size(if t.is_empty() { " " } else { t }, px as i32);
        let (padx, pady) = (px * 0.35, px * 0.15);
        let (x0, y0) = (self.a.0 - padx, self.a.1 - pady);
        let (x1, y1) = (self.a.0 + w as f32 + padx, self.a.1 + h as f32 + pady);
        let r = (px * 0.25).min((y1 - y0) / 2.0).min((x1 - x0) / 2.0);
        let mut bb = Bubble { x0, y0, x1, y1, r, tail: None };
        let (tx, ty) = self.b;
        let ox = (x0 - tx).max(tx - x1).max(0.0);
        let oy = (y0 - ty).max(ty - y1).max(0.0);
        if ox.hypot(oy) < px * 0.4 {
            return Some(bb);
        }
        let dist = ox.hypot(oy);
        let base = |lo: f32, hi: f32, along: f32| {
            let hw = (px * 0.375 * (dist.sqrt() / 35.0 + 0.9)).min((hi - lo) / 2.0 - r).max(1.0);
            let (l, h) = (lo + r + hw, hi - r - hw);
            let c = if l <= h { along.clamp(l, h) } else { (lo + hi) / 2.0 };
            (c - hw, c + hw)
        };
        bb.tail = Some(if ox > oy {
            // Left or right side; the base overlaps the box by 1 px so there is no seam.
            let (sx, inw) = if tx < x0 { (x0, 1.0) } else { (x1, -1.0) };
            let (b0, b1) = base(y0, y1, ty);
            [(sx + inw, b0), (sx + inw, b1), (tx, ty)]
        } else {
            let (sy, inw) = if ty < y0 { (y0, 1.0) } else { (y1, -1.0) };
            let (b0, b1) = base(x0, x1, tx);
            [(b0, sy + inw), (b1, sy + inw), (tx, ty)]
        });
        Some(bb)
    }

    /// Too small to keep (a click without drag for drag-shaped tools).
    pub fn degenerate(&self) -> bool {
        match &self.shape {
            Shape::Text(t) => t.trim().is_empty(),
            Shape::Counter(_) => false,
            Shape::Pen(p) => p.len() < 2,
            Shape::Arrow | Shape::Line => {
                (self.a.0 - self.b.0).hypot(self.a.1 - self.b.1) < 4.0
            }
            _ => {
                let (x0, y0, x1, y1) = self.norm();
                x1 - x0 < 3.0 || y1 - y0 < 3.0
            }
        }
    }
}

// ---------------------------------------------------------------- geometry

/// Text box geometry (see `Obj::bubble`).
#[derive(Clone, Copy, Debug)]
pub struct Bubble {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    pub r: f32,
    pub tail: Option<[(f32, f32); 3]>,
}

/// Signed distance to the rounded box (negative inside).
fn rr_dist(px: f32, py: f32, b: &Bubble) -> f32 {
    rect_dist(px, py, b.x0 + b.r, b.y0 + b.r, b.x1 - b.r, b.y1 - b.r) - b.r
}

fn seg_dist(px: f32, py: f32, a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let l2 = dx * dx + dy * dy;
    let t = if l2 > 0.0 { (((px - a.0) * dx + (py - a.1) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
    (px - a.0 - t * dx).hypot(py - a.1 - t * dy)
}

/// Signed distance to the rectangle outline (negative inside).
fn rect_dist(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32) -> f32 {
    let (cx, cy, hx, hy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0, (x1 - x0) / 2.0, (y1 - y0) / 2.0);
    let (qx, qy) = ((px - cx).abs() - hx, (py - cy).abs() - hy);
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0)
}

/// Approximate signed distance to an axis-aligned ellipse.
fn ellipse_dist(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32) -> f32 {
    let (a, b) = (((x1 - x0) / 2.0).max(0.5), ((y1 - y0) / 2.0).max(0.5));
    let (x, y) = (px - (x0 + x1) / 2.0, py - (y0 + y1) / 2.0);
    let k0 = (x / a).hypot(y / b);
    let k1 = (x / (a * a)).hypot(y / (b * b));
    if k1 == 0.0 {
        return -a.min(b);
    }
    k0 * (k0 - 1.0) / k1
}

// ---------------------------------------------------------------- canvas

struct Canvas<'a> {
    img: &'a mut Rgba,
}

impl Canvas<'_> {
    /// Blends `color` at (x, y) with coverage `c` (0..1).
    #[inline]
    fn blend(&mut self, x: i32, y: i32, color: u32, c: f32) {
        if c <= 0.0 || x < 0 || y < 0 || x >= self.img.w as i32 || y >= self.img.h as i32 {
            return;
        }
        let c = c.min(1.0);
        let i = (y as usize * self.img.w as usize + x as usize) * 4;
        let rgb = [(color >> 16) as u8, (color >> 8) as u8, color as u8];
        for k in 0..3 {
            let d = self.img.px[i + k] as f32;
            self.img.px[i + k] = (d + (rgb[k] as f32 - d) * c).round() as u8;
        }
        self.img.px[i + 3] = 255;
    }

    /// Fills pixels of the clipped box with coverage from `f(px, py)` (pixel centre coords).
    fn shade(&mut self, b: (f32, f32, f32, f32), color: u32, f: impl Fn(f32, f32) -> f32) {
        let (w, h) = (self.img.w as i32, self.img.h as i32);
        let x0 = (b.0.floor() as i32).max(0);
        let y0 = (b.1.floor() as i32).max(0);
        let x1 = (b.2.ceil() as i32).min(w - 1);
        let y1 = (b.3.ceil() as i32).min(h - 1);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let c = f(x as f32 + 0.5, y as f32 + 0.5);
                if c > 0.0 {
                    self.blend(x, y, color, c);
                }
            }
        }
    }
}

#[inline]
fn cov(half: f32, d: f32) -> f32 {
    (half - d + 0.5).clamp(0.0, 1.0)
}

fn clip_box(b: (f32, f32, f32, f32), w: u32, h: u32) -> Option<(usize, usize, usize, usize)> {
    let x0 = b.0.round().max(0.0) as usize;
    let y0 = b.1.round().max(0.0) as usize;
    let x1 = (b.2.round().max(0.0) as usize).min(w as usize);
    let y1 = (b.3.round().max(0.0) as usize).min(h as usize);
    (x1 > x0 && y1 > y0).then_some((x0, y0, x1, y1))
}

/// Draws every object over a copy of `base`.
pub fn render(base: &Rgba, objs: &[Obj], s: Scale) -> Rgba {
    let mut out = Rgba { w: base.w, h: base.h, px: base.px.clone() };
    for o in objs {
        draw(&mut out, base, o, s);
    }
    out
}

/// Draws one object onto `out`; blur/erase sample `orig`.
pub fn draw(out: &mut Rgba, orig: &Rgba, o: &Obj, s: Scale) {
    let hw = s.stroke(o.size) / 2.0;
    let mut cv = Canvas { img: out };
    match &o.shape {
        Shape::Line => {
            let (a, b) = (o.a, o.b);
            cv.shade(o.bounds(s), o.color, |x, y| cov(hw, seg_dist(x, y, a, b)));
        }
        Shape::Arrow => draw_arrow(&mut cv, o, s),
        Shape::Pen(pts) => {
            if pts.len() == 1 {
                let p = pts[0];
                cv.shade(o.bounds(s), o.color, |x, y| cov(hw, (x - p.0).hypot(y - p.1)));
            } else {
                // Max coverage over segments, so joints don't double-blend.
                let b = o.bounds(s);
                let Some((x0, y0, x1, y1)) = clip_box(b, cv.img.w, cv.img.h) else { return };
                let bw = x1 - x0;
                let mut m = vec![0f32; bw * (y1 - y0)];
                for seg in pts.windows(2) {
                    let (p, q) = (seg[0], seg[1]);
                    let sx0 = ((p.0.min(q.0) - hw - 1.0).floor().max(x0 as f32)) as usize;
                    let sx1 = ((p.0.max(q.0) + hw + 1.0).ceil().min(x1 as f32)) as usize;
                    let sy0 = ((p.1.min(q.1) - hw - 1.0).floor().max(y0 as f32)) as usize;
                    let sy1 = ((p.1.max(q.1) + hw + 1.0).ceil().min(y1 as f32)) as usize;
                    for y in sy0..sy1 {
                        for x in sx0..sx1 {
                            let c = cov(hw, seg_dist(x as f32 + 0.5, y as f32 + 0.5, p, q));
                            let k = &mut m[(y - y0) * bw + (x - x0)];
                            *k = k.max(c);
                        }
                    }
                }
                for y in y0..y1 {
                    for x in x0..x1 {
                        cv.blend(x as i32, y as i32, o.color, m[(y - y0) * bw + (x - x0)]);
                    }
                }
            }
        }
        Shape::Rect => {
            let (x0, y0, x1, y1) = o.norm();
            cv.shade(o.bounds(s), o.color, |x, y| cov(hw, rect_dist(x, y, x0, y0, x1, y1).abs()));
        }
        Shape::Oval => {
            let (x0, y0, x1, y1) = o.norm();
            cv.shade(o.bounds(s), o.color, |x, y| cov(hw, ellipse_dist(x, y, x0, y0, x1, y1).abs()));
        }
        Shape::Highlight => {
            // Marker: multiply blend, so text underneath stays readable.
            let Some((x0, y0, x1, y1)) = clip_box(o.norm(), cv.img.w, cv.img.h) else { return };
            let rgb = [(o.color >> 16) as u8, (o.color >> 8) as u8, o.color as u8];
            let w = cv.img.w as usize;
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = (y * w + x) * 4;
                    for k in 0..3 {
                        cv.img.px[i + k] = (cv.img.px[i + k] as u32 * rgb[k] as u32 / 255) as u8;
                    }
                }
            }
        }
        Shape::Blur => {
            let Some(r) = clip_box(o.norm(), cv.img.w, cv.img.h) else { return };
            blur_region(cv.img, orig, r, s.blur_r());
        }
        Shape::Erase => {
            let Some((x0, y0, x1, y1)) = clip_box(o.norm(), cv.img.w, cv.img.h) else { return };
            let c = border_color(orig, (x0, y0, x1, y1));
            let w = cv.img.w as usize;
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = (y * w + x) * 4;
                    cv.img.px[i..i + 4].copy_from_slice(&[(c >> 16) as u8, (c >> 8) as u8, c as u8, 255]);
                }
            }
        }
        Shape::Text(t) => {
            // Shottr: a filled rounded box (+ tail) in the colour, text in a contrasting colour.
            let bb = o.bubble(s).unwrap();
            cv.shade(o.bounds(s), o.color, |x, y| {
                let d = bb.tail.map_or(f32::MAX, |t| tri_dist(x, y, t)).min(rr_dist(x, y, &bb));
                cov(0.0, d)
            });
            let fg = if luma(o.color) > 170 { 0x111111 } else { 0xFFFFFF };
            if let Some((mw, mh, mask)) = text_mask(t, s.font_px(o.size)) {
                blit_mask(&mut cv, &mask, mw, mh, o.a.0.round() as i32, o.a.1.round() as i32, fg, 1.0);
            }
        }
        Shape::Counter(n) => {
            let rad = s.counter_r(o.size);
            let (cx, cy) = o.a;
            cv.shade(o.bounds(s), o.color, |x, y| cov(rad, (x - cx).hypot(y - cy)));
            let label = n.to_string();
            let px = (rad * if n >= &10 { 1.05 } else { 1.3 }) as i32;
            if let Some((mw, mh, mask)) = text_mask(&label, px) {
                let fg = if luma(o.color) > 170 { 0x000000 } else { 0xFFFFFF };
                let (ax, ay) = ((cx - mw as f32 / 2.0).round() as i32, (cy - mh as f32 / 2.0).round() as i32);
                blit_mask(&mut cv, &mask, mw, mh, ax, ay, fg, 1.0);
            }
        }
    }
}

fn draw_arrow(cv: &mut Canvas, o: &Obj, s: Scale) {
    let w = s.stroke(o.size);
    let (a, b) = (o.a, o.b);
    let len = (b.0 - a.0).hypot(b.1 - a.1);
    if len < 1.0 {
        return;
    }
    let (ux, uy) = ((b.0 - a.0) / len, (b.1 - a.1) / len);
    let head = (w * 3.2 + 8.0 * s.0).min(len * 0.6);
    let half = head * 0.55;
    let base = (b.0 - ux * head, b.1 - uy * head);
    let l = (base.0 - uy * half, base.1 + ux * half);
    let r = (base.0 + uy * half, base.1 - ux * half);
    // Shaft stops inside the head so the tip stays sharp.
    let shaft_end = (b.0 - ux * head * 0.5, b.1 - uy * head * 0.5);
    let hw = w / 2.0;
    let tri = [b, l, r];
    cv.shade(o.bounds(s), o.color, |x, y| {
        let shaft = cov(hw, seg_dist(x, y, a, shaft_end));
        let d = tri_dist(x, y, tri);
        shaft.max(cov(0.0, d))
    });
}

/// Signed distance to a triangle (negative inside).
fn tri_dist(px: f32, py: f32, t: [(f32, f32); 3]) -> f32 {
    let mut d = f32::MAX;
    let mut inside = true;
    let sign = |p: (f32, f32), q: (f32, f32)| (q.0 - p.0) * (py - p.1) - (q.1 - p.1) * (px - p.0);
    let s0 = sign(t[0], t[1]).signum();
    for i in 0..3 {
        let (p, q) = (t[i], t[(i + 1) % 3]);
        d = d.min(seg_dist(px, py, p, q));
        if sign(p, q).signum() != s0 {
            inside = false;
        }
    }
    if inside { -d } else { d }
}

fn luma(c: u32) -> u32 {
    (((c >> 16) & 255) * 299 + ((c >> 8) & 255) * 587 + (c & 255) * 114) / 1000
}

fn blit_mask(cv: &mut Canvas, m: &[u8], mw: usize, mh: usize, ax: i32, ay: i32, color: u32, k: f32) {
    for y in 0..mh {
        for x in 0..mw {
            let a = m[y * mw + x];
            if a > 0 {
                cv.blend(ax + x as i32, ay + y as i32, color, a as f32 / 255.0 * k);
            }
        }
    }
}

#[allow(dead_code)]
fn dilate(m: &[u8], w: usize, h: usize, r: i32) -> Vec<u8> {
    let mut out = vec![0u8; m.len()];
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let mut v = 0u8;
            for dy in -r..=r {
                for dx in -r..=r {
                    if dx * dx + dy * dy > r * r + r {
                        continue;
                    }
                    let (sx, sy) = (x + dx, y + dy);
                    if sx >= 0 && sy >= 0 && (sx as usize) < w && (sy as usize) < h {
                        v = v.max(m[sy as usize * w + sx as usize]);
                    }
                }
            }
            out[y as usize * w + x as usize] = v;
        }
    }
    out
}

/// Three box-blur passes (≈ gaussian) of `orig` inside r, written to `out`.
fn blur_region(out: &mut Rgba, orig: &Rgba, (x0, y0, x1, y1): (usize, usize, usize, usize), rad: usize) {
    let (w, h) = (x1 - x0, y1 - y0);
    let iw = orig.w as usize;
    let mut buf: Vec<[f32; 3]> = (0..w * h)
        .map(|i| {
            let j = ((y0 + i / w) * iw + x0 + i % w) * 4;
            [orig.px[j] as f32, orig.px[j + 1] as f32, orig.px[j + 2] as f32]
        })
        .collect();
    let mut tmp = buf.clone();
    let r = rad as isize;
    for _ in 0..3 {
        for (len, stride, lines, lstride) in [(w, 1, h, w), (h, w, w, 1)] {
            for l in 0..lines {
                let base = l * lstride;
                let at = |i: isize| base + (i.clamp(0, len as isize - 1) as usize) * stride;
                let mut acc = [0f32; 3];
                for i in -r..=r {
                    let p = buf[at(i)];
                    acc = [acc[0] + p[0], acc[1] + p[1], acc[2] + p[2]];
                }
                let n = (2 * r + 1) as f32;
                for i in 0..len as isize {
                    tmp[at(i)] = [acc[0] / n, acc[1] / n, acc[2] / n];
                    let (add, sub) = (buf[at(i + r + 1)], buf[at(i - r)]);
                    acc = [acc[0] + add[0] - sub[0], acc[1] + add[1] - sub[1], acc[2] + add[2] - sub[2]];
                }
            }
            std::mem::swap(&mut buf, &mut tmp);
        }
    }
    for (i, p) in buf.iter().enumerate() {
        let j = ((y0 + i / w) * iw + x0 + i % w) * 4;
        out.px[j] = p[0].round() as u8;
        out.px[j + 1] = p[1].round() as u8;
        out.px[j + 2] = p[2].round() as u8;
        out.px[j + 3] = 255;
    }
}

/// Most common colour on the rectangle's border (the "background" to erase with).
fn border_color(img: &Rgba, (x0, y0, x1, y1): (usize, usize, usize, usize)) -> u32 {
    let w = img.w as usize;
    let mut counts = std::collections::HashMap::<u32, u32>::new();
    let mut add = |x: usize, y: usize| {
        let i = (y * w + x) * 4;
        let c = (img.px[i] as u32) << 16 | (img.px[i + 1] as u32) << 8 | img.px[i + 2] as u32;
        *counts.entry(c).or_default() += 1;
    };
    for x in x0..x1 {
        add(x, y0);
        add(x, y1 - 1);
    }
    for y in y0..y1 {
        add(x0, y);
        add(x1 - 1, y);
    }
    counts.into_iter().max_by_key(|&(c, n)| (n, c)).map(|(c, _)| c).unwrap_or(0xFFFFFF)
}

// ---------------------------------------------------------------- text (GDI)

unsafe fn font(px: i32) -> HFONT {
    // Malgun Gothic covers Hangul and Latin; greyscale AA so the mask is a clean alpha.
    let face: Vec<u16> = "Malgun Gothic".encode_utf16().chain(Some(0)).collect();
    CreateFontW(-px, 0, 0, 0, 700, 0, 0, 0, DEFAULT_CHARSET as u32, 0, 0, ANTIALIASED_QUALITY as u32, 0, face.as_ptr())
}

fn lines(t: &str) -> Vec<u16> {
    t.replace('\n', "\r\n").encode_utf16().collect()
}

/// Pixel size of `t` (multi-line) at `px`.
pub fn text_size(t: &str, px: i32) -> (i32, i32) {
    unsafe {
        let dc = CreateCompatibleDC(null_mut());
        let f = font(px);
        let old = SelectObject(dc, f);
        let s = lines(if t.is_empty() { " " } else { t });
        let mut r: RECT = zeroed();
        DrawTextW(dc, s.as_ptr(), s.len() as i32, &mut r, DT_CALCRECT | DT_NOPREFIX);
        // An empty last line still takes a line (caret goes there).
        let extra = if t.ends_with('\n') { px * 13 / 10 } else { 0 };
        SelectObject(dc, old);
        DeleteObject(f);
        DeleteDC(dc);
        (r.right.max(px / 3), r.bottom + extra)
    }
}

/// Greyscale coverage mask (w, h, alpha) of `t` rendered at `px`.
pub fn text_mask(t: &str, px: i32) -> Option<(usize, usize, Vec<u8>)> {
    if t.is_empty() {
        return None;
    }
    let (w, h) = text_size(t, px);
    unsafe {
        let dc = CreateCompatibleDC(null_mut());
        let mut bi: BITMAPINFO = zeroed();
        bi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        bi.bmiHeader.biWidth = w;
        bi.bmiHeader.biHeight = -h;
        bi.bmiHeader.biPlanes = 1;
        bi.bmiHeader.biBitCount = 32;
        let mut bits = null_mut();
        let bmp = CreateDIBSection(dc, &bi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
        if bmp.is_null() {
            DeleteDC(dc);
            return None;
        }
        let old = SelectObject(dc, bmp);
        let f = font(px);
        let oldf = SelectObject(dc, f);
        SetBkMode(dc, TRANSPARENT as i32);
        SetTextColor(dc, 0xFFFFFF);
        let s = lines(t);
        let mut r = RECT { left: 0, top: 0, right: w, bottom: h };
        DrawTextW(dc, s.as_ptr(), s.len() as i32, &mut r, DT_NOPREFIX);
        GdiFlush();
        let px_slice = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize);
        let mask = px_slice.chunks_exact(4).map(|p| p[0].max(p[1]).max(p[2])).collect();
        SelectObject(dc, oldf);
        DeleteObject(f);
        SelectObject(dc, old);
        DeleteObject(bmp);
        DeleteDC(dc);
        Some((w as usize, h as usize, mask))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blank(w: u32, h: u32, c: u8) -> Rgba {
        Rgba { w, h, px: vec![c; (w * h * 4) as usize] }
    }
    fn at(img: &Rgba, x: u32, y: u32) -> [u8; 3] {
        let i = ((y * img.w + x) * 4) as usize;
        [img.px[i], img.px[i + 1], img.px[i + 2]]
    }
    fn obj(shape: Shape, a: (f32, f32), b: (f32, f32)) -> Obj {
        Obj { shape, a, b, color: 0xFF0000, size: 1 }
    }

    #[test]
    fn arrow_tip_and_shaft_are_coloured() {
        let base = blank(200, 100, 255);
        let out = render(&base, &[obj(Shape::Arrow, (10.0, 50.0), (190.0, 50.0))], Scale(1.0));
        assert_eq!(at(&out, 186, 50), [255, 0, 0]); // just inside the tip
        assert_eq!(at(&out, 60, 50), [255, 0, 0]); // shaft
        assert_eq!(at(&out, 60, 80), [255, 255, 255]); // untouched
        assert_eq!(at(&base, 60, 50), [255, 255, 255]); // base not modified
    }

    #[test]
    fn rect_is_outline_only() {
        let base = blank(100, 100, 255);
        let out = render(&base, &[obj(Shape::Rect, (20.0, 20.0), (80.0, 80.0))], Scale(1.0));
        assert_eq!(at(&out, 20, 50), [255, 0, 0]);
        assert_eq!(at(&out, 50, 50), [255, 255, 255]);
    }

    #[test]
    fn oval_outline() {
        let base = blank(100, 100, 255);
        let out = render(&base, &[obj(Shape::Oval, (10.0, 10.0), (90.0, 90.0))], Scale(1.0));
        assert_eq!(at(&out, 10, 50), [255, 0, 0]);
        assert_eq!(at(&out, 50, 50), [255, 255, 255]);
        assert_eq!(at(&out, 12, 12), [255, 255, 255]); // corner outside the ellipse
    }

    #[test]
    fn blur_changes_detail_and_samples_original() {
        let mut base = blank(64, 64, 255);
        for y in 0..64 {
            for x in (0..64).step_by(2) {
                let i = ((y * 64 + x) * 4) as usize;
                base.px[i..i + 3].copy_from_slice(&[0, 0, 0]);
            }
        }
        // A red line under the blur must not bleed in: blur samples the original.
        let objs = [obj(Shape::Line, (0.0, 32.0), (64.0, 32.0)), obj(Shape::Blur, (8.0, 8.0), (56.0, 56.0))];
        let out = render(&base, &objs, Scale(1.0));
        let p = at(&out, 32, 32);
        assert!(p[0] > 90 && p[0] < 170 && (p[0] as i32 - p[1] as i32).abs() < 3, "{p:?}");
        assert_eq!(at(&out, 2, 2), at(&base, 2, 2));
    }

    #[test]
    fn erase_uses_border_colour() {
        let mut base = blank(50, 50, 200);
        let i = ((25 * 50 + 25) * 4) as usize;
        base.px[i..i + 3].copy_from_slice(&[0, 0, 0]);
        let out = render(&base, &[obj(Shape::Erase, (10.0, 10.0), (40.0, 40.0))], Scale(1.0));
        assert_eq!(at(&out, 25, 25), [200, 200, 200]);
    }

    #[test]
    fn highlight_multiplies() {
        let base = blank(20, 20, 255);
        let mut o = obj(Shape::Highlight, (0.0, 0.0), (20.0, 20.0));
        o.color = 0xFFE600;
        let out = render(&base, &[o], Scale(1.0));
        assert_eq!(at(&out, 10, 10), [255, 230, 0]);
    }

    #[test]
    fn counter_and_text_draw_something() {
        let base = blank(120, 80, 255);
        let mut t = obj(Shape::Text("가A".into()), (60.0, 10.0), (60.0, 10.0));
        t.color = 0x0000FF;
        let objs = [obj(Shape::Counter(1), (30.0, 40.0), (30.0, 40.0)), t];
        let out = render(&base, &objs, Scale(1.0));
        assert_eq!(at(&out, 30, 30), [255, 0, 0]); // disc
        let white_in_disc = (24..36).any(|y| (24..36).any(|x| at(&out, x, y) == [255, 255, 255]));
        assert!(white_in_disc, "number drawn in white");
        let blue = (10..50).any(|y| (60..120).any(|x| { let p = at(&out, x, y); p[2] > 200 && p[0] < 60 }));
        assert!(blue, "text box drawn");
    }

    #[test]
    fn text_is_a_speech_bubble() {
        let base = blank(300, 200, 255);
        let s = Scale(1.0);
        let mut t = obj(Shape::Text("Hello".into()), (150.0, 40.0), (150.0, 40.0));
        t.color = 0xE5383B;
        let bb = t.bubble(s).unwrap();
        assert!(bb.tail.is_none(), "b inside the box: no tail");
        // Box filled with the colour, glyphs in white.
        let out = render(&base, &[t.clone()], s);
        assert_eq!(at(&out, (bb.x0 + 3.0) as u32, ((bb.y0 + bb.y1) / 2.0) as u32), [0xE5, 0x38, 0x3B]);
        let white = ((bb.y0 as u32 + 2)..(bb.y1 as u32 - 2)).any(|y| ((bb.x0 as u32 + 4)..(bb.x1 as u32 - 4)).any(|x| at(&out, x, y) == [255, 255, 255]));
        assert!(white, "contrasting text");
        // Rounded corner stays background.
        assert_eq!(at(&out, bb.x0 as u32, bb.y0 as u32), [255, 255, 255]);
        // Tail toward a point below-left: drawn down to the tip, from the bottom side.
        t.b = (60.0, 180.0);
        let bb = t.bubble(s).unwrap();
        let tri = bb.tail.expect("tail");
        assert!((tri[0].1 - (bb.y1 - 1.0)).abs() < 0.01 && tri[2] == (60.0, 180.0));
        let out = render(&base, &[t.clone()], s);
        // A point on the tail's centre line, 80 % of the way to the tip.
        let (mx, my) = ((tri[0].0 + tri[1].0) / 2.0, (tri[0].1 + tri[1].1) / 2.0);
        let (qx, qy) = (mx + (60.0 - mx) * 0.8, my + (180.0 - my) * 0.8);
        assert_eq!(at(&out, qx as u32, qy as u32), [0xE5, 0x38, 0x3B]);
        assert!(t.hit(s, qx, qy, 2.0) && !t.hit(s, 250.0, 180.0, 2.0));
        // Tail to the right picks the right side.
        t.b = (290.0, 50.0);
        let tri = t.bubble(s).unwrap().tail.unwrap();
        assert!((tri[0].0 - (t.bubble(s).unwrap().x1 - 1.0)).abs() < 0.01);
    }

    #[test]
    fn hit_and_translate() {
        let mut o = obj(Shape::Rect, (10.0, 10.0), (50.0, 50.0));
        assert!(o.hit(Scale(1.0), 10.0, 30.0, 2.0));
        assert!(!o.hit(Scale(1.0), 30.0, 30.0, 2.0));
        o.translate(5.0, 0.0);
        assert!(o.hit(Scale(1.0), 15.0, 30.0, 2.0));
    }
}

#[cfg(test)]
mod sample {
    use super::*;

    /// Writes a preview of every shape to $SHOTCLIP_ANNOT_OUT (manual visual check).
    #[test]
    #[ignore]
    fn render_sample() {
        let Ok(out) = std::env::var("SHOTCLIP_ANNOT_OUT") else { return };
        let (w, h) = (900u32, 520u32);
        let mut base = Rgba { w, h, px: Vec::with_capacity((w * h * 4) as usize) };
        for y in 0..h {
            for x in 0..w {
                let c = if (x / 40 + y / 40) % 2 == 0 { 245 } else { 225 };
                base.px.extend_from_slice(&[c, c, (c as u32 * (x + 200) / 1100) as u8 + 20, 255]);
            }
        }
        if let Some((mw, mh, m)) = text_mask("Secret token: abc-123-XYZ  비밀번호 1234", 22) {
            let mut cv = Canvas { img: &mut base };
            blit_mask(&mut cv, &m, mw, mh, 40, 420, 0x202020, 1.0);
            blit_mask(&mut cv, &m, mw, mh, 40, 470, 0x202020, 1.0);
        }
        let o = |shape, a, b, color, size| Obj { shape, a, b, color, size };
        let s = Scale(1.0);
        let objs = vec![
            o(Shape::Arrow, (60.0, 60.0), (300.0, 180.0), 0xE5383B, 0),
            o(Shape::Arrow, (60.0, 120.0), (300.0, 260.0), 0xE5383B, 1),
            o(Shape::Arrow, (60.0, 190.0), (300.0, 340.0), 0xE5383B, 2),
            o(Shape::Rect, (360.0, 40.0), (560.0, 160.0), 0x1E88E5, 1),
            o(Shape::Oval, (600.0, 40.0), (860.0, 160.0), 0x2DC653, 2),
            o(Shape::Line, (360.0, 200.0), (560.0, 260.0), 0x8E44AD, 1),
            o(Shape::Pen(vec![(600.0, 220.0), (640.0, 200.0), (690.0, 260.0), (740.0, 210.0), (800.0, 250.0)]), (0.0, 0.0), (0.0, 0.0), 0xF77F00, 1),
            o(Shape::Text("텍스트 Text\n두 번째 줄".into()), (360.0, 290.0), (330.0, 380.0), 0xE5383B, 1),
            o(Shape::Text("노란 말풍선".into()), (560.0, 400.0), (560.0, 400.0), 0xFFD60A, 2),
            o(Shape::Counter(1), (650.0, 320.0), (650.0, 320.0), 0xE5383B, 1),
            o(Shape::Counter(2), (710.0, 320.0), (710.0, 320.0), 0x1E88E5, 2),
            o(Shape::Counter(12), (770.0, 320.0), (770.0, 320.0), 0xFFD60A, 0),
            o(Shape::Highlight, (36.0, 415.0), (330.0, 448.0), 0xFFE14D, 1),
            o(Shape::Blur, (200.0, 462.0), (560.0, 500.0), 0, 1),
            o(Shape::Erase, (560.0, 412.0), (880.0, 450.0), 0, 1),
        ];
        let img = render(&base, &objs, s);
        std::fs::write(out, crate::optimize::encode_png(&img).unwrap()).unwrap();
    }
}
