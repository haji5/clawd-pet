//! Compositing frames onto the logical canvas and scaling it to the window
//! buffer. Pure; the Windows renderer only copies the result into a DIB.

use crate::anim::Look;
use crate::sprite::{Frame, Rgba, Sheet};

/// Logical canvas (2x art: the body is 32 px wide): the body sits
/// bottom-centre, with room for a bubble above-right and props on either
/// side (PLAN.md §4).
pub const CANVAS_W: usize = 64;
pub const CANVAS_H: usize = 48;
/// Where a body frame's anchor (the point between its feet) lands.
pub const FEET: (i32, i32) = (32, 47);
/// Top-left of the speech bubble, and of the glyph inside it.
pub const BUBBLE_AT: (i32, i32) = (38, 2);
pub const GLYPH_IN_BUBBLE: (i32, i32) = (5, 3);
/// Top-left of the "sessions waiting" badge, and of its digit.
pub const BADGE_AT: (i32, i32) = (55, 0);
const DIGIT_IN_BADGE: (i32, i32) = (2, 2);
/// Feet point of the mini-clawd (left of the body).
pub const MINI_FEET: (i32, i32) = (10, 47);
/// Each extra mini-clawd stands on the head of the one below.
const MINI_STACK: i32 = 8;

/// A rectangle of canvas pixels (`x1` and `y1` exclusive).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Dirty {
    pub x0: usize,
    pub y0: usize,
    pub x1: usize,
    pub y1: usize,
}

/// Fully transparent and fully opaque pixels, nearly all of them, skip the
/// division.
fn premultiplied_bgra([r, g, b, a]: Rgba) -> [u8; 4] {
    match a {
        255 => [b, g, r, 255],
        0 => [0; 4],
        _ => {
            let pm = |c: u8| ((c as u16 * a as u16 + 127) / 255) as u8;
            [pm(b), pm(g), pm(r), a]
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct Canvas {
    pub w: usize,
    pub h: usize,
    pub px: Vec<Rgba>,
}

impl Canvas {
    pub fn new(w: usize, h: usize) -> Self {
        Canvas { w, h, px: vec![[0; 4]; w * h] }
    }

    pub fn clear(&mut self) {
        self.px.fill([0; 4]);
    }

    /// Draw `f` with its top-left at (x, y). Transparent pixels are skipped,
    /// anything off-canvas is clipped. `flip` mirrors horizontally.
    pub fn blit(&mut self, f: &Frame, x: i32, y: i32, flip: bool) {
        for fy in 0..f.h {
            let cy = y + fy as i32;
            if cy < 0 || cy >= self.h as i32 {
                continue;
            }
            for fx in 0..f.w {
                let src = f.at(if flip { f.w - 1 - fx } else { fx }, fy);
                let cx = x + fx as i32;
                if src[3] == 0 || cx < 0 || cx >= self.w as i32 {
                    continue;
                }
                self.px[cy as usize * self.w + cx as usize] = src;
            }
        }
    }

    /// Body frames are drawn with their anchor on the canvas feet point.
    /// The anchor's x is a pixel *boundary* (x=8 in a 16-wide frame is the
    /// centre line), so a symmetric frame doesn't shift when flipped.
    /// Draw `f` so its anchor (x = pixel boundary) lands on `at`.
    pub fn blit_anchored(&mut self, f: &Frame, at: (i32, i32), flip: bool) {
        let ax = if flip { f.w as i32 - f.anchor.0 } else { f.anchor.0 };
        self.blit(f, at.0 - ax, at.1 - f.anchor.1, flip);
    }

    /// Compose a whole look: mini-clawd, body, eyes, fx, bubble + glyph, badge
    /// (the layer order of PLAN.md §6b). Rotated looks draw the body layers
    /// into a scratch canvas and turn it in 90° steps (lossless).
    pub fn draw(&mut self, sheet: &Sheet, look: &Look) {
        self.clear();
        if let Some(m) = &look.mini {
            for i in 0..look.crew as i32 {
                self.blit_anchored(&sheet.frames[m.body], (MINI_FEET.0, MINI_FEET.1 + m.dy - i * MINI_STACK), false);
            }
        }
        if look.rot.is_multiple_of(4) {
            self.draw_body(sheet, look);
        } else {
            let mut body = Canvas::new(self.w, self.h);
            body.draw_body(sheet, look);
            self.rotate_from(&body, look.rot % 4, look.step.dy);
        }
        self.draw_bubble(sheet, look);
    }

    /// Copy `src` turned `rot` quarter turns clockwise around the body's
    /// centre. Sideways, the body is taller than wide, so it is lifted to
    /// keep its lowest point on the floor line.
    fn rotate_from(&mut self, src: &Canvas, rot: u8, dy: i32) {
        // Centre of the 32x22 body (pixel boundaries, so turns are exact).
        let (cx, cy) = (FEET.0, FEET.1 + 1 - 11 + dy);
        let cyd = if rot % 2 == 1 { FEET.1 + 1 - 16 + dy } else { cy };
        for y in 0..self.h as i32 {
            for x in 0..self.w as i32 {
                // Twice the offset from the centre to the pixel centre (odd numbers).
                let (dx2, dy2) = (2 * (x - cx) + 1, 2 * (y - cyd) + 1);
                let (sx2, sy2) = match rot {
                    1 => (dy2, -dx2),
                    2 => (-dx2, -dy2),
                    _ => (-dy2, dx2),
                };
                let (sx, sy) = (cx + (sx2 - 1) / 2, cy + (sy2 - 1) / 2);
                if sx >= 0 && sy >= 0 && (sx as usize) < src.w && (sy as usize) < src.h {
                    let p = src.px[sy as usize * src.w + sx as usize];
                    if p[3] != 0 {
                        self.px[y as usize * self.w + x as usize] = p;
                    }
                }
            }
        }
    }

    fn draw_body(&mut self, sheet: &Sheet, look: &Look) {
        let fr = |i: usize| &sheet.frames[i];
        let st = &look.step;
        let body = fr(st.body);
        let feet = (FEET.0, FEET.1 + st.dy);
        self.blit_anchored(body, feet, look.flip);
        if let Some(e) = st.eyes.map(fr) {
            let ax = if look.flip { body.w as i32 - body.anchor.0 } else { body.anchor.0 };
            let (left, top) = (feet.0 - ax, feet.1 - body.anchor.1);
            let gaze_x = if look.flip { -st.look.0 } else { st.look.0 };
            // Eye anchors (in the body and in the eye sprite) have x on a
            // pixel boundary, the eye's centre line, so mirroring is exact.
            for (i, &(ex, ey)) in body.eyes.iter().enumerate() {
                // The second eye is the first one mirrored (unless the eye
                // sprite has a shine that must stay on the lit side).
                let mirror = e.mirror && ((i % 2 == 1) != look.flip);
                let bx = if look.flip { body.w as i32 - ex } else { ex };
                let eax = if mirror { e.w as i32 - e.anchor.0 } else { e.anchor.0 };
                self.blit(e, left + bx - eax + gaze_x, top + ey - e.anchor.1 + st.look.1, mirror);
            }
        }
        if let Some(hat) = look.hat.map(fr) {
            // On the top of the head in every pose, however squashed: the
            // hat's anchor goes on the topmost body pixel of the centre column.
            let centre = (body.anchor.0.max(0) as usize).min(body.w - 1);
            let top = (0..body.h).find(|&y| body.at(centre, y)[3] != 0).unwrap_or(0) as i32;
            self.blit_anchored(hat, (feet.0, feet.1 - body.anchor.1 + top), look.flip);
        }
        for &f in &st.fx {
            self.blit_anchored(fr(f), feet, look.flip);
        }
    }

    fn draw_bubble(&mut self, sheet: &Sheet, look: &Look) {
        if let Some(g) = look.step.glyph.map(|i| &sheet.frames[i]) {
            if let Some(b) = sheet.frame("bubble") {
                self.blit(b, BUBBLE_AT.0, BUBBLE_AT.1, false);
            }
            self.blit(g, BUBBLE_AT.0 + GLYPH_IN_BUBBLE.0, BUBBLE_AT.1 + GLYPH_IN_BUBBLE.1, false);
            if look.badge >= 2
                && let Some(b) = sheet.frame("badge")
            {
                self.blit(b, BADGE_AT.0, BADGE_AT.1, false);
                let n = if look.badge <= 9 { format!("num_{}", look.badge) } else { "num_more".into() };
                if let Some(d) = sheet.frame(&n) {
                    self.blit(d, BADGE_AT.0 + DIGIT_IN_BADGE.0, BADGE_AT.1 + DIGIT_IN_BADGE.1, false);
                }
            }
        }
    }

    /// The canvas cropped to its visible pixels, as a frame (tray icon, sheet export).
    pub fn to_frame(&self, name: &'static str) -> Frame {
        let opaque = |x: usize, y: usize| self.px[y * self.w + x][3] != 0;
        let xs: Vec<usize> = (0..self.w).filter(|&x| (0..self.h).any(|y| opaque(x, y))).collect();
        let ys: Vec<usize> = (0..self.h).filter(|&y| (0..self.w).any(|x| opaque(x, y))).collect();
        let (Some(&x0), Some(&x1), Some(&y0), Some(&y1)) = (xs.first(), xs.last(), ys.first(), ys.last()) else {
            return Frame::from_rgba(name, 1, 1, vec![[0; 4]]);
        };
        let px = (y0..=y1).flat_map(|y| (x0..=x1).map(move |x| (x, y))).map(|(x, y)| self.px[y * self.w + x]).collect();
        Frame::from_rgba(name, x1 - x0 + 1, y1 - y0 + 1, px)
    }

    pub fn whole(&self) -> Dirty {
        Dirty { x0: 0, y0: 0, x1: self.w, y1: self.h }
    }

    /// The smallest rectangle around every pixel that differs from `old`
    /// (same size); `None` if nothing does.
    pub fn changed_since(&self, old: &Canvas) -> Option<Dirty> {
        let mut dirty: Option<Dirty> = None;
        for y in 0..self.h {
            let range = y * self.w..(y + 1) * self.w;
            let (now, then) = (&self.px[range.clone()], &old.px[range]);
            if now == then {
                continue;
            }
            let first = (0..self.w).find(|&x| now[x] != then[x]).unwrap_or(0);
            let last = (first..self.w).rfind(|&x| now[x] != then[x]).unwrap_or(first);
            let d = dirty.get_or_insert(Dirty { x0: first, y0: y, x1: last + 1, y1: y + 1 });
            d.x0 = d.x0.min(first);
            d.x1 = d.x1.max(last + 1);
            d.y1 = y + 1;
        }
        dirty
    }

    /// Nearest-neighbour integer upscale into premultiplied BGRA, top-down,
    /// row stride = `w * scale * 4`. `out` must be exactly that size.
    pub fn to_bgra_premul(&self, scale: usize, out: &mut [u8]) {
        self.write_bgra(scale, self.whole(), out);
    }

    /// The same, for one rectangle of the canvas: the rest of `out` is left alone.
    pub fn write_bgra(&self, scale: usize, d: Dirty, out: &mut [u8]) {
        let stride = self.w * scale * 4;
        debug_assert_eq!(out.len(), stride * self.h * scale);
        let bytes = (d.x1 - d.x0) * scale * 4;
        for y in d.y0..d.y1 {
            // Build one scaled row, then copy it `scale` times.
            let start = y * scale * stride + d.x0 * scale * 4;
            let row = &self.px[y * self.w + d.x0..y * self.w + d.x1];
            for (block, &p) in out[start..start + bytes].chunks_exact_mut(scale * 4).zip(row) {
                let bgra = premultiplied_bgra(p);
                block.as_chunks_mut::<4>().0.fill(bgra);
            }
            for s in 1..scale {
                out.copy_within(start..start + bytes, start + s * stride);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: usize, h: usize, px: Vec<Rgba>) -> Frame {
        let mut f = Frame::from_rgba("t", w, h, px);
        f.anchor = (0, h as i32 - 1);
        f
    }
    const R: Rgba = [255, 0, 0, 255];
    const T: Rgba = [0, 0, 0, 0];

    #[test]
    fn changed_since_is_the_tight_box() {
        let old = Canvas::new(5, 4);
        assert_eq!(old.changed_since(&old), None);
        let mut new = old.clone();
        new.px[5 + 3] = R;
        new.px[2 * 5 + 1] = R;
        assert_eq!(new.changed_since(&old), Some(Dirty { x0: 1, y0: 1, x1: 4, y1: 3 }));
    }

    #[test]
    fn writing_a_rectangle_matches_a_full_write_there_and_leaves_the_rest() {
        let mut c = Canvas::new(4, 3);
        c.px[4 + 2] = R;
        let mut full = vec![0u8; 4 * 3 * 3 * 3 * 4];
        c.to_bgra_premul(3, &mut full);
        let mut part = vec![7u8; full.len()];
        c.write_bgra(3, Dirty { x0: 2, y0: 1, x1: 3, y1: 2 }, &mut part);
        let stride = 4 * 3 * 4;
        for y in 0..9 {
            for x in 0..12 {
                let i = y * stride + x * 4;
                let inside = (6..9).contains(&x) && (3..6).contains(&y);
                assert_eq!(&part[i..i + 4], if inside { &full[i..i + 4] } else { &[7u8, 7, 7, 7][..] }, "{x},{y}");
            }
        }
    }

    #[test]
    fn blit_clips_skips_transparent_and_flips() {
        let f = frame(2, 1, vec![R, T]);
        let mut c = Canvas::new(3, 1);
        c.px[1] = [0, 0, 255, 255];
        c.blit(&f, 0, 0, false);
        assert_eq!(c.px, vec![R, [0, 0, 255, 255], T]);
        let mut c = Canvas::new(3, 1);
        c.blit(&f, 1, 0, true);
        assert_eq!(c.px, vec![T, T, R]);
        let mut c = Canvas::new(1, 1);
        c.blit(&f, -1, 5, false); // fully off-canvas: no panic
        assert_eq!(c.px, vec![T]);
    }

    #[test]
    fn scale_2_is_pixel_exact_and_premultiplied() {
        let mut c = Canvas::new(2, 1);
        c.px = vec![[200, 100, 50, 255], [255, 255, 255, 128]];
        let mut out = vec![0u8; 2 * 2 * 2 * 4];
        c.to_bgra_premul(2, &mut out);
        let a = [50, 100, 200, 255];
        let b = [128, 128, 128, 128];
        let row: Vec<u8> = [a, a, b, b].concat();
        assert_eq!(out, [row.clone(), row].concat());
    }

    fn look(s: &Sheet, anim: &str, step: usize) -> Look {
        Look { step: s.anim(anim).unwrap().steps[step].clone(), mini: None, crew: 0, badge: 0, flip: false, rot: 0, hat: None }
    }
    fn at(c: &Canvas, x: usize, y: usize) -> Rgba {
        c.px[y * c.w + x]
    }
    const INK: Rgba = [0x1E, 0x17, 0x14, 255];

    #[test]
    fn eyes_land_on_their_anchors_and_mirror() {
        let s = crate::sprite::builtin();
        let mut c = Canvas::new(CANVAS_W, CANVAS_H);
        // idle step 0 = `stand`, eye centre lines x=11 and x=21 at row 7; body top-left = (16,26),
        // so the eyes cover x 26-27 and 36-37, rows 33-36.
        c.draw(&s, &look(&s, "idle", 0));
        const SHINE: Rgba = [255, 255, 255, 255];
        // The shine stays top-left on both eyes (mirror=no).
        assert_eq!((at(&c, 26, 33), at(&c, 27, 33)), (SHINE, INK));
        assert_eq!((at(&c, 36, 33), at(&c, 37, 33)), (SHINE, INK));
        for (x, y) in [(26, 36), (27, 36), (36, 36), (37, 36)] {
            assert_eq!(at(&c, x, y), INK, "eye pixel at {x},{y}");
        }
        // Squint (anchor 1,-1, 4x3): `>` on the left eye, `<` on the right one.
        c.draw(&s, &look(&s, "oops", 1));
        assert_eq!((at(&c, 25, 34), at(&c, 27, 35), at(&c, 25, 36)), (INK, INK, INK));
        assert_eq!((at(&c, 38, 34), at(&c, 36, 35), at(&c, 38, 36)), (INK, INK, INK));
        assert_ne!(at(&c, 34, 35), INK, "the right squint is centred on its eye");
        // Gaze shifts both eyes.
        let mut l = look(&s, "idle", 0);
        l.step.look = (2, 0);
        c.draw(&s, &l);
        assert_eq!((at(&c, 29, 36), at(&c, 39, 36)), (INK, INK));
        assert_ne!(at(&c, 26, 36), INK);
    }

    #[test]
    fn quarter_turns_are_lossless() {
        let s = crate::sprite::builtin();
        let mut l = look(&s, "idle", 0);
        let mut plain = Canvas::new(CANVAS_W, CANVAS_H);
        plain.draw(&s, &l);
        let count = |c: &Canvas| c.px.iter().filter(|p| p[3] != 0).count();
        let mut turned = Canvas::new(CANVAS_W, CANVAS_H);
        for rot in 1..4 {
            l.rot = rot;
            turned.draw(&s, &l);
            assert_eq!(count(&turned), count(&plain), "rot {rot} keeps every pixel");
            assert!(turned.px[45 * CANVAS_W..].iter().any(|p| p[3] != 0), "rot {rot} stays near the floor");
        }
        // 180° maps each pixel through the body centre.
        l.rot = 2;
        turned.draw(&s, &l);
        let (left, top) = (16, 26);
        assert_eq!(turned.px[(top + 21) * CANVAS_W + left + 31 - 10], plain.px[top * CANVAS_W + left + 10]);
    }

    #[test]
    fn bubble_and_badge() {
        let s = crate::sprite::builtin();
        let mut c = Canvas::new(CANVAS_W, CANVAS_H);
        let mut l = look(&s, "attention", 0);
        c.draw(&s, &l);
        assert_eq!(at(&c, BUBBLE_AT.0 as usize + 2, BUBBLE_AT.1 as usize), INK, "bubble outline");
        assert_eq!(at(&c, 61, 5)[3], 0, "no badge for one session");
        l.badge = 3;
        c.draw(&s, &l);
        assert_eq!(at(&c, 61, 5)[0], 0xE5, "badge drawn");
        let f = c.to_frame("x");
        assert!(f.w <= CANVAS_W && f.h <= CANVAS_H && (0..f.h).any(|y| (0..f.w).any(|x| f.at(x, y)[3] != 0)));
    }

    #[test]
    fn body_lands_on_feet_point() {
        let s = crate::sprite::builtin();
        let idle = s.frame("stand").unwrap();
        let mut c = Canvas::new(CANVAS_W, CANVAS_H);
        c.blit_anchored(idle, FEET, false);
        // Bottom row of the canvas holds the leg soles; above the body is empty.
        assert_ne!(c.px[47 * CANVAS_W + 21][3], 0);
        assert!(c.px[..26 * CANVAS_W].iter().all(|p| p[3] == 0));
        let mut flipped = Canvas::new(CANVAS_W, CANVAS_H);
        flipped.blit_anchored(idle, FEET, true);
        // The shading isn't symmetric, but the outline is: flipping mirrors
        // around the centre line without shifting.
        for y in 0..CANVAS_H {
            for x in 0..CANVAS_W {
                let (a, b) = (c.px[y * CANVAS_W + x][3], flipped.px[y * CANVAS_W + CANVAS_W - 1 - x][3]);
                assert_eq!(a, b, "alpha at {x},{y}");
            }
        }
    }
}
