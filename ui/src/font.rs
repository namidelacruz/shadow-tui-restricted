//! Fonts: found through fontconfig, memory-mapped, rasterised lazily with ab_glyph.
//! The primary face is the configured monospace font; fallbacks (symbols, CJK, a general sans)
//! are only opened when a character is missing from the primary face.
use ab_glyph::{Font as _, FontRef, ScaleFont as _};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct Glyph {
    pub left: i32,
    /// offset of the bitmap's top edge from the baseline (negative = above)
    pub top: i32,
    pub w: usize,
    pub h: usize,
    pub adv: f32,
    pub bmp: Vec<u8>,
}

struct Face {
    path: PathBuf,
    font: Option<Option<FontRef<'static>>>,
}

impl Face {
    fn new(path: PathBuf) -> Face {
        Face { path, font: None }
    }
    fn get(&mut self) -> Option<&FontRef<'static>> {
        let p = &self.path;
        self.font.get_or_insert_with(|| map_font(p)).as_ref()
    }
}

pub struct Fonts {
    regular: Face,
    bold: Option<Face>,
    fallbacks: Vec<Face>,
    cache: HashMap<(char, u32, bool), Glyph>,
}

/// Default family; [look] font = "..." in config.toml overrides it.
pub const DEFAULT_FAMILY: &str = "JetBrainsMono Nerd Font Mono";

fn fc_match(pattern: &str) -> Option<PathBuf> {
    let out = std::process::Command::new("fc-match").args(["-f", "%{file}", pattern]).output().ok()?;
    let p = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string());
    p.is_file().then_some(p)
}

fn fc_spacing_mono(path: &Path) -> bool {
    let n = path.file_name().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
    if n.contains("mono") || n.contains("code") || n.contains("courier") {
        return true;
    }
    std::process::Command::new("fc-query")
        .args(["-f", "%{spacing}", &path.to_string_lossy()])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim_start().starts_with("100"))
        .unwrap_or(false)
}

fn scan(name: &str) -> Option<PathBuf> {
    let mut stack: Vec<PathBuf> = vec!["/usr/share/fonts".into(), "/usr/local/share/fonts".into()];
    if let Ok(h) = std::env::var("HOME") {
        stack.push(Path::new(&h).join(".local/share/fonts"));
    }
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().is_some_and(|f| f.to_string_lossy().eq_ignore_ascii_case(name)) {
                return Some(p);
            }
        }
    }
    None
}

fn map_font(p: &Path) -> Option<FontRef<'static>> {
    use std::os::fd::AsRawFd;
    let file = std::fs::File::open(p).ok()?;
    let len = file.metadata().ok()?.len() as usize;
    if len == 0 {
        return None;
    }
    // SAFETY: read-only private mapping that lives for the whole process (fonts are never
    // unloaded); the descriptor may be closed after mmap.
    let ptr = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_PRIVATE, file.as_raw_fd(), 0) };
    if ptr == libc::MAP_FAILED {
        return None;
    }
    let data: &'static [u8] = unsafe { std::slice::from_raw_parts(ptr as *const u8, len) };
    FontRef::try_from_slice_and_index(data, 0).ok()
}

impl Fonts {
    pub fn load(family: &str) -> Result<Fonts, String> {
        let regular = fc_match(&format!("{family}:style=Regular:spacing=mono"))
            .filter(|p| fc_spacing_mono(p))
            .or_else(|| fc_match("monospace:spacing=mono").filter(|p| fc_spacing_mono(p)))
            .or_else(|| scan("DejaVuSansMono.ttf"))
            .or_else(|| scan("JetBrainsMonoNerdFontMono-Regular.ttf"))
            .ok_or_else(|| "no monospace font found (install ttf-jetbrains-mono-nerd or ttf-dejavu)".to_string())?;
        let bold = fc_match(&format!("{family}:style=Bold:spacing=mono")).filter(|p| p != &regular && fc_spacing_mono(p));
        let mut fallbacks = vec![];
        for pat in ["Symbols Nerd Font", "Noto Sans Symbols 2", "Noto Sans CJK JP", "DejaVu Sans"] {
            if let Some(p) = fc_match(pat) {
                if p != regular && !fallbacks.iter().any(|f: &Face| f.path == p) {
                    fallbacks.push(Face::new(p));
                }
            }
        }
        let mut f = Fonts { regular: Face::new(regular), bold: bold.map(Face::new), fallbacks, cache: HashMap::new() };
        if f.regular.get().is_none() {
            return Err(format!("cannot read font {}", f.regular.path.display()));
        }
        Ok(f)
    }

    pub fn primary_path(&self) -> &Path {
        &self.regular.path
    }

    /// (ascent, line height) in device pixels for a font size of `px` pixels per em.
    pub fn metrics(&mut self, px: f32) -> (i32, i32) {
        let Some(f) = self.regular.get() else { return ((px * 0.8) as i32, (px * 1.2) as i32) };
        let Some(scale) = f.pt_to_px_scale(px) else { return ((px * 0.8) as i32, (px * 1.2) as i32) };
        let sf = f.as_scaled(scale);
        (sf.ascent().round() as i32, (sf.ascent() - sf.descent() + sf.line_gap()).ceil() as i32)
    }

    pub fn glyph(&mut self, c: char, px: f32, bold: bool) -> &Glyph {
        let key = (c, px.to_bits(), bold);
        if !self.cache.contains_key(&key) {
            if self.cache.len() > 4096 {
                self.cache.clear();
            }
            let g = self.raster(c, px, bold);
            self.cache.insert(key, g);
        }
        &self.cache[&key]
    }

    fn raster(&mut self, c: char, px: f32, bold: bool) -> Glyph {
        let primary_has = |f: &mut Face| f.get().is_some_and(|f| f.glyph_id(c).0 != 0);
        let face: &mut Face = if bold && self.bold.as_mut().is_some_and(primary_has) {
            self.bold.as_mut().unwrap()
        } else if primary_has(&mut self.regular) || c == ' ' {
            &mut self.regular
        } else {
            let idx = self.fallbacks.iter_mut().position(|f| f.get().is_some_and(|f| f.glyph_id(c).0 != 0));
            match idx {
                Some(i) => &mut self.fallbacks[i],
                None => &mut self.regular,
            }
        };
        let empty = |adv| Glyph { left: 0, top: 0, w: 0, h: 0, adv, bmp: vec![] };
        let Some(f) = face.get() else { return empty(px * 0.6) };
        let Some(scale) = f.pt_to_px_scale(px) else { return empty(px * 0.6) };
        let id = f.glyph_id(c);
        let adv = f.as_scaled(scale).h_advance(id);
        let Some(og) = f.outline_glyph(id.with_scale_and_position(scale, ab_glyph::point(0.0, 0.0))) else { return empty(adv) };
        let bb = og.px_bounds();
        let (w, h) = (bb.width().ceil() as usize, bb.height().ceil() as usize);
        let mut bmp = vec![0u8; w * h];
        og.draw(|x, y, cov| {
            if let Some(p) = bmp.get_mut(y as usize * w + x as usize) {
                *p = (cov.clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        });
        Glyph { left: bb.min.x.floor() as i32, top: bb.min.y.floor() as i32, w, h, adv, bmp }
    }

    /// Advance width of `s` in device pixels.
    pub fn width(&mut self, s: &str, px: f32, bold: bool) -> i32 {
        s.chars().map(|c| self.glyph(c, px, bold).adv).sum::<f32>().round() as i32
    }

    /// Longest prefix of `s` that fits in `max` device pixels, with an ellipsis if cut.
    pub fn fit(&mut self, s: &str, px: f32, bold: bool, max: i32) -> String {
        if self.width(s, px, bold) <= max {
            return s.to_string();
        }
        let ell = self.width("…", px, bold);
        let mut out = String::new();
        let mut w = 0.0;
        for c in s.chars() {
            let a = self.glyph(c, px, bold).adv;
            if (w + a).round() as i32 + ell > max {
                break;
            }
            w += a;
            out.push(c);
        }
        out.push('…');
        out
    }
}
