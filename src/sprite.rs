//! Parser for `assets/palette.txt` and `assets/sprites.txt` (format in PLAN.md §6).
//! The parser is pure, so it is unit-tested and the same code serves `--art` hot reload.

use std::collections::HashMap;

/// Straight (non-premultiplied) RGBA.
pub type Rgba = [u8; 4];

#[derive(Debug, Clone)]
pub struct Palette {
    /// Indexed by ASCII char. `None` = not defined. Transparent = `Some([0;4])`.
    colors: [Option<Rgba>; 128],
}

impl Palette {
    pub fn color(&self, c: u8) -> Option<Rgba> {
        self.colors.get(c as usize).copied().flatten()
    }
}

/// A frame's pixels. The art is already one byte per pixel in the embedded
/// text, so frames from a sheet just point at it instead of copying it.
#[derive(Debug, Clone, PartialEq)]
enum Pixels {
    Art { rows: Box<[u32]>, text: &'static [u8], pal: &'static [Rgba; 128] },
    /// Row-major straight RGBA, alpha 0 = transparent.
    Rgba(Vec<Rgba>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub name: &'static str,
    pub w: usize,
    pub h: usize,
    px: Pixels,
    pub anchor: (i32, i32),
    pub eyes: Vec<(i32, i32)>,
    pub hold: u32,
    /// Eye sprites: drawn mirrored for the right eye (`mirror=no` keeps a
    /// shine on the lit side).
    pub mirror: bool,
}

impl Frame {
    /// A frame made at run time (the tray fallback, the sheet export, tests).
    pub fn from_rgba(name: &'static str, w: usize, h: usize, px: Vec<Rgba>) -> Frame {
        Frame { name, w, h, px: Pixels::Rgba(px), anchor: (0, 0), eyes: vec![], hold: 1, mirror: true }
    }

    pub fn at(&self, x: usize, y: usize) -> Rgba {
        match &self.px {
            Pixels::Art { rows, text, pal } => pal[text[rows[y] as usize + x] as usize],
            Pixels::Rgba(px) => px[y * self.w + x],
        }
    }
}

/// One step of an animation: a body frame plus the layers drawn with it
/// (PLAN.md §6b rig). All `usize`s index `Sheet::frames`.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub body: usize,
    /// Eye sprite drawn at the body's eye anchors (`None`: no eyes).
    pub eyes: Option<usize>,
    /// Gaze offset of the eyes, in body space.
    pub look: (i32, i32),
    /// Vertical offset of the body and its overlays (negative = up).
    pub dy: i32,
    /// Overlays (props, particles), anchored to the feet point like the body.
    pub fx: Vec<usize>,
    /// Glyph shown in the speech bubble.
    pub glyph: Option<usize>,
    /// Ticks (1 tick = 1000/fps ms) this step stays up.
    pub hold: u32,
    /// Quarter turns clockwise (tumbles, flips), and a horizontal mirror.
    pub rot: u8,
    pub flip: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Anim {
    pub name: &'static str,
    pub fps: u32,
    pub looping: bool,
    pub steps: Vec<Step>,
}

impl Anim {
    pub fn tick_ms(&self) -> u64 {
        (1000 / self.fps.max(1)) as u64
    }
    /// Length of one pass in ms.
    pub fn duration_ms(&self) -> u64 {
        self.steps.iter().map(|s| s.hold as u64).sum::<u64>() * self.tick_ms()
    }
}

#[derive(Debug, Clone)]
pub struct Sheet {
    pub frames: Vec<Frame>,
    pub anims: Vec<Anim>,
    by_name: HashMap<&'static str, usize>,
}

impl Sheet {
    pub fn frame(&self, name: &str) -> Option<&Frame> {
        self.by_name.get(name).map(|&i| &self.frames[i])
    }
    pub fn index(&self, name: &str) -> Option<usize> {
        self.by_name.get(name).copied()
    }
    pub fn anim(&self, name: &str) -> Option<&Anim> {
        self.anims.iter().find(|a| a.name == name)
    }
}

#[derive(Debug, PartialEq)]
pub struct ParseError {
    pub file: &'static str,
    pub line: usize,
    pub msg: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}: {}", self.file, self.line, self.msg)
    }
}

fn err(file: &'static str, line: usize, msg: impl Into<String>) -> ParseError {
    ParseError { file, line, msg: msg.into() }
}

fn content_lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.lines().enumerate().map(|(i, l)| (i + 1, l.trim_end())).filter(|(_, l)| !l.trim_start().starts_with("//"))
}

pub fn parse_palette(text: &str) -> Result<Palette, ParseError> {
    let mut colors = [None; 128];
    for (ln, line) in content_lines(text) {
        let mut parts = line.split_whitespace();
        let (Some(ch), Some(val)) = (parts.next(), parts.next()) else {
            if line.trim().is_empty() {
                continue;
            }
            return Err(err("palette.txt", ln, "expected `<char> <#RRGGBB|transparent>`"));
        };
        let &[c] = ch.as_bytes() else {
            return Err(err("palette.txt", ln, format!("`{ch}` is not a single ASCII char")));
        };
        if c >= 128 {
            return Err(err("palette.txt", ln, "palette chars must be ASCII"));
        }
        let rgba = if val == "transparent" {
            [0, 0, 0, 0]
        } else {
            let hex = val.strip_prefix('#').filter(|h| h.len() == 6);
            let v = hex.and_then(|h| u32::from_str_radix(h, 16).ok());
            let Some(v) = v else { return Err(err("palette.txt", ln, format!("bad colour `{val}`"))) };
            [(v >> 16) as u8, (v >> 8) as u8, v as u8, 255]
        };
        if colors[c as usize].replace(rgba).is_some() {
            return Err(err("palette.txt", ln, format!("`{}` defined twice", c as char)));
        }
    }
    Ok(Palette { colors })
}

fn parse_pair(s: &str) -> Option<(i32, i32)> {
    let (a, b) = s.split_once(',')?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}

struct PendingStep {
    line: usize,
    body: &'static str,
    eyes: Option<&'static str>,
    look: (i32, i32),
    dy: i32,
    fx: Vec<&'static str>,
    glyph: Option<&'static str>,
    hold: Option<u32>,
    rot: u8,
    flip: bool,
}

impl PendingStep {
    fn plain(line: usize, body: &'static str) -> Self {
        PendingStep { line, body, eyes: None, look: (0, 0), dy: 0, fx: vec![], glyph: None, hold: None, rot: 0, flip: false }
    }
}

struct PendingAnim {
    line: usize,
    name: &'static str,
    fps: u32,
    looping: bool,
    eyes: &'static str,
    glyph: Option<&'static str>,
    explicit: Option<Vec<&'static str>>,
    steps: Vec<PendingStep>,
    following: Vec<usize>,
}

/// The frames keep pointing into `text`, which is why it must live for ever.
pub fn parse_sheet(text: &'static str, pal: &Palette) -> Result<Sheet, ParseError> {
    const F: &str = "sprites.txt";
    let mut frames: Vec<Frame> = Vec::new();
    let mut rows: Vec<(usize, &'static str)> = Vec::new();
    let mut anims: Vec<PendingAnim> = Vec::new();
    let mut cur: Option<(usize, Frame)> = None;
    // One per parse, shared by every frame: every byte in a row was checked
    // against the palette, so an entry that's left empty is never read.
    let table: &'static [Rgba; 128] = Box::leak(Box::new(std::array::from_fn(|i| pal.colors[i].unwrap_or([0; 4]))));

    // Turn collected rows into the current frame's pixels.
    let finish = |cur: &mut Option<(usize, Frame)>,
                  rows: &mut Vec<(usize, &'static str)>,
                  frames: &mut Vec<Frame>,
                  anims: &mut Vec<PendingAnim>|
     -> Result<(), ParseError> {
        let Some((ln, mut f)) = cur.take() else {
            if let Some(&(l, _)) = rows.first() {
                return Err(err(F, l, "pixel row outside of a @frame"));
            }
            return Ok(());
        };
        if rows.is_empty() {
            return Err(err(F, ln, format!("frame `{}` has no rows", f.name)));
        }
        f.w = rows[0].1.len();
        f.h = rows.len();
        for &(rl, row) in rows.iter() {
            if row.len() != f.w {
                return Err(err(F, rl, format!("row is {} wide, frame `{}` is {} wide", row.len(), f.name, f.w)));
            }
            for (x, c) in row.bytes().enumerate() {
                if pal.color(c).is_none() {
                    return Err(err(F, rl, format!("colour `{}` (column {}) is not in the palette", c as char, x + 1)));
                }
            }
        }
        let offsets = rows.iter().map(|&(_, row)| (row.as_ptr() as usize - text.as_ptr() as usize) as u32).collect();
        f.px = Pixels::Art { rows: offsets, text: text.as_bytes(), pal: table };
        rows.clear();
        if frames.iter().any(|g| g.name == f.name) {
            return Err(err(F, ln, format!("frame `{}` defined twice", f.name)));
        }
        if let Some(a) = anims.last_mut().filter(|a| a.explicit.is_none() && a.steps.is_empty()) {
            a.following.push(frames.len());
        }
        frames.push(f);
        Ok(())
    };

    for (ln, line) in content_lines(text) {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if let Some(rest) = t.strip_prefix("@frame") {
            finish(&mut cur, &mut rows, &mut frames, &mut anims)?;
            let mut parts = rest.split_whitespace();
            let Some(name) = parts.next() else { return Err(err(F, ln, "@frame needs a name")) };
            let mut f = Frame::from_rgba(name, 0, 0, vec![]);
            for kv in parts {
                let (k, v) = kv.split_once('=').ok_or_else(|| err(F, ln, format!("expected key=value, got `{kv}`")))?;
                match k {
                    "anchor" => f.anchor = parse_pair(v).ok_or_else(|| err(F, ln, "anchor=x,y"))?,
                    "eyes" => {
                        f.eyes = v.split(';').map(parse_pair).collect::<Option<_>>().ok_or_else(|| err(F, ln, "eyes=x,y;x,y"))?
                    }
                    "hold" => f.hold = v.parse().ok().filter(|&h| h >= 1).ok_or_else(|| err(F, ln, "hold=<n ≥ 1>"))?,
                    "mirror" => {
                        f.mirror = match v {
                            "yes" => true,
                            "no" => false,
                            _ => return Err(err(F, ln, "mirror=yes|no")),
                        }
                    }
                    _ => return Err(err(F, ln, format!("unknown frame attribute `{k}`"))),
                }
            }
            cur = Some((ln, f));
        } else if let Some(rest) = t.strip_prefix("@anim") {
            finish(&mut cur, &mut rows, &mut frames, &mut anims)?;
            let mut parts = rest.split_whitespace();
            let Some(name) = parts.next() else { return Err(err(F, ln, "@anim needs a name")) };
            let mut a = PendingAnim {
                line: ln,
                name,
                fps: 0,
                looping: true,
                eyes: "normal",
                glyph: None,
                explicit: None,
                steps: vec![],
                following: vec![],
            };
            for p in parts {
                match p.split_once('=') {
                    Some(("fps", v)) => a.fps = v.parse().ok().filter(|&f| f > 0).ok_or_else(|| err(F, ln, "fps=<n > 0>"))?,
                    Some(("frames", v)) => a.explicit = Some(v.split(',').map(str::trim).collect()),
                    Some(("eyes", v)) => a.eyes = v,
                    Some(("glyph", v)) => a.glyph = Some(v),
                    None if p == "loop" => a.looping = true,
                    None if p == "once" => a.looping = false,
                    _ => return Err(err(F, ln, format!("unknown anim attribute `{p}`"))),
                }
            }
            if a.fps == 0 {
                return Err(err(F, ln, format!("anim `{name}` needs fps=")));
            }
            anims.push(a);
        } else if let Some(rest) = t.strip_prefix("@step") {
            finish(&mut cur, &mut rows, &mut frames, &mut anims)?;
            let Some(a) = anims.last_mut() else { return Err(err(F, ln, "@step outside of an @anim")) };
            if a.explicit.is_some() {
                return Err(err(F, ln, "an anim uses either frames= or @step lines, not both"));
            }
            let mut parts = rest.split_whitespace();
            let Some(body) = parts.next() else { return Err(err(F, ln, "@step needs a body frame")) };
            let mut st = PendingStep::plain(ln, body);
            for kv in parts {
                let (k, v) = kv.split_once('=').ok_or_else(|| err(F, ln, format!("expected key=value, got `{kv}`")))?;
                match k {
                    "eyes" => st.eyes = Some(v),
                    "look" => st.look = parse_pair(v).ok_or_else(|| err(F, ln, "look=dx,dy"))?,
                    "dy" => st.dy = v.parse().map_err(|_| err(F, ln, "dy=<n>"))?,
                    "fx" => st.fx = v.split('+').collect(),
                    "glyph" => st.glyph = Some(v),
                    "hold" => st.hold = Some(v.parse().ok().filter(|&h| h >= 1).ok_or_else(|| err(F, ln, "hold=<n ≥ 1>"))?),
                    "rot" => st.rot = v.parse().ok().filter(|&r| r < 4).ok_or_else(|| err(F, ln, "rot=0..3"))?,
                    "flip" => {
                        st.flip = match v {
                            "yes" => true,
                            "no" => false,
                            _ => return Err(err(F, ln, "flip=yes|no")),
                        }
                    }
                    _ => return Err(err(F, ln, format!("unknown step attribute `{k}`"))),
                }
            }
            a.steps.push(st);
        } else if t.starts_with('@') {
            return Err(err(F, ln, format!("unknown directive `{t}`")));
        } else {
            rows.push((ln, t));
        }
    }
    finish(&mut cur, &mut rows, &mut frames, &mut anims)?;

    let by_name: HashMap<&'static str, usize> = frames.iter().enumerate().map(|(i, f)| (f.name, i)).collect();
    let find = |name: &str, line: usize, what: &str| {
        by_name.get(name).copied().ok_or_else(|| err(F, line, format!("{what}: no frame `{name}`")))
    };
    // `none` turns a layer off; other names get the layer's prefix (`happy` → `eye_happy`).
    let layer = |v: &str, prefix: &str, line: usize, what: &str| -> Result<Option<usize>, ParseError> {
        if v == "none" { Ok(None) } else { find(&format!("{prefix}{v}"), line, what).map(Some) }
    };
    let mut out = Vec::new();
    for a in anims {
        let what = format!("anim `{}`", a.name);
        let pending: Vec<PendingStep> = if !a.steps.is_empty() {
            a.steps
        } else if let Some(names) = &a.explicit {
            names.iter().map(|&n| PendingStep::plain(a.line, n)).collect()
        } else {
            a.following.iter().map(|&i| PendingStep::plain(a.line, frames[i].name)).collect()
        };
        if pending.is_empty() {
            return Err(err(F, a.line, format!("{what} has no frames")));
        }
        let mut steps = Vec::with_capacity(pending.len());
        for p in pending {
            let body = find(p.body, p.line, &what)?;
            let eyes = if frames[body].eyes.is_empty() {
                None
            } else {
                layer(p.eyes.unwrap_or(a.eyes), "eye_", p.line, &what)?
            };
            let glyph = match p.glyph.or(a.glyph) {
                Some(g) => layer(g, "glyph_", p.line, &what)?,
                None => None,
            };
            let fx = p.fx.iter().map(|n| find(n, p.line, &what)).collect::<Result<Vec<_>, _>>()?;
            let hold = p.hold.unwrap_or(frames[body].hold);
            steps.push(Step { body, eyes, look: p.look, dy: p.dy, fx, glyph, hold, rot: p.rot, flip: p.flip });
        }
        out.push(Anim { name: a.name, fps: a.fps, looping: a.looping, steps });
    }
    Ok(Sheet { frames, anims: out, by_name })
}

pub const PALETTE_TXT: &str = include_str!("../assets/palette.txt");
pub const SPRITES_TXT: &str = include_str!("../assets/sprites.txt");

/// The embedded art. The embedded files are covered by tests, so this can't fail.
pub fn builtin() -> Sheet {
    let pal = parse_palette(PALETTE_TXT).expect("embedded palette");
    parse_sheet(SPRITES_TXT, &pal).expect("embedded sprites")
}

/// `--art <sprites.txt>`: load art from disk. Uses `palette.txt` from the
/// same folder if there is one, else the embedded palette.
pub fn load(path: &std::path::Path) -> Result<Sheet, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let pal_path = path.with_file_name("palette.txt");
    let pal_text = std::fs::read_to_string(&pal_path).unwrap_or_else(|_| PALETTE_TXT.to_string());
    let pal = parse_palette(&pal_text).map_err(|e| e.to_string())?;
    // Frames point into the text, so a reload leaks the old one: a few
    // dozen KB per save, and only with the --art development flag.
    parse_sheet(text.leak(), &pal).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pal() -> Palette {
        parse_palette(". transparent\nO #D97757 orange\nK #1E1714 ink").unwrap()
    }

    #[test]
    fn builtin_art_parses() {
        let s = builtin();
        let idle = s.frame("stand").unwrap();
        assert_eq!((idle.w, idle.h, idle.anchor), (32, 22, (16, 21)));
        assert_eq!(idle.at(10, 10), [0xD9, 0x77, 0x57, 255]);
        assert_eq!(idle.at(0, 0)[3], 0);
        assert!(!s.anim("idle").unwrap().steps.is_empty());
        // Every pixel is reachable (a bad row offset would panic).
        for f in &s.frames {
            std::hint::black_box(f.at(f.w - 1, f.h - 1));
        }
    }

    #[test]
    fn anim_defaults_to_following_frames() {
        let s = parse_sheet("@anim a fps=4 once\n@frame x\nO\n@frame y\nK\n@anim b fps=1 frames=y,x", &pal()).unwrap();
        let bodies = |n: &str| s.anim(n).unwrap().steps.iter().map(|s| s.body).collect::<Vec<_>>();
        assert_eq!((bodies("a"), s.anim("a").unwrap().looping), (vec![0, 1], false));
        assert_eq!(bodies("b"), vec![1, 0]);
    }

    #[test]
    fn frame_attributes() {
        let s = parse_sheet("@frame x anchor=1,2 eyes=5,4;10,4 hold=3 mirror=no\nOO", &pal()).unwrap();
        let f = s.frame("x").unwrap();
        assert_eq!((f.anchor, f.eyes.clone(), f.hold, f.mirror), ((1, 2), vec![(5, 4), (10, 4)], 3, false));
    }

    #[test]
    fn steps_with_layers_and_defaults() {
        let src = "@frame eye_normal\nK\n@frame eye_happy\nK\n@frame glyph_q\nK\n@frame body eyes=1,0 hold=2\nOO\n@frame noeyes\nO\n@frame spark\nK\n\
                   @anim a fps=4 once glyph=q\n@step body\n@step body eyes=happy look=-1,1 dy=-2 fx=spark+spark glyph=none hold=5\n@step noeyes rot=3 flip=yes";
        let s = parse_sheet(src, &pal()).unwrap();
        let a = s.anim("a").unwrap();
        let ix = |n: &str| s.index(n).unwrap();
        let [s0, s1, s2] = &a.steps[..] else { panic!() };
        assert_eq!((s0.eyes, s0.glyph, s0.hold), (Some(ix("eye_normal")), Some(ix("glyph_q")), 2));
        assert_eq!((s1.eyes, s1.look, s1.dy, s1.glyph, s1.hold), (Some(ix("eye_happy")), (-1, 1), -2, None, 5));
        assert_eq!(s1.fx, vec![ix("spark"), ix("spark")]);
        assert_eq!(s2.eyes, None, "a body without eye anchors gets no eyes");
        assert_eq!((s0.rot, s0.flip, s2.rot, s2.flip), (0, false, 3, true));
        assert_eq!(a.duration_ms(), 8 * 250);
    }

    #[test]
    fn step_errors_name_the_line() {
        let e = parse_sheet("@frame x eyes=0,0\nO\n@anim a fps=2\n@step x eyes=sad", &pal()).unwrap_err();
        assert_eq!(e.line, 4);
        assert!(e.msg.contains("eye_sad"), "{e}");
        assert_eq!(parse_sheet("@step x", &pal()).unwrap_err().line, 1);
    }

    #[test]
    fn ragged_rows_name_the_line() {
        let e = parse_sheet("// c\n@frame x\nOO\nO", &pal()).unwrap_err();
        assert_eq!(e.line, 4);
        assert!(e.msg.contains("wide"), "{e}");
    }

    #[test]
    fn unknown_colour_names_the_line() {
        let e = parse_sheet("@frame x\nOO\nOZ", &pal()).unwrap_err();
        assert_eq!(e.line, 3);
        assert!(e.msg.contains("`Z`"), "{e}");
    }

    #[test]
    fn missing_frame_in_anim() {
        let e = parse_sheet("@anim a fps=2 frames=nope\n@frame x\nO", &pal()).unwrap_err();
        assert_eq!(e.line, 1);
        assert!(e.msg.contains("nope"));
    }

    #[test]
    fn bad_palette() {
        assert_eq!(parse_palette("O #12345").unwrap_err().line, 1);
        assert!(parse_palette("O #123456\nO #000000").unwrap_err().msg.contains("twice"));
    }
}
