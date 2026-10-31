//! Software renderer: fonts (fontdue), glyph cache, pixel-exact box drawing,
//! and the Shadow TUI Restricted HUD concept (status strip, command-block gutter,
//! scanlines, boot sequence, alert flash).
use crate::config::Config;
use crate::vt::{self, Color, CursorShape, Term};
use fontdue::{Font, FontSettings, Metrics};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

// ====================================================================== fonts
pub struct Glyph {
    pub m: Metrics,
    pub bmp: Vec<u8>,
}

pub struct Fonts {
    regular: Font,
    bold: Option<Font>,
    fallbacks: Vec<Font>,
    pub px: f32,
    pub cell_w: usize,
    pub cell_h: usize,
    pub ascent: i32,
    cache: HashMap<(char, bool), Glyph>,
}

fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase()
}

fn font_files() -> Vec<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    let mut dirs: Vec<PathBuf> = vec!["/usr/share/fonts".into(), "/usr/local/share/fonts".into()];
    dirs.push(Path::new(&home).join(".local/share/fonts"));
    dirs.push(Path::new(&home).join(".fonts"));
    let mut out = vec![];
    let mut stack = dirs;
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if let Some(ext) = p.extension().and_then(|x| x.to_str()) {
                if matches!(ext.to_lowercase().as_str(), "ttf" | "otf" | "ttc") {
                    out.push(p);
                }
            }
        }
    }
    out.sort();
    out
}

fn load_font(p: &Path) -> Option<Font> {
    let data = std::fs::read(p).ok()?;
    Font::from_bytes(data, FontSettings { scale: 40.0, ..FontSettings::default() }).ok()
}

fn find_style(files: &[PathBuf], family: &str, style: &str) -> Option<PathBuf> {
    let fam = norm(family);
    let want = format!("{fam}{style}");
    let stem = |p: &PathBuf| norm(&p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default());
    files.iter().find(|p| stem(p) == want).cloned().or_else(|| {
        files.iter().find(|p| {
            let s = stem(p);
            s.starts_with(&fam) && s.ends_with(style) && !s.contains("propo") && !s.contains("italic")
        }).cloned()
    })
}

impl Fonts {
    pub fn load(family: &str, px: f32) -> Result<Fonts, String> {
        let files = font_files();
        let reg_path = find_style(&files, family, "regular")
            .or_else(|| find_style(&files, "DejaVu Sans Mono", ""))
            .or_else(|| find_style(&files, "DejaVuSansMono", ""))
            .or_else(|| find_style(&files, "Noto Sans Mono", "regular"))
            .or_else(|| find_style(&files, "Liberation Mono", "regular"))
            .ok_or_else(|| format!("no usable monospace font found (wanted {family:?})"))?;
        let regular = load_font(&reg_path).ok_or("failed to parse the main font")?;
        let bold = find_style(&files, family, "bold").and_then(|p| load_font(&p));
        let mut fallbacks = vec![];
        for (fam, style) in [
            ("DejaVu Sans Mono", ""),
            ("Noto Sans Mono", "regular"),
            ("Noto Sans Symbols2", "regular"),
            ("Noto Sans Symbols", "regular"),
            ("Noto Sans CJK", "regular"),
            ("Noto Sans", "regular"),
            ("DejaVu Sans", ""),
        ] {
            if let Some(p) = find_style(&files, fam, style) {
                if p != reg_path {
                    if let Some(f) = load_font(&p) {
                        fallbacks.push(f);
                    }
                }
            }
        }
        let mut f = Fonts { regular, bold, fallbacks, px, cell_w: 8, cell_h: 16, ascent: 12, cache: HashMap::new() };
        f.set_px(px);
        Ok(f)
    }

    pub fn set_px(&mut self, px: f32) {
        self.px = px;
        self.cache.clear();
        let lm = self.regular.horizontal_line_metrics(px);
        let (asc, desc, gap) = lm.map(|l| (l.ascent, l.descent, l.line_gap)).unwrap_or((px * 0.8, -px * 0.2, 0.0));
        let adv = self.regular.metrics('M', px).advance_width;
        self.cell_w = adv.round().max(1.0) as usize;
        self.cell_h = (asc - desc + gap).ceil().max(1.0) as usize;
        self.ascent = asc.round() as i32;
    }

    pub fn glyph(&mut self, c: char, bold: bool) -> &Glyph {
        if !self.cache.contains_key(&(c, bold)) {
            let primary = if bold { self.bold.as_ref().unwrap_or(&self.regular) } else { &self.regular };
            let font = if primary.lookup_glyph_index(c) != 0 {
                primary
            } else {
                self.fallbacks.iter().find(|f| f.lookup_glyph_index(c) != 0).unwrap_or(primary)
            };
            let (m, bmp) = font.rasterize(c, self.px);
            self.cache.insert((c, bold), Glyph { m, bmp });
            if self.cache.len() > 6000 {
                self.cache.clear();
                let (m, bmp) = self.regular.rasterize(c, self.px);
                self.cache.insert((c, bold), Glyph { m, bmp });
            }
        }
        &self.cache[&(c, bold)]
    }
}

// ====================================================================== canvas
pub type Rgb = (u8, u8, u8);

pub struct Canvas<'a> {
    pub buf: &'a mut [u8],
    pub w: usize,
    pub h: usize,
}

#[inline]
fn premul(c: Rgb, a: f32) -> [u8; 4] {
    let a8 = (a.clamp(0.0, 1.0) * 255.0).round() as u32;
    let m = |v: u8| ((v as u32 * a8 + 127) / 255) as u8;
    [m(c.2), m(c.1), m(c.0), a8 as u8] // BGRA little-endian for ARGB8888
}

impl<'a> Canvas<'a> {
    pub fn fill(&mut self, x: i32, y: i32, w: i32, h: i32, c: Rgb, a: f32) {
        let px = premul(c, a);
        let (x0, y0) = (x.max(0) as usize, y.max(0) as usize);
        let x1 = ((x + w).max(0) as usize).min(self.w);
        let y1 = ((y + h).max(0) as usize).min(self.h);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        for yy in y0..y1 {
            let row = &mut self.buf[(yy * self.w + x0) * 4..(yy * self.w + x1) * 4];
            for p in row.chunks_exact_mut(4) {
                p.copy_from_slice(&px);
            }
        }
    }

    /// Source-over blend of a solid colour with coverage alpha.
    #[inline]
    fn blend_px(&mut self, x: usize, y: usize, c: Rgb, a: f32) {
        if x >= self.w || y >= self.h || a <= 0.0 {
            return;
        }
        let i = (y * self.w + x) * 4;
        let s = premul(c, a);
        let ia = 1.0 - a;
        for k in 0..4 {
            self.buf[i + k] = (s[k] as f32 + self.buf[i + k] as f32 * ia).round().min(255.0) as u8;
        }
    }

    pub fn blend(&mut self, x: i32, y: i32, w: i32, h: i32, c: Rgb, a: f32) {
        for yy in y.max(0)..(y + h).min(self.h as i32) {
            for xx in x.max(0)..(x + w).min(self.w as i32) {
                self.blend_px(xx as usize, yy as usize, c, a);
            }
        }
    }

    pub fn glyph(&mut self, g: &Glyph, x: i32, baseline: i32, c: Rgb, alpha: f32) {
        let gx = x + g.m.xmin;
        let gy = baseline - g.m.height as i32 - g.m.ymin;
        for row in 0..g.m.height {
            let yy = gy + row as i32;
            if yy < 0 || yy >= self.h as i32 {
                continue;
            }
            for col in 0..g.m.width {
                let cov = g.bmp[row * g.m.width + col];
                if cov == 0 {
                    continue;
                }
                let xx = gx + col as i32;
                if xx < 0 || xx >= self.w as i32 {
                    continue;
                }
                self.blend_px(xx as usize, yy as usize, c, cov as f32 / 255.0 * alpha);
            }
        }
    }
}

// ====================================================================== box drawing
/// (up, right, down, left) weights: 0 none, 1 light, 2 heavy, 3 double
fn box_parts(c: char) -> Option<[u8; 4]> {
    Some(match c {
        '─' | '┄' | '┈' | '╌' => [0, 1, 0, 1],
        '━' | '┅' | '┉' | '╍' => [0, 2, 0, 2],
        '│' | '┆' | '┊' | '╎' => [1, 0, 1, 0],
        '┃' | '┇' | '┋' | '╏' => [2, 0, 2, 0],
        '┌' | '╭' => [0, 1, 1, 0],
        '┐' | '╮' => [0, 0, 1, 1],
        '└' | '╰' => [1, 1, 0, 0],
        '┘' | '╯' => [1, 0, 0, 1],
        '├' => [1, 1, 1, 0],
        '┤' => [1, 0, 1, 1],
        '┬' => [0, 1, 1, 1],
        '┴' => [1, 1, 0, 1],
        '┼' => [1, 1, 1, 1],
        '┏' => [0, 2, 2, 0],
        '┓' => [0, 0, 2, 2],
        '┗' => [2, 2, 0, 0],
        '┛' => [2, 0, 0, 2],
        '┣' => [2, 2, 2, 0],
        '┫' => [2, 0, 2, 2],
        '┳' => [0, 2, 2, 2],
        '┻' => [2, 2, 0, 2],
        '╋' => [2, 2, 2, 2],
        '═' => [0, 3, 0, 3],
        '║' => [3, 0, 3, 0],
        '╔' => [0, 3, 3, 0],
        '╗' => [0, 0, 3, 3],
        '╚' => [3, 3, 0, 0],
        '╝' => [3, 0, 0, 3],
        '╠' => [3, 3, 3, 0],
        '╣' => [3, 0, 3, 3],
        '╦' => [0, 3, 3, 3],
        '╩' => [3, 3, 0, 3],
        '╬' => [3, 3, 3, 3],
        '╴' => [0, 0, 0, 1],
        '╵' => [1, 0, 0, 0],
        '╶' => [0, 1, 0, 0],
        '╷' => [0, 0, 1, 0],
        _ => return None,
    })
}

/// Double-line segments: (horizontal?, fixed, from, to) with symbolic
/// coordinates 0 = cell edge, 1 = A (centre - gap), 2 = B (centre + gap), 3 = far edge.
fn double_segments(c: char) -> Option<&'static [(bool, u8, u8, u8)]> {
    Some(match c {
        '═' => &[(true, 1, 0, 3), (true, 2, 0, 3)],
        '║' => &[(false, 1, 0, 3), (false, 2, 0, 3)],
        '╔' => &[(true, 1, 1, 3), (true, 2, 2, 3), (false, 1, 1, 3), (false, 2, 2, 3)],
        '╗' => &[(true, 1, 0, 2), (true, 2, 0, 1), (false, 2, 1, 3), (false, 1, 2, 3)],
        '╚' => &[(true, 2, 1, 3), (true, 1, 2, 3), (false, 1, 0, 2), (false, 2, 0, 1)],
        '╝' => &[(true, 2, 0, 2), (true, 1, 0, 1), (false, 2, 0, 2), (false, 1, 0, 1)],
        '╠' => &[(false, 1, 0, 3), (false, 2, 0, 1), (false, 2, 2, 3), (true, 1, 2, 3), (true, 2, 2, 3)],
        '╣' => &[(false, 2, 0, 3), (false, 1, 0, 1), (false, 1, 2, 3), (true, 1, 0, 1), (true, 2, 0, 1)],
        '╦' => &[(true, 1, 0, 3), (true, 2, 0, 1), (true, 2, 2, 3), (false, 1, 2, 3), (false, 2, 2, 3)],
        '╩' => &[(true, 2, 0, 3), (true, 1, 0, 1), (true, 1, 2, 3), (false, 1, 0, 1), (false, 2, 0, 1)],
        '╬' => &[
            (true, 1, 0, 1), (true, 1, 2, 3), (true, 2, 0, 1), (true, 2, 2, 3),
            (false, 1, 0, 1), (false, 1, 2, 3), (false, 2, 0, 1), (false, 2, 2, 3),
        ],
        _ => return None,
    })
}

/// Draw box-drawing / block characters procedurally so lines join seamlessly.
fn draw_special(cv: &mut Canvas, c: char, x: i32, y: i32, cw: i32, ch: i32, col: Rgb, scale: i32) -> bool {
    let t = scale.max(1); // light line thickness
    let mx = x + cw / 2;
    let my = y + ch / 2;
    if let Some(segs) = double_segments(c) {
        let gap = t + 1;
        let hx = [x, mx - gap, mx + gap, x + cw];
        let vy = [y, my - gap, my + gap, y + ch];
        for &(horizontal, fixed, from, to) in segs {
            if horizontal {
                let yy = vy[fixed as usize];
                let (a, b) = (hx[from as usize], hx[to as usize]);
                let (a, b) = if from == 1 || from == 2 { (a - t / 2, b) } else { (a, b) };
                cv.fill(a, yy - t / 2, (b - a + if to == 1 || to == 2 { t } else { 0 }).max(1), t, col, 1.0);
            } else {
                let xx = hx[fixed as usize];
                let (a, b) = (vy[from as usize], vy[to as usize]);
                let (a, b) = if from == 1 || from == 2 { (a - t / 2, b) } else { (a, b) };
                cv.fill(xx - t / 2, a, t, (b - a + if to == 1 || to == 2 { t } else { 0 }).max(1), col, 1.0);
            }
        }
        return true;
    }
    if let Some([u, r, d, l]) = box_parts(c) {
        let seg = |cv: &mut Canvas, w: u8, vertical: bool, from: i32, to: i32| {
            if w == 0 {
                return;
            }
            let th = if w == 2 { t * 2 } else { t };
            if w == 3 {
                let gap = t + 1;
                for off in [-gap, gap] {
                    if vertical {
                        cv.fill(mx + off - t / 2, from, t, to - from, col, 1.0);
                    } else {
                        cv.fill(from, my + off - t / 2, to - from, t, col, 1.0);
                    }
                }
            } else if vertical {
                cv.fill(mx - th / 2, from, th, to - from, col, 1.0);
            } else {
                cv.fill(from, my - th / 2, to - from, th, col, 1.0);
            }
        };
        let ext = 2 * t; // overlap through the centre
        seg(cv, u, true, y, my + ext);
        seg(cv, d, true, my - ext, y + ch);
        seg(cv, l, false, x, mx + ext);
        seg(cv, r, false, mx - ext, x + cw);
        return true;
    }
    match c {
        '█' => cv.fill(x, y, cw, ch, col, 1.0),
        '▀' => cv.fill(x, y, cw, ch / 2, col, 1.0),
        '▄' => cv.fill(x, y + ch / 2, cw, ch - ch / 2, col, 1.0),
        '▌' => cv.fill(x, y, cw / 2, ch, col, 1.0),
        '▐' => cv.fill(x + cw / 2, y, cw - cw / 2, ch, col, 1.0),
        '░' => cv.blend(x, y, cw, ch, col, 0.25),
        '▒' => cv.blend(x, y, cw, ch, col, 0.5),
        '▓' => cv.blend(x, y, cw, ch, col, 0.75),
        '▁'..='▇' => {
            let eighths = c as i32 - '▀' as i32; // ▁=1 .. ▇=7
            let h = ch * eighths / 8;
            cv.fill(x, y + ch - h, cw, h, col, 1.0)
        }
        '▉'..='▏' => {
            let eighths = 8 - (c as i32 - '█' as i32); // ▉=7 .. ▏=1
            cv.fill(x, y, cw * eighths / 8, ch, col, 1.0)
        }
        _ => return false,
    }
    true
}

// ====================================================================== layout + view state
pub struct View {
    pub scale: i32,
    pub pad: i32,
    pub hud_h: i32,
    pub gutter_w: i32,
    pub x0: i32,
    pub y0: i32,
}

pub fn layout(cfg: &Config, fonts: &Fonts, scale: i32) -> View {
    let pad = cfg.padding as i32 * scale;
    let hud_h = if cfg.hud { fonts.cell_h as i32 + 8 * scale } else { 0 };
    let gutter_w = if cfg.gutter { (fonts.cell_w as i32 * 3) / 2 } else { 0 };
    View { scale, pad, hud_h, gutter_w, x0: pad + gutter_w, y0: pad + hud_h + if cfg.hud { 4 * scale } else { 0 } }
}

pub struct Ui<'a> {
    pub cfg: &'a Config,
    pub focused: bool,
    pub scroll_off: usize,
    pub selection: Option<((u64, usize), (u64, usize))>,
    pub started: Instant,
    pub bell_at: Option<Instant>,
    pub user_host: &'a str,
    pub cwd: &'a str,
    pub foreground: &'a str,
    pub shell_name: &'a str,
    pub exited: Option<i32>,
    pub blink_on: bool,
}

fn rgb_of(c: Color, cfg: &Config, fg: bool) -> Rgb {
    match c {
        Color::Default => if fg { cfg.fg } else { cfg.bg },
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Idx(i) if i < 16 => cfg.palette[i as usize],
        Color::Idx(i) if i < 232 => {
            let i = i - 16;
            let v = |n: u8| if n == 0 { 0 } else { 55 + n * 40 };
            (v(i / 36), v((i / 6) % 6), v(i % 6))
        }
        Color::Idx(i) => {
            let g = 8 + (i - 232) * 10;
            (g, g, g)
        }
    }
}

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    (m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

/// Draw `s` in the HUD/overlay font; returns the advance in pixels.
fn text(cv: &mut Canvas, fonts: &mut Fonts, s: &str, x: i32, y: i32, col: Rgb, bold: bool) -> i32 {
    let base = y + fonts.ascent;
    let mut xx = x;
    let cw = fonts.cell_w as i32;
    for c in s.chars() {
        if c != ' ' {
            let g = fonts.glyph(c, bold);
                        cv.glyph(g, xx, base, col, 1.0);
        }
        xx += cw;
    }
    xx - x
}

// ====================================================================== frame
pub fn draw(cv: &mut Canvas, fonts: &mut Fonts, term: &Term, v: &View, ui: &Ui) {
    let cfg = ui.cfg;
    let (cw, ch) = (fonts.cell_w as i32, fonts.cell_h as i32);
    let s = v.scale;
    let age = ui.started.elapsed().as_secs_f32();
    let boot = if cfg.boot { (age / 0.9).min(1.0) } else { 1.0 };
    let bell = ui.bell_at.map(|t| t.elapsed().as_secs_f32() < 0.35).unwrap_or(false);

    // background
    cv.fill(0, 0, cv.w as i32, cv.h as i32, cfg.bg, cfg.opacity);

    // ---- grid
    let rows = term.rows;
    let view_top = term.abs_line(0).saturating_sub(ui.scroll_off as u64);
    let sel = ui.selection.map(|(a, b)| if a <= b { (a, b) } else { (b, a) });
    let in_sel = |abs: u64, col: usize| -> bool {
        match sel {
            Some((a, b)) => (abs, col) >= a && (abs, col) <= b,
            None => false,
        }
    };
    for row in 0..rows {
        let abs = view_top + row as u64;
        let Some(line) = term.line_at(abs) else { continue };
        let y = v.y0 + row as i32 * ch;
        // backgrounds (+ selection)
        for (col, cell) in line.iter().enumerate().take(term.cols) {
            let x = v.x0 + col as i32 * cw;
            let (mut fg, mut bg) = (cell.fg, cell.bg);
            if cell.flags & vt::REVERSE != 0 {
                std::mem::swap(&mut fg, &mut bg);
                if bg == Color::Default {
                    bg = Color::Rgb(cfg.fg.0, cfg.fg.1, cfg.fg.2);
                }
            }
            let _ = fg;
            if bg != Color::Default {
                cv.fill(x, y, cw, ch, rgb_of(bg, cfg, false), 1.0);
            }
            if in_sel(abs, col) {
                cv.blend(x, y, cw, ch, cfg.selection, 0.85);
            }
        }
    }
    // scanlines over the background layer (text stays crisp on top)
    if cfg.scanlines {
        let step = (3 * s).max(3);
        let mut yy = v.y0;
        let gw = term.cols as i32 * cw;
        while yy < v.y0 + rows as i32 * ch {
            cv.blend(v.x0, yy, gw, s.max(1), (0, 0, 0), 0.10);
            yy += step;
        }
    }
    // glyphs + decorations
    for row in 0..rows {
        let abs = view_top + row as u64;
        let Some(line) = term.line_at(abs) else { continue };
        let y = v.y0 + row as i32 * ch;
        for (col, cell) in line.iter().enumerate().take(term.cols) {
            if cell.flags & vt::SPACER != 0 || cell.flags & vt::HIDDEN != 0 {
                continue;
            }
            let x = v.x0 + col as i32 * cw;
            let (mut fg, mut bg) = (cell.fg, cell.bg);
            if cell.flags & vt::REVERSE != 0 {
                std::mem::swap(&mut fg, &mut bg);
                if fg == Color::Default {
                    fg = Color::Rgb(cfg.bg.0, cfg.bg.1, cfg.bg.2);
                }
            }
            let mut col_rgb = rgb_of(fg, cfg, true);
            // bold brightens the 8 base colours, like most terminals
            if cell.flags & vt::BOLD != 0 {
                if let Color::Idx(i) = fg {
                    if i < 8 {
                        col_rgb = cfg.palette[i as usize + 8];
                    }
                }
            }
            if cell.flags & vt::DIM != 0 {
                col_rgb = mix(col_rgb, cfg.bg, 0.45);
            }
            let w = if cell.flags & vt::WIDE != 0 { cw * 2 } else { cw };
            if cell.ch != ' ' {
                let alpha = boot.powf(0.5);
                if !draw_special(cv, cell.ch, x, y, w, ch, col_rgb, s) {
                    let asc = fonts.ascent;
                    let g = fonts.glyph(cell.ch, cell.flags & vt::BOLD != 0);
                                        cv.glyph(g, x, y + asc, col_rgb, alpha);
                }
            }
            if cell.flags & vt::UNDERLINE != 0 {
                cv.fill(x, y + ch - 2 * s, w, s, col_rgb, 1.0);
            }
            if cell.flags & vt::STRIKE != 0 {
                cv.fill(x, y + ch / 2, w, s, col_rgb, 1.0);
            }
        }
    }

    // ---- cursor
    if ui.scroll_off == 0 && term.cursor_visible && ui.exited.is_none() && (ui.blink_on || !ui.focused) {
        let x = v.x0 + term.cx as i32 * cw;
        let y = v.y0 + term.cy as i32 * ch;
        let cell = term.screen()[term.cy][term.cx];
        let wide = cell.flags & vt::WIDE != 0;
        let w = if wide { cw * 2 } else { cw };
        let shape = match cfg.cursor_style.as_str() {
            "beam" => CursorShape::Beam,
            "underline" => CursorShape::Underline,
            _ => term.cursor_shape,
        };
        if !ui.focused {
            // hollow box when unfocused
            cv.fill(x, y, w, s, cfg.cursor, 1.0);
            cv.fill(x, y + ch - s, w, s, cfg.cursor, 1.0);
            cv.fill(x, y, s, ch, cfg.cursor, 1.0);
            cv.fill(x + w - s, y, s, ch, cfg.cursor, 1.0);
        } else {
            match shape {
                CursorShape::Block => {
                    cv.fill(x, y, w, ch, cfg.cursor, 1.0);
                    // soft glow
                    cv.blend(x - s, y - s, w + 2 * s, ch + 2 * s, cfg.cursor, 0.15);
                    if cell.ch != ' ' {
                        if !draw_special(cv, cell.ch, x, y, w, ch, cfg.bg, s) {
                            let asc = fonts.ascent;
                            let g = fonts.glyph(cell.ch, cell.flags & vt::BOLD != 0);
                                                        cv.glyph(g, x, y + asc, cfg.bg, 1.0);
                        }
                    }
                }
                CursorShape::Beam => cv.fill(x, y, 2 * s, ch, cfg.cursor, 1.0),
                CursorShape::Underline => cv.fill(x, y + ch - 2 * s, w, 2 * s, cfg.cursor, 1.0),
            }
        }
    }

    // ---- command-block gutter (OSC 133)
    if cfg.gutter && v.gutter_w > 0 {
        let gx = v.pad;
        let last_abs = view_top + rows as u64;
        for b in term.blocks.iter() {
            if b.prompt_line >= last_abs {
                continue;
            }
            let end = b.end_line.unwrap_or(term.abs_line(term.cy) + 1);
            if end < view_top {
                continue;
            }
            let color = match (b.end_line, b.exit) {
                (None, _) if b.cmd_line.is_some() => cfg.accent2,
                (None, _) => cfg.accent,
                (Some(_), Some(0)) => cfg.ok,
                (Some(_), _) => cfg.alert,
            };
            // prompt marker
            if b.prompt_line >= view_top {
                let y = v.y0 + (b.prompt_line - view_top) as i32 * ch;
                let asc = fonts.ascent;
                let g = fonts.glyph('▸', true);
                                cv.glyph(g, gx, y + asc, color, 1.0);
                // result badge at the right end of the command line
                if let (Some(_), Some(code)) = (b.end_line, b.exit) {
                    let dur = b.duration.map(|d| if d >= 1.0 { format!(" {d:.1}s") } else { format!(" {:.0}ms", d * 1000.0) }).unwrap_or_default();
                    let badge = if code == 0 { format!("✓{dur}") } else { format!("✗ {code}{dur}") };
                    let n = badge.chars().count();
                    let line = term.line_at(b.prompt_line);
                    let free = line.map(|l| l.iter().rev().take(n + 2).all(|c| c.ch == ' ')).unwrap_or(false);
                    if free && n + 2 < term.cols {
                        let bx = v.x0 + (term.cols - n) as i32 * cw;
                        let col = if code == 0 { mix(cfg.ok, cfg.bg, 0.35) } else { cfg.alert };
                        text(cv, fonts, &badge, bx, y, col, false);
                    }
                }
            }
            // output bar
            if let Some(cmd) = b.cmd_line {
                let from = cmd.max(view_top);
                let to = end.min(last_abs);
                if to > from {
                    let y1 = v.y0 + (from - view_top) as i32 * ch;
                    let y2 = v.y0 + (to - view_top) as i32 * ch;
                    let pulse = if b.end_line.is_none() { 0.55 + 0.45 * (age * 4.0).sin().abs() } else { 0.8 };
                    cv.blend(gx + cw / 3, y1, (2 * s).max(2), y2 - y1, color, pulse);
                }
            }
        }
    }

    // ---- tactical frame
    if cfg.frame {
        let (w, h) = (cv.w as i32, cv.h as i32);
        let l = 14 * s;
        let t = s.max(1);
        let c = if bell { cfg.alert } else { cfg.accent };
        let off = (v.pad / 3).max(2);
        for (x, y, dx, dy) in [(off, off, 1, 1), (w - off, off, -1, 1), (off, h - off, 1, -1), (w - off, h - off, -1, -1)] {
            let hx = if dx > 0 { x } else { x - l };
            let vy = if dy > 0 { y } else { y - l };
            cv.fill(hx, if dy > 0 { y } else { y - t }, l, t, c, 0.9);
            cv.fill(if dx > 0 { x } else { x - t }, vy, t, l, c, 0.9);
        }
    }
    if bell {
        let (w, h) = (cv.w as i32, cv.h as i32);
        let t = 2 * s;
        cv.fill(0, 0, w, t, cfg.alert, 0.9);
        cv.fill(0, h - t, w, t, cfg.alert, 0.9);
        cv.fill(0, 0, t, h, cfg.alert, 0.9);
        cv.fill(w - t, 0, t, h, cfg.alert, 0.9);
    }

    // ---- HUD strip
    if cfg.hud {
        hud(cv, fonts, term, v, ui, bell);
    }

    // ---- boot sequence overlay
    if cfg.boot && age < 1.2 {
        let fade = if age < 0.8 { 1.0 } else { 1.0 - (age - 0.8) / 0.4 };
        let lines = [
            format!("{} // TERMINAL", ui.cfg.unit),
            "UPLINK ............ OK".to_string(),
            "CRYPTO ............ OK".to_string(),
            format!("SHELL ............. {}", ui.shell_name.to_uppercase()),
        ];
        let shown = ((age / 0.18) as usize + 1).min(lines.len());
        let bw = 34 * cw;
        let bh = (lines.len() as i32 + 2) * ch;
        let bx = (cv.w as i32 - bw) / 2;
        let by = (cv.h as i32 - bh) / 2;
        cv.blend(bx, by, bw, bh, cfg.panel, 0.92 * fade);
        cv.blend(bx, by, bw, s.max(1), cfg.accent, fade);
        cv.blend(bx, by + bh - s.max(1), bw, s.max(1), cfg.accent, fade);
        for (i, l) in lines.iter().take(shown).enumerate() {
            let col = if i == 0 { cfg.accent } else { mix(cfg.bg, cfg.fg, fade) };
            let base = by + (i as i32 + 1) * ch;
            let mut xx = bx + 2 * cw;
            for c in l.chars() {
                if c != ' ' {
                    let asc = fonts.ascent;
                    let g = fonts.glyph(c, i == 0);
                                        cv.glyph(g, xx, base + asc, col, fade);
                }
                xx += cw;
            }
        }
    }
}

fn hud(cv: &mut Canvas, fonts: &mut Fonts, term: &Term, v: &View, ui: &Ui, bell: bool) {
    let cfg = ui.cfg;
    let (cw, ch) = (fonts.cell_w as i32, fonts.cell_h as i32);
    let s = v.scale;
    let x = v.pad;
    let y = v.pad;
    let w = cv.w as i32 - 2 * v.pad;
    let h = v.hud_h;
    cv.fill(x, y, w, h, cfg.panel, 0.95);
    cv.fill(x, y + h - s.max(1), w, s.max(1), cfg.accent, 0.8);
    let ty = y + (h - ch) / 2;

    // unit tag (inverted)
    let tag = format!(" ◢ {} ", ui.cfg.unit);
    let tag_w = tag.chars().count() as i32 * cw;
    cv.fill(x, y, tag_w, h, cfg.accent, 1.0);
    text(cv, fonts, &tag, x, ty, cfg.bg, true);
    let mut cx = x + tag_w + cw;

    let mut field = |cv: &mut Canvas, fonts: &mut Fonts, label: &str, value: &str, max: usize| -> i32 {
        let start = cx;
        cx += text(cv, fonts, label, cx, ty, cfg.dim, false) + cw / 2;
        let val: String = if value.chars().count() > max {
            format!("…{}", value.chars().rev().take(max - 1).collect::<Vec<_>>().into_iter().rev().collect::<String>())
        } else {
            value.to_string()
        };
        cx += text(cv, fonts, &val, cx, ty, cfg.fg, false) + cw * 2;
        cx - start
    };
    let avail_cols = (w / cw) as usize;
    field(cv, fonts, "OPR", ui.user_host, 24);
    let cwd_max = avail_cols.saturating_sub(70).clamp(8, 48);
    field(cv, fonts, "LOC", ui.cwd, cwd_max);

    // right side: status badge + mission clock
    let t = ui.started.elapsed().as_secs();
    let clock = format!("T+{:02}:{:02}:{:02}", t / 3600, (t / 60) % 60, t % 60);
    let (badge, bcol): (String, Rgb) = if let Some(code) = ui.exited {
        (format!(" EXIT {code} · ANY KEY CLOSES "), cfg.alert)
    } else if bell {
        (" ALERT ".into(), cfg.alert)
    } else if ui.scroll_off > 0 {
        (format!(" ▲ SCROLLBACK -{} ", ui.scroll_off), cfg.accent2)
    } else {
        match term.blocks.last() {
            Some(b) if b.cmd_line.is_some() && b.end_line.is_none() => {
                let el = b.started.map(|s| s.elapsed().as_secs_f32()).unwrap_or(0.0);
                (format!(" RUN {} {:.0}s ", ui.foreground.to_uppercase(), el), cfg.accent2)
            }
            _ => match term.last_finished().and_then(|b| b.exit) {
                Some(0) | None => (" LINK ● OK ".into(), cfg.ok),
                Some(c) => (format!(" SIG {c} "), cfg.alert),
            },
        }
    };
    let clock_w = clock.chars().count() as i32 * cw;
    let badge_w = badge.chars().count() as i32 * cw;
    let rx = x + w - clock_w - cw;
    text(cv, fonts, &clock, rx, ty, cfg.accent, false);
    let bx = rx - badge_w - cw;
    if bx > cx {
        cv.fill(bx, y + s, badge_w, h - 2 * s - s.max(1), bcol, 0.22);
        text(cv, fonts, &badge, bx, ty, bcol, true);
    }
}

#[allow(dead_code)]
pub fn cell_px(fonts: &Fonts) -> (i32, i32) {
    (fonts.cell_w as i32, fonts.cell_h as i32)
}
