//! Editor key dispatch, modelled on Shottr 1.10.0b (MainWindowController keyDown ->
//! FUN_1000296c0 and the MainMenu.nib key equivalents; table in parity/shottr_keys.md).
//! macOS Cmd = Ctrl here. Pure, so `shotclip.exe --probe` can check it against
//! parity/golden (rea-check).

use crate::annot::Tool;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Key {
    Char(char), // upper-case letters, digits and [ ] = -
    Tab,
    Esc,
    Enter,
    Delete,
    Backspace,
    Left,
    Right,
    Up,
    Down,
    Space,
    F2,
    Other,
}

impl Key {
    pub fn from_vk(vk: u32) -> Key {
        match vk {
            0x09 => Key::Tab,
            0x1B => Key::Esc,
            0x0D => Key::Enter,
            0x2E => Key::Delete,
            0x08 => Key::Backspace,
            0x25 => Key::Left,
            0x26 => Key::Up,
            0x27 => Key::Right,
            0x28 => Key::Down,
            0x20 => Key::Space,
            0x71 => Key::F2,
            0x30..=0x39 | 0x41..=0x5A => Key::Char(vk as u8 as char),
            0x60..=0x69 => Key::Char((b'0' + (vk - 0x60) as u8) as char), // numpad digits
            0xDB => Key::Char('['),
            0xDD => Key::Char(']'),
            0xBB | 0x6B => Key::Char('='),
            0xBD | 0x6D => Key::Char('-'),
            _ => Key::Other,
        }
    }

    pub fn from_name(s: &str) -> Key {
        match s {
            "Tab" => Key::Tab,
            "Esc" => Key::Esc,
            "Enter" => Key::Enter,
            "Delete" => Key::Delete,
            "Backspace" => Key::Backspace,
            "Left" => Key::Left,
            "Right" => Key::Right,
            "Up" => Key::Up,
            "Down" => Key::Down,
            "Space" => Key::Space,
            "F2" => Key::F2,
            s if s.chars().count() == 1 => Key::Char(s.chars().next().unwrap().to_ascii_uppercase()),
            _ => Key::Other,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

/// What the editor is doing when the key arrives.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Ctx {
    None,
    /// An area selection exists (Shottr's "cutter").
    Area,
    /// An object is selected.
    Object,
    /// A text box is being typed into.
    Text,
    /// An object is being drawn (mouse held).
    Drawing,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Act {
    Tool(Tool),
    /// Turn the area selection into an object of this kind (Shottr "imprint").
    AreaInto(Tool),
    AreaErase,
    AreaMove(i32, i32),
    /// Moves the right / bottom edge.
    AreaResize(i32, i32),
    /// Grows (negative: shrinks) the selection on all four sides.
    AreaGrow(i32),
    AreaAvgColor,
    AreaAutoAdjust,
    AreaCrop,
    AreaClear,
    AreaCopy,
    AreaOcr,
    ColorPixel,
    ColorDarkest,
    ObjNext,
    ObjDelete,
    ObjDuplicate,
    ObjSize(i8),
    ObjCopy,
    ObjPaste,
    /// ShotClip: arrows move the selected object (Shottr's per-object handler is unknown).
    ObjNudge(i32, i32),
    /// ShotClip: Esc with an object selected deselects it.
    ObjDeselect,
    CopyImage,
    Finish,
    DrawFinish,
    TextInput,
    TextCommit,
    Undo,
    Redo,
    Save,
    SaveAs,
    Ocr,
    Units,
    Pin,
    AddCapture,
    Upload,
    Rasterize,
    Backdrop,
    ZoomIn,
    ZoomOut,
    ZoomFit,
    Zoom100,
    ZoomSelection,
    CornerTopLeft,
    CornerBottomRight,
    /// Hold to measure; true = horizontal.
    Ruler(bool),
    Pan,
    Nothing,
}

fn tool_name(t: Tool) -> &'static str {
    match t {
        Tool::Select => "select",
        Tool::Arrow => "arrow",
        Tool::Rect => "rect",
        Tool::Oval => "oval",
        Tool::Line => "line",
        Tool::Pen => "pen",
        Tool::Text => "text",
        Tool::Highlight => "highlight",
        Tool::Blur => "blur",
        Tool::Erase => "erase",
        Tool::Counter => "counter",
        Tool::Spotlight => "spotlight",
        Tool::Magnifier => "magnifier",
    }
}

impl Act {
    /// Name shared with parity/original/shottr-keys.mjs.
    pub fn name(&self) -> String {
        match *self {
            Act::Tool(t) => format!("tool:{}", tool_name(t)),
            Act::AreaInto(t) => format!("area:{}", tool_name(t)),
            Act::AreaErase => "area:erase".into(),
            Act::AreaMove(x, y) => format!("area:move:{x},{y}"),
            Act::AreaResize(x, y) => format!("area:resize:{x},{y}"),
            Act::AreaGrow(n) => format!("area:grow:{n}"),
            Act::AreaAvgColor => "area:avgcolor".into(),
            Act::AreaAutoAdjust => "area:autoadjust".into(),
            Act::AreaCrop => "area:crop".into(),
            Act::AreaClear => "area:clear".into(),
            Act::AreaCopy => "area:copy".into(),
            Act::AreaOcr => "area:ocr".into(),
            Act::ColorPixel => "color:pixel".into(),
            Act::ColorDarkest => "color:darkest".into(),
            Act::ObjNext => "obj:next".into(),
            Act::ObjDelete => "obj:delete".into(),
            Act::ObjDuplicate => "obj:duplicate".into(),
            Act::ObjSize(n) => format!("obj:size:{n:+}"),
            Act::ObjCopy => "obj:copy".into(),
            Act::ObjPaste => "obj:paste".into(),
            Act::ObjNudge(x, y) => format!("obj:move:{x},{y}"),
            Act::ObjDeselect => "obj:deselect".into(),
            Act::CopyImage => "copy:image".into(),
            Act::Finish => "finish".into(),
            Act::DrawFinish => "draw:finish".into(),
            Act::TextInput => "text:input".into(),
            Act::TextCommit => "text:commit".into(),
            Act::Undo => "undo".into(),
            Act::Redo => "redo".into(),
            Act::Save => "save".into(),
            Act::SaveAs => "saveas".into(),
            Act::Ocr => "ocr".into(),
            Act::Units => "units".into(),
            Act::Pin => "pin".into(),
            Act::AddCapture => "addcapture".into(),
            Act::Upload => "upload".into(),
            Act::Rasterize => "rasterize".into(),
            Act::Backdrop => "backdrop".into(),
            Act::ZoomIn => "zoom:in".into(),
            Act::ZoomOut => "zoom:out".into(),
            Act::ZoomFit => "zoom:fit".into(),
            Act::Zoom100 => "zoom:100".into(),
            Act::ZoomSelection => "zoom:selection".into(),
            Act::CornerTopLeft => "corner:topleft".into(),
            Act::CornerBottomRight => "corner:bottomright".into(),
            Act::Ruler(_) => "ruler".into(),
            Act::Pan => "pan".into(),
            Act::Nothing => "unknown".into(),
        }
    }
}

pub fn action(k: Key, m: Mods, ctx: Ctx) -> Act {
    use Key::*;
    let c = |ch: char| k == Char(ch);
    let step = if m.shift { 10 } else { 1 };

    if ctx == Ctx::Text {
        return match k {
            Enter if !m.shift => Act::TextCommit,
            Esc => Act::TextCommit,
            _ => Act::TextInput,
        };
    }
    if k == Esc && !m.ctrl {
        return match ctx {
            Ctx::Drawing => Act::DrawFinish,
            Ctx::Area => Act::AreaClear,
            Ctx::Object => Act::ObjDeselect,
            _ => Act::Finish,
        };
    }
    if c('C') && m.ctrl && !m.shift {
        return match ctx {
            Ctx::Area => Act::AreaCopy,
            Ctx::Object => Act::ObjCopy,
            _ => Act::CopyImage,
        };
    }
    if c('Z') && m.ctrl {
        return if m.shift { Act::Redo } else { Act::Undo };
    }
    if c('Y') && m.ctrl {
        return Act::Redo;
    }
    if k == Tab && !m.ctrl {
        if ctx == Ctx::Object {
            return Act::ObjNext;
        }
        return if m.shift { Act::ColorDarkest } else { Act::ColorPixel };
    }
    if c('I') && !m.ctrl {
        return if m.shift { Act::ColorDarkest } else { Act::ColorPixel };
    }
    if c('P') && !m.shift {
        return if m.ctrl { Act::Pin } else { Act::Units };
    }
    if c('A') && m.ctrl && !m.shift {
        return Act::AddCapture;
    }
    if k == F2 || (c('E') && m.ctrl && !m.shift) {
        return Act::Upload;
    }
    if c('S') && m.ctrl {
        return if m.shift { Act::SaveAs } else { Act::Save };
    }
    if c('R') && m.ctrl && m.shift {
        return Act::Rasterize;
    }
    if k == Space {
        return Act::Pan;
    }

    if ctx != Ctx::Area {
        if !m.shift && !m.ctrl {
            let tool = match k {
                Char('A') => Some(Tool::Arrow),
                Char('R') => Some(Tool::Rect),
                Char('O') => Some(Tool::Oval),
                Char('L') => Some(Tool::Line),
                Char('T') => Some(Tool::Text),
                Char('B') => Some(Tool::Blur),
                Char('C') | Char('N') => Some(Tool::Counter),
                Char('S') => Some(Tool::Spotlight),
                Char('D') => Some(Tool::Pen),
                Char('H') => Some(Tool::Highlight),
                Char('V') => Some(Tool::Select),
                Char('M') => Some(Tool::Magnifier),
                Char('E') => Some(Tool::Erase), // ShotClip extra (Shottr: none)
                _ => None,
            };
            if let Some(t) = tool {
                return Act::Tool(t);
            }
            match k {
                Char('K') => return Act::Backdrop,
                Char('Q') => return Act::CornerTopLeft,
                Char('W') => return Act::CornerBottomRight,
                _ => {}
            }
        }
    } else {
        if c('B') {
            return Act::AreaInto(Tool::Blur);
        }
        if k == Delete || k == Backspace {
            return Act::AreaErase;
        }
        if !m.shift && !m.ctrl && !m.alt {
            match k {
                // ShotClip: Q / W keep working once they made a selection (Shottr: menu keys).
                Char('Q') => return Act::CornerTopLeft,
                Char('W') => return Act::CornerBottomRight,
                Char('R') => return Act::AreaInto(Tool::Rect),
                Char('O') => return Act::AreaInto(Tool::Oval),
                Char('S') => return Act::AreaInto(Tool::Spotlight),
                Char('M') => return Act::AreaInto(Tool::Magnifier),
                _ => {}
            }
        }
        let dir = match k {
            Left => Some((-step, 0)),
            Right => Some((step, 0)),
            Up => Some((0, -step)),
            Down => Some((0, step)),
            _ => None,
        };
        if let Some((dx, dy)) = dir {
            return if m.ctrl { Act::AreaResize(dx, dy) } else { Act::AreaMove(dx, dy) };
        }
        match k {
            Char(']') | Char('9') => return Act::AreaGrow(step),
            Char('[') | Char('8') => return Act::AreaGrow(-step),
            Char('C') if !m.ctrl => return Act::AreaAvgColor,
            Char('A') if !m.ctrl => return Act::AreaAutoAdjust,
            Enter => return Act::AreaCrop,
            Char('O') if m.ctrl => return Act::AreaOcr,
            _ => {}
        }
    }
    if ctx == Ctx::Object {
        match k {
            Delete | Backspace => return Act::ObjDelete,
            Char('D') if m.ctrl => return Act::ObjDuplicate,
            Char(']') if m.ctrl => return Act::ObjSize(1),
            Char('[') if m.ctrl => return Act::ObjSize(-1),
            Left => return Act::ObjNudge(-step, 0),
            Right => return Act::ObjNudge(step, 0),
            Up => return Act::ObjNudge(0, -step),
            Down => return Act::ObjNudge(0, step),
            _ => {}
        }
    }
    if c('V') && m.ctrl && !m.shift {
        return Act::ObjPaste;
    }
    if c('O') && m.ctrl && ctx != Ctx::Area {
        return Act::Ocr;
    }
    if matches!(ctx, Ctx::None | Ctx::Drawing) && !m.ctrl {
        match k {
            Left | Right | Char('2') => return Act::Ruler(true),
            Up | Down | Char('1') => return Act::Ruler(false),
            _ => {}
        }
    }
    if m.ctrl && !m.shift {
        match k {
            Char('=') => return Act::ZoomIn,
            Char('-') => return Act::ZoomOut,
            Char('1') => return Act::ZoomFit,
            Char('0') => return Act::Zoom100,
            Char('2') => return Act::ZoomSelection,
            _ => {}
        }
    }
    if !m.ctrl {
        match k {
            Char('=') => return Act::ZoomIn, // Shottr: Shift+=; plain = kept from ShotClip
            Char('-') => return Act::ZoomOut,
            _ => {}
        }
    }
    if k == Enter && ctx == Ctx::None {
        return Act::Finish; // ShotClip: Enter = done (Shottr: not established)
    }
    Act::Nothing
}

/// `shotclip.exe --probe`: stdin `{"key","shift","ctrl","alt","ctx"}` -> stdout `{"action"}`.
pub fn probe(input: &str) -> String {
    if let Some(op) = str_field(input, "op") {
        return probe_op(input, &op);
    }
    format!("{{\"action\":\"{}\"}}", action_of(input, Ctx::None).name())
}

fn str_field(input: &str, name: &str) -> Option<String> {
    let i = input.find(&format!("\"{name}\""))? + name.len() + 2;
    let rest = input[i..].trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
    Some(rest[..rest.find('"')?].to_string())
}

fn action_of(input: &str, default: Ctx) -> Act {
    let field = |name: &str| -> Option<String> {
        let i = input.find(&format!("\"{name}\""))? + name.len() + 2;
        let rest = input[i..].trim_start().strip_prefix(':')?.trim_start();
        if let Some(r) = rest.strip_prefix('"') {
            // JSON string (only \" and \\ escapes are expected in key names)
            let mut out = String::new();
            let mut it = r.chars();
            while let Some(ch) = it.next() {
                match ch {
                    '"' => return Some(out),
                    '\\' => out.push(it.next()?),
                    c => out.push(c),
                }
            }
            None
        } else {
            Some(rest.chars().take_while(|c| c.is_alphanumeric()).collect())
        }
    };
    let flag = |name: &str| field(name).as_deref() == Some("true");
    let key = Key::from_name(&field("key").unwrap_or_default());
    let ctx = match field("ctx").as_deref() {
        Some("area") => Ctx::Area,
        Some("object") => Ctx::Object,
        Some("text") => Ctx::Text,
        Some("drawing") => Ctx::Drawing,
        Some("none") => Ctx::None,
        _ => default,
    };
    let m = Mods { shift: flag("shift"), ctrl: flag("ctrl"), alt: flag("alt") };
    action(key, m, ctx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_parses_and_dispatches() {
        assert_eq!(probe(r#"{"key":"R","ctx":"area"}"#), r#"{"action":"area:rect"}"#);
        assert_eq!(probe(r#"{"key": "Down", "shift": true, "ctrl": true, "ctx": "area"}"#), r#"{"action":"area:resize:0,10"}"#);
        assert_eq!(probe(r#"{"key":"]","ctrl":true,"ctx":"object"}"#), r#"{"action":"obj:size:+1"}"#);
        assert_eq!(probe(r#"{"key":"C","ctx":"none"}"#), r#"{"action":"tool:counter"}"#);
    }

    #[test]
    fn q_then_w_makes_a_selection() {
        let none = Mods { shift: false, ctrl: false, alt: false };
        assert_eq!(action(Key::Char('Q'), none, Ctx::None), Act::CornerTopLeft);
        // After Q there is an (empty) area: W must still set the other corner.
        assert_eq!(action(Key::Char('W'), none, Ctx::Area), Act::CornerBottomRight);
        assert_eq!(action(Key::Char('Q'), none, Ctx::Area), Act::CornerTopLeft);
    }
}

/// Applies an area-selection key (move / resize / grow) to `r` (x0, y0, x1, y1, normalised),
/// keeping it inside a `w`×`h` image and at least 2 px in size. `unit` = pixels per step.
pub fn area_apply(r: (f32, f32, f32, f32), act: Act, unit: f32, w: f32, h: f32) -> (f32, f32, f32, f32) {
    let (mut x0, mut y0, mut x1, mut y1) = (r.0.min(r.2), r.1.min(r.3), r.0.max(r.2), r.1.max(r.3));
    match act {
        Act::AreaMove(dx, dy) => {
            let (dx, dy) = (dx as f32 * unit, dy as f32 * unit);
            let dx = dx.clamp(-x0, w - x1);
            let dy = dy.clamp(-y0, h - y1);
            (x0, y0, x1, y1) = (x0 + dx, y0 + dy, x1 + dx, y1 + dy);
        }
        Act::AreaResize(dx, dy) => {
            x1 = (x1 + dx as f32 * unit).clamp(x0 + 2.0, w);
            y1 = (y1 + dy as f32 * unit).clamp(y0 + 2.0, h);
        }
        Act::AreaGrow(n) => {
            let d = n as f32 * unit;
            let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
            x0 = (x0 - d).min(cx - 1.0).max(0.0);
            y0 = (y0 - d).min(cy - 1.0).max(0.0);
            x1 = (x1 + d).max(cx + 1.0).min(w);
            y1 = (y1 + d).max(cy + 1.0).min(h);
        }
        _ => {}
    }
    (x0, y0, x1, y1)
}

#[cfg(test)]
mod area_tests {
    use super::*;

    #[test]
    fn area_keys_stay_inside_the_image() {
        let r = (10.0, 10.0, 50.0, 30.0);
        assert_eq!(area_apply(r, Act::AreaMove(-10, 0), 1.0, 100.0, 100.0), (0.0, 10.0, 40.0, 30.0));
        // Shift = 10 units, clamped at the left border without shrinking.
        assert_eq!(area_apply(r, Act::AreaMove(-1, 0), 20.0, 100.0, 100.0), (0.0, 10.0, 40.0, 30.0));
        // Ctrl+arrows move the right / bottom edge.
        assert_eq!(area_apply(r, Act::AreaResize(5, -3), 1.0, 100.0, 100.0), (10.0, 10.0, 55.0, 27.0));
        assert_eq!(area_apply(r, Act::AreaResize(-100, 0), 1.0, 100.0, 100.0), (10.0, 10.0, 12.0, 30.0));
        // Grow on all four sides, clipped at the image border.
        assert_eq!(area_apply(r, Act::AreaGrow(10), 1.0, 100.0, 100.0), (0.0, 0.0, 60.0, 40.0));
        // Shrinking through zero stops at 2 px around the centre.
        assert_eq!(area_apply(r, Act::AreaGrow(-50), 1.0, 100.0, 100.0), (29.0, 19.0, 31.0, 21.0));
        // pt units (scale 2): one step = 2 px.
        assert_eq!(area_apply(r, Act::AreaMove(1, 1), 2.0, 100.0, 100.0), (12.0, 12.0, 52.0, 32.0));
    }
}

/// Shift+Tab: the darkest pixel (RGB) of the 20×20 box starting 10 px up-left of (x, y)
/// (Shottr kh:742-853: offset −10, size 20). `px` is RGBA, `w`×`h`.
pub fn darkest(px: &[u8], w: usize, h: usize, x: usize, y: usize) -> [u8; 3] {
    let mut best = (u32::MAX, [0u8; 3]);
    for yy in y.saturating_sub(10)..(y + 10).min(h) {
        for xx in x.saturating_sub(10)..(x + 10).min(w) {
            let i = (yy * w + xx) * 4;
            let p = [px[i], px[i + 1], px[i + 2]];
            let l = p[0] as u32 * 299 + p[1] as u32 * 587 + p[2] as u32 * 114;
            if l < best.0 {
                best = (l, p);
            }
        }
    }
    best.1
}

/// Numbers of the JSON array value of `name` (flat; nested brackets are ignored).
fn nums(input: &str, name: &str) -> Vec<f32> {
    let Some(i) = input.find(&format!("\"{name}\"")) else { return Vec::new() };
    let rest = &input[i..];
    let (Some(a), Some(b)) = (rest.find('['), rest.find(']')) else { return Vec::new() };
    rest[a + 1..b].split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-')).filter(|t| !t.is_empty()).filter_map(|t| t.parse().ok()).collect()
}

/// `--probe` operations other than key dispatch (parity/original/shottr-ops.mjs).
pub fn probe_op(input: &str, op: &str) -> String {
    let num = |name: &str| -> f32 {
        let Some(i) = input.find(&format!("\"{name}\"")) else { return 0.0 };
        let rest = input[i + name.len() + 2..].trim_start().trim_start_matches(':').trim_start();
        rest[..rest.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-')).unwrap_or(rest.len())].parse().unwrap_or(0.0)
    };
    match op {
        // {"op":"area","rect":[x0,y0,x1,y1],"w","h", key/shift/ctrl} -> {"rect":[...]}
        "area" => {
            let r = nums(input, "rect");
            let act = action_of(input, Ctx::Area);
            let out = area_apply((r[0], r[1], r[2], r[3]), act, 1.0, num("w"), num("h"));
            format!("{{\"rect\":[{},{},{},{}]}}", out.0, out.1, out.2, out.3)
        }
        // {"op":"darkest","w","h","x","y","dark":[x,y,r,g,b,...]} on a white image -> {"color":"#RRGGBB"}
        "darkest" => {
            let (w, h) = (num("w") as usize, num("h") as usize);
            let mut px = vec![255u8; w * h * 4];
            for d in nums(input, "dark").chunks_exact(5) {
                let i = (d[1] as usize * w + d[0] as usize) * 4;
                px[i..i + 3].copy_from_slice(&[d[2] as u8, d[3] as u8, d[4] as u8]);
            }
            let c = darkest(&px, w, h, num("x") as usize, num("y") as usize);
            format!("{{\"color\":\"#{:02X}{:02X}{:02X}\"}}", c[0], c[1], c[2])
        }
        _ => "{\"error\":\"unknown op\"}".into(),
    }
}
