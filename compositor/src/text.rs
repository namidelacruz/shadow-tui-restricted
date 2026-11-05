//! Text for the HUD, ring labels, greeter and lock screen: fontdue rasterises a line into an
//! ARGB8888 buffer once; the buffer is only rebuilt when its text changes (keeps idle damage at 0).
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::utils::Transform;
use std::collections::HashMap;

pub struct Text {
    font: Option<fontdue::Font>,
    /// Shadow Glyph, for lines whose key starts with "d:" (titles, the login screen)
    display: Option<fontdue::Font>,
    /// Japanese (and other CJK) glyphs the faces above don't have: loaded on first use only
    cjk: Option<Option<fontdue::Font>>,
    cache: HashMap<String, (String, u32, MemoryRenderBuffer, (i32, i32))>,
}

fn find_font(pref: &str) -> Option<Vec<u8>> {
    let mut cands: Vec<String> = vec![];
    if !pref.is_empty() {
        cands.push(pref.into());
    }
    // fc-match always answers with *something*; only accept monospace matches
    for q in ["JetBrainsMono Nerd Font Mono", "JetBrains Mono", "monospace"] {
        if let Ok(o) = std::process::Command::new("fc-match").args(["-f", "%{file}|%{spacing}", q]).output() {
            let out = String::from_utf8_lossy(&o.stdout).trim().to_string();
            let (f, spacing) = out.split_once('|').unwrap_or((&out, ""));
            if !f.is_empty() && (spacing == "100" || f.to_lowercase().contains("mono")) {
                cands.push(f.to_string());
            }
        }
    }
    cands.extend(["/usr/share/fonts/TTF/JetBrainsMonoNerdFontMono-Regular.ttf", "/usr/share/fonts/TTF/DejaVuSansMono.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf", "/usr/share/fonts/noto/NotoSansMono-Regular.ttf"].map(String::from));
    cands.iter().find_map(|p| std::fs::read(p).ok())
}

impl Text {
    pub fn new(pref: &str) -> Text {
        let font = find_font(pref).and_then(|b| fontdue::Font::from_bytes(b, fontdue::FontSettings::default()).ok());
        if font.is_none() {
            tracing::warn!("no font found: HUD text disabled");
        }
        let display = ["/opt/shadow-tui/share/fonts/ShadowGlyph-Regular.ttf".to_string(), format!("{}/.local/share/fonts/ShadowGlyph-Regular.ttf", std::env::var("HOME").unwrap_or_default()), "/usr/share/fonts/shadow/ShadowGlyph-Regular.ttf".into()]
            .iter()
            .chain(std::env::var("SHADOW_DISPLAY_FONT").ok().iter())
            .find_map(|p| std::fs::read(p).ok())
            .and_then(|b| fontdue::Font::from_bytes(b, fontdue::FontSettings::default()).ok());
        Text { font, display, cjk: None, cache: HashMap::new() }
    }

    /// the CJK fallback face (fontconfig's choice for Japanese), loaded once, on demand
    fn cjk(&mut self) -> Option<&fontdue::Font> {
        if self.cjk.is_none() {
            let f = std::process::Command::new("fc-match").args(["-f", "%{file}|%{index}", "sans:lang=ja"]).output().ok().and_then(|o| {
                let out = String::from_utf8_lossy(&o.stdout).trim().to_string();
                let (p, i) = out.split_once('|')?;
                let bytes = std::fs::read(p).ok()?;
                fontdue::Font::from_bytes(bytes, fontdue::FontSettings { collection_index: i.parse().unwrap_or(0), ..Default::default() }).ok()
            });
            if f.is_none() {
                tracing::info!("no Japanese font found (install google-noto-sans-cjk-fonts / noto-fonts-cjk): CJK text shows as boxes");
            }
            self.cjk = Some(f);
        }
        self.cjk.as_ref().and_then(|f| f.as_ref())
    }

    fn needs_cjk(font: &fontdue::Font, text: &str) -> bool {
        text.chars().any(|c| (c as u32) >= 0x2E80 && font.lookup_glyph_index(c) == 0)
    }

    /// Rasterise `text` at `px` (physical) in `rgb`; returns the buffer and its physical size.
    /// `key` identifies the slot (e.g. "hud-top"), so a changed string reuses the slot.
    /// A line of text as a buffer, cached under `key`. Keys starting with "d:" use the display face.
    pub fn line(&mut self, key: &str, text: &str, px: f32, rgb: [f32; 3]) -> Option<(MemoryRenderBuffer, (i32, i32))> {
        let col = ((rgb[0] * 255.0) as u32) << 16 | ((rgb[1] * 255.0) as u32) << 8 | (rgb[2] * 255.0) as u32;
        if let Some((t, c, b, s)) = self.cache.get(key) {
            if t == text && *c == col {
                return Some((b.clone(), *s));
            }
        }
        let want_cjk = {
            let font = if key.starts_with("d:") { self.display.as_ref().or(self.font.as_ref())? } else { self.font.as_ref()? };
            Self::needs_cjk(font, text)
        };
        if want_cjk {
            self.cjk();
        }
        let font = if key.starts_with("d:") { self.display.as_ref().or(self.font.as_ref())? } else { self.font.as_ref()? };
        let cjk = self.cjk.as_ref().and_then(|f| f.as_ref());
        let lm = font.horizontal_line_metrics(px)?;
        let h = (lm.ascent - lm.descent).ceil().max(1.0) as i32 + 2;
        // each character from the first face that has it (Japanese from the CJK face)
        let glyphs: Vec<(fontdue::Metrics, Vec<u8>)> = text.chars().map(|ch| match cjk {
            Some(c) if (ch as u32) >= 0x2E80 && font.lookup_glyph_index(ch) == 0 => c.rasterize(ch, px * 0.92),
            _ => font.rasterize(ch, px),
        }).collect();
        let w = glyphs.iter().map(|(m, _)| m.advance_width.round() as i32).sum::<i32>().max(1) + 2;
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let base = lm.ascent.round() as i32 + 1;
        let mut x = 1;
        for (m, cov) in &glyphs {
            let gx = x + m.xmin;
            let gy = base - m.height as i32 - m.ymin;
            for row in 0..m.height as i32 {
                for colx in 0..m.width as i32 {
                    let a = cov[(row * m.width as i32 + colx) as usize] as u32;
                    let (px_, py) = (gx + colx, gy + row);
                    if a == 0 || px_ < 0 || py < 0 || px_ >= w || py >= h {
                        continue;
                    }
                    let i = ((py * w + px_) * 4) as usize;
                    // premultiplied ARGB8888, little endian = B G R A
                    buf[i] = ((col & 0xff) * a / 255) as u8;
                    buf[i + 1] = (((col >> 8) & 0xff) * a / 255) as u8;
                    buf[i + 2] = (((col >> 16) & 0xff) * a / 255) as u8;
                    buf[i + 3] = a as u8;
                }
            }
            x += m.advance_width.round() as i32;
        }
        let b = MemoryRenderBuffer::from_slice(&buf, Fourcc::Argb8888, (w, h), 1, Transform::Normal, None);
        self.cache.insert(key.into(), (text.into(), col, b.clone(), (w, h)));
        Some((b, (w, h)))
    }

    /// width in the display face
    pub fn width_d(&self, text: &str, px: f32) -> i32 {
        self.display.as_ref().or(self.font.as_ref()).map(|f| self.measure(f, text, px)).unwrap_or(0)
    }
    pub fn width(&self, text: &str, px: f32) -> i32 {
        self.font.as_ref().map(|f| self.measure(f, text, px)).unwrap_or(0)
    }
    fn measure(&self, f: &fontdue::Font, text: &str, px: f32) -> i32 {
        let cjk = self.cjk.as_ref().and_then(|c| c.as_ref());
        text.chars().map(|c| match cjk {
            Some(k) if (c as u32) >= 0x2E80 && f.lookup_glyph_index(c) == 0 => k.metrics(c, px * 0.92).advance_width.round() as i32,
            _ => f.metrics(c, px).advance_width.round() as i32,
        }).sum::<i32>() + 2
    }

    /// memory pressure: drop every cached line (they are rebuilt on demand)
    pub fn evict(&mut self) {
        self.cache.clear();
    }
    pub fn forget(&mut self, key_prefix: &str) {
        self.cache.retain(|k, _| !k.starts_with(key_prefix));
    }
}
