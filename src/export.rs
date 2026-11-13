//! `--export-sheet out.bmp [--scale N]`: every animation as one labelled row
//! of composed steps, for reviewing art without running the pet. The last
//! cell of each row repeats step 0 on a dark background. Pure (returns BMP
//! bytes); BMP because it needs no compression code.

use crate::anim::Look;
use crate::compose::{CANVAS_H, CANVAS_W, Canvas};
use crate::sprite::{Frame, Sheet};

const LIGHT: [u8; 3] = [0xF2, 0xEF, 0xE8];
const DARK: [u8; 3] = [0x26, 0x29, 0x30];
const PAGE: [u8; 3] = [0xFF, 0xFF, 0xFF];
const TEXT: [u8; 3] = [0x30, 0x30, 0x30];
const LABEL_CHARS: usize = 26;

/// 3×5 font: each row is 3 bits (4 = left pixel).
fn glyph(c: char) -> [u8; 5] {
    match c.to_ascii_lowercase() {
        'a' => [2, 5, 7, 5, 5],
        'b' => [6, 5, 6, 5, 6],
        'c' => [3, 4, 4, 4, 3],
        'd' => [6, 5, 5, 5, 6],
        'e' => [7, 4, 6, 4, 7],
        'f' => [7, 4, 6, 4, 4],
        'g' => [3, 4, 5, 5, 3],
        'h' => [5, 5, 7, 5, 5],
        'i' => [7, 2, 2, 2, 7],
        'j' => [1, 1, 1, 5, 2],
        'k' => [5, 5, 6, 5, 5],
        'l' => [4, 4, 4, 4, 7],
        'm' => [5, 7, 7, 5, 5],
        'n' => [6, 5, 5, 5, 5],
        'o' => [2, 5, 5, 5, 2],
        'p' => [6, 5, 6, 4, 4],
        'q' => [2, 5, 5, 6, 3],
        'r' => [6, 5, 6, 5, 5],
        's' => [3, 4, 2, 1, 6],
        't' => [7, 2, 2, 2, 2],
        'u' => [5, 5, 5, 5, 7],
        'v' => [5, 5, 5, 5, 2],
        'w' => [5, 5, 7, 7, 5],
        'x' => [5, 5, 2, 5, 5],
        'y' => [5, 5, 2, 2, 2],
        'z' => [7, 1, 2, 4, 7],
        '0' => [7, 5, 5, 5, 7],
        '1' => [2, 6, 2, 2, 7],
        '2' => [6, 1, 2, 4, 7],
        '3' => [6, 1, 2, 1, 6],
        '4' => [5, 5, 7, 1, 1],
        '5' => [7, 4, 6, 1, 6],
        '6' => [3, 4, 7, 5, 7],
        '7' => [7, 1, 2, 2, 2],
        '8' => [7, 5, 7, 5, 7],
        '9' => [7, 5, 7, 1, 6],
        '_' => [0, 0, 0, 0, 7],
        '=' => [0, 7, 0, 7, 0],
        '-' => [0, 0, 7, 0, 0],
        _ => [0; 5],
    }
}

struct Image {
    w: usize,
    h: usize,
    rgb: Vec<[u8; 3]>,
}

impl Image {
    fn fill(&mut self, x: usize, y: usize, w: usize, h: usize, c: [u8; 3]) {
        for yy in y..(y + h).min(self.h) {
            for xx in x..(x + w).min(self.w) {
                self.rgb[yy * self.w + xx] = c;
            }
        }
    }

    fn text(&mut self, x: usize, y: usize, k: usize, s: &str) {
        for (i, ch) in s.chars().enumerate() {
            for (row, bits) in glyph(ch).iter().enumerate() {
                for col in 0..3 {
                    if bits & (4 >> col) != 0 {
                        self.fill(x + (i * 4 + col) * k, y + row * k, k, k, TEXT);
                    }
                }
            }
        }
    }

    fn canvas(&mut self, x: usize, y: usize, scale: usize, c: &Canvas, bg: [u8; 3]) {
        self.fill(x, y, c.w * scale, c.h * scale, bg);
        for cy in 0..c.h {
            for cx in 0..c.w {
                let [r, g, b, a] = c.px[cy * c.w + cx];
                if a != 0 {
                    self.fill(x + cx * scale, y + cy * scale, scale, scale, [r, g, b]);
                }
            }
        }
    }

    fn bmp(&self) -> Vec<u8> {
        let stride = (self.w * 3).div_ceil(4) * 4;
        let size = 54 + stride * self.h;
        let mut out = Vec::with_capacity(size);
        out.extend_from_slice(b"BM");
        out.extend_from_slice(&(size as u32).to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&54u32.to_le_bytes());
        out.extend_from_slice(&40u32.to_le_bytes());
        out.extend_from_slice(&(self.w as i32).to_le_bytes());
        out.extend_from_slice(&(self.h as i32).to_le_bytes()); // positive = bottom-up
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&24u16.to_le_bytes());
        out.extend_from_slice(&[0; 24]); // BI_RGB, sizes and resolution unused
        for y in (0..self.h).rev() {
            for x in 0..self.w {
                let [r, g, b] = self.rgb[y * self.w + x];
                out.extend_from_slice(&[b, g, r]);
            }
            out.resize(out.len() + stride - self.w * 3, 0);
        }
        out
    }
}

pub fn sheet_bmp(sheet: &Sheet, scale: usize) -> Vec<u8> {
    let scale = scale.clamp(1, 16);
    let k = scale.div_ceil(2);
    let gap = scale * 2;
    let (cw, ch) = (CANVAS_W * scale, CANVAS_H * scale);
    let label_w = LABEL_CHARS * 4 * k + gap;
    let cols = sheet.anims.iter().map(|a| a.steps.len() + 1).max().unwrap_or(1);
    let w = gap + label_w + cols * (cw + gap);
    let hats: Vec<usize> = (0..sheet.frames.len()).filter(|&i| sheet.frames[i].name.starts_with("hat_")).collect();
    let h = gap + (sheet.anims.len() + hats.len() + 1) * (ch + gap);
    let mut img = Image { w, h, rgb: vec![PAGE; w * h] };
    let mini = sheet.anim("mini").map(|m| m.steps[0].clone());
    let mut c = Canvas::new(CANVAS_W, CANVAS_H);
    for (row, a) in sheet.anims.iter().enumerate() {
        let y = gap + row * (ch + gap);
        let kind = if a.looping { "loop" } else { "once" };
        img.text(gap, y + ch / 2 - 6 * k, k, a.name);
        img.text(gap, y + ch / 2 + 2 * k, k, &format!("{}fps {kind} {}ms", a.fps, a.duration_ms()));
        let look = |i: usize| Look {
            step: a.steps[i].clone(),
            mini: if a.name == "delegating" { mini.clone() } else { None },
            crew: if a.name == "delegating" { 3 } else { 0 },
            badge: if a.name == "attention" { 2 } else { 0 },
            flip: a.steps[i].flip,
            rot: a.steps[i].rot,
            hat: None,
        };
        for i in 0..a.steps.len() {
            c.draw(sheet, &look(i));
            img.canvas(gap + label_w + i * (cw + gap), y, scale, &c, LIGHT);
        }
        c.draw(sheet, &look(0));
        img.canvas(gap + label_w + a.steps.len() * (cw + gap), y, scale, &c, DARK);
    }
    // What's added at draw time rather than by an animation: each hat on a
    // few differently shaped poses, then the morning mug.
    let plain = |step: &crate::sprite::Step, hat| Look { step: step.clone(), mini: None, crew: 0, badge: 0, flip: false, rot: 0, hat };
    let row_y = |row: usize| gap + (sheet.anims.len() + row) * (ch + gap);
    for (row, &hat) in hats.iter().enumerate() {
        let y = row_y(row);
        img.text(gap, y + ch / 2 - 3 * k, k, sheet.frames[hat].name);
        let poses = [("idle", 0), ("attention", 0), ("asleep", 0), ("compacting", 2)];
        for (col, step) in poses.iter().filter_map(|&(a, i)| sheet.anim(a)?.steps.get(i)).enumerate() {
            c.draw(sheet, &plain(step, Some(hat)));
            img.canvas(gap + label_w + col * (cw + gap), y, scale, &c, LIGHT);
        }
    }
    let y = row_y(hats.len());
    img.text(gap, y + ch / 2 - 3 * k, k, "morning mug");
    if let Some(idle) = sheet.anim("idle") {
        for (col, (step, mug)) in idle.steps.iter().zip(["mug_0", "mug_1"]).enumerate() {
            let mut step = step.clone();
            step.fx.extend(sheet.index(mug));
            c.draw(sheet, &plain(&step, None));
            img.canvas(gap + label_w + col * (cw + gap), y, scale, &c, LIGHT);
        }
    }
    img.bmp()
}

/// One icon image: `f` scaled by `k` and centred on a `size`-px square, as
/// the bottom-up BGRA DIB (height doubled, empty AND mask) an `.ico` holds.
fn ico_image(f: &Frame, k: usize, size: usize) -> Vec<u8> {
    let logical = size / k;
    let mut c = Canvas::new(logical, logical);
    c.blit(f, (logical as i32 - f.w as i32) / 2, (logical as i32 - f.h as i32) / 2, false);
    let mut top_down = vec![0u8; size * size * 4];
    c.to_bgra_premul(k, &mut top_down); // alpha is 0 or 255: same as straight alpha
    let mask = size.div_ceil(32) * 4 * size;
    let mut out = Vec::with_capacity(40 + top_down.len() + mask);
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(size as i32).to_le_bytes());
    out.extend_from_slice(&(size as i32 * 2).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&[0; 24]); // BI_RGB, sizes and resolution unused
    for row in top_down.chunks_exact(size * 4).rev() {
        out.extend_from_slice(row);
    }
    out.resize(out.len() + mask, 0);
    out
}

/// `--export-ico`: the exe's icon. Integer scales only, so it stays crisp:
/// the 16-px tray art where the 2x body doesn't fit a whole number of times.
pub fn ico(sheet: &Sheet) -> Vec<u8> {
    let mut canvas = Canvas::new(CANVAS_W, CANVAS_H);
    if let Some(step) = sheet.anim("idle").map(|a| a.steps[0].clone()) {
        canvas.draw(sheet, &Look { step, mini: None, crew: 0, badge: 0, flip: false, rot: 0, hat: None });
    }
    let body = canvas.to_frame("body");
    let small = sheet.frame("icon").unwrap_or(&body);
    let images = [ico_image(small, 1, 16), ico_image(&body, 1, 32), ico_image(small, 3, 48), ico_image(&body, 2, 64)];
    const SIZES: [u8; 4] = [16, 32, 48, 64];

    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]); // reserved, type = icon
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len();
    for (image, size) in images.iter().zip(SIZES) {
        out.extend_from_slice(&[size, size, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(image.len() as u32).to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += image.len();
    }
    for image in &images {
        out.extend_from_slice(image);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_a_valid_ico() {
        let b = ico(&crate::sprite::builtin());
        assert_eq!(b[..4], [0, 0, 1, 0]);
        let count = u16::from_le_bytes([b[4], b[5]]) as usize;
        assert_eq!(count, 4);
        let u32_at = |at: usize| u32::from_le_bytes(b[at..at + 4].try_into().unwrap()) as usize;
        let mut end = 6 + 16 * count;
        for i in 0..count {
            let entry = 6 + 16 * i;
            let (size, bytes, offset) = (b[entry] as usize, u32_at(entry + 8), u32_at(entry + 12));
            assert_eq!(offset, end, "images are contiguous");
            assert_eq!(bytes, 40 + size * size * 4 + size.div_ceil(32) * 4 * size);
            assert_eq!((u32_at(offset + 4), u32_at(offset + 8)), (size, size * 2));
            let pixels = &b[offset + 40..offset + 40 + size * size * 4];
            assert_eq!(pixels[3], 0, "{size}: the corner is transparent");
            assert_eq!(pixels[size * (size / 2) * 4 + size * 2 + 3], 255, "{size}: the middle is Clawd");
            end += bytes;
        }
        assert_eq!(end, b.len());
    }

    #[test]
    fn writes_a_valid_bmp() {
        let s = crate::sprite::builtin();
        let b = sheet_bmp(&s, 2);
        assert_eq!(&b[..2], b"BM");
        assert_eq!(u32::from_le_bytes(b[2..6].try_into().unwrap()) as usize, b.len());
        let w = i32::from_le_bytes(b[18..22].try_into().unwrap()) as usize;
        let h = i32::from_le_bytes(b[22..26].try_into().unwrap()) as usize;
        assert_eq!(b.len(), 54 + (w * 3).div_ceil(4) * 4 * h);
    }
}
