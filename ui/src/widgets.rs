//! Small keyboard-first building blocks shared by the apps.
use crate::shell::{Frame, Key};
use crate::Keysym;

/// Single-line text input with a cursor.
#[derive(Default, Clone)]
pub struct Input {
    pub text: String,
    pub cur: usize, // char index
    pub secret: bool,
}

impl Input {
    pub fn secret() -> Input {
        Input { secret: true, ..Default::default() }
    }
    pub fn set(&mut self, s: &str) {
        self.text = s.to_string();
        self.cur = self.text.chars().count();
    }
    pub fn clear(&mut self) {
        self.text.clear();
        self.cur = 0;
    }
    fn byte(&self, ci: usize) -> usize {
        self.text.char_indices().nth(ci).map(|(b, _)| b).unwrap_or(self.text.len())
    }
    /// Returns true if the key was consumed (text or cursor changed).
    pub fn key(&mut self, k: &Key) -> bool {
        let n = self.text.chars().count();
        match k.sym {
            Keysym::BackSpace if k.ctrl => {
                let b = self.byte(self.cur);
                let head = self.text[..b].trim_end();
                let cut = head.rfind(|c: char| c.is_whitespace() || c == '/').map(|i| i + 1).unwrap_or(0);
                let removed = self.text[cut..b].chars().count();
                self.text.replace_range(cut..b, "");
                self.cur -= removed;
                true
            }
            Keysym::BackSpace if self.cur > 0 => {
                let (a, b) = (self.byte(self.cur - 1), self.byte(self.cur));
                self.text.replace_range(a..b, "");
                self.cur -= 1;
                true
            }
            Keysym::Delete if self.cur < n => {
                let (a, b) = (self.byte(self.cur), self.byte(self.cur + 1));
                self.text.replace_range(a..b, "");
                true
            }
            Keysym::Left if self.cur > 0 => {
                self.cur -= 1;
                true
            }
            Keysym::Right if self.cur < n => {
                self.cur += 1;
                true
            }
            Keysym::Home => {
                self.cur = 0;
                true
            }
            Keysym::End => {
                self.cur = n;
                true
            }
            Keysym::u if k.ctrl => {
                self.clear();
                true
            }
            _ => match k.typed() {
                Some(t) => {
                    let b = self.byte(self.cur);
                    self.text.insert_str(b, t);
                    self.cur += t.chars().count();
                    true
                }
                None => false,
            },
        }
    }
    pub fn shown(&self) -> String {
        if self.secret {
            "•".repeat(self.text.chars().count())
        } else {
            self.text.clone()
        }
    }
    /// Draw at (x, top); returns width. The cursor is a solid bar (no blinking: nothing animates).
    pub fn draw(&self, f: &mut Frame, x: i32, top: i32, pt: f32, focused: bool) -> i32 {
        let shown = self.shown();
        let col = f.theme.text;
        let w = f.text(x, top, &shown, pt, col, false);
        if focused {
            let pre: String = shown.chars().take(self.cur).collect();
            let cx = x + f.text_w(&pre, pt, false);
            let lh = f.line_h(pt);
            let amber = f.theme.amber;
            let t = f.hair().max(f.px(2.0));
            f.cv.fill(cx, top, t, lh, amber, 1.0);
        }
        w
    }
}

/// Selection + scrolling over a list of `len` rows, `visible` at a time.
#[derive(Default, Clone, Copy)]
pub struct ListNav {
    pub sel: usize,
    pub top: usize,
}

impl ListNav {
    pub fn clamp(&mut self, len: usize, visible: usize) {
        if len == 0 {
            self.sel = 0;
            self.top = 0;
            return;
        }
        self.sel = self.sel.min(len - 1);
        let visible = visible.max(1);
        if self.sel < self.top {
            self.top = self.sel;
        }
        if self.sel >= self.top + visible {
            self.top = self.sel + 1 - visible;
        }
        self.top = self.top.min(len.saturating_sub(visible));
    }
    /// Up/Down/j/k/PgUp/PgDn/Home/End. Returns true if consumed.
    pub fn key(&mut self, k: &Key, len: usize, visible: usize, vim: bool) -> bool {
        let page = visible.max(1);
        let before = self.sel;
        match k.sym {
            Keysym::Up => self.sel = self.sel.saturating_sub(1),
            Keysym::Down => self.sel += 1,
            Keysym::k if vim && !k.ctrl => self.sel = self.sel.saturating_sub(1),
            Keysym::j if vim && !k.ctrl => self.sel += 1,
            Keysym::Page_Up => self.sel = self.sel.saturating_sub(page),
            Keysym::Page_Down => self.sel += page,
            Keysym::Home => self.sel = 0,
            Keysym::End => self.sel = len.saturating_sub(1),
            Keysym::g if vim && !k.shift => self.sel = 0,
            Keysym::G if vim => self.sel = len.saturating_sub(1),
            _ => return false,
        }
        self.clamp(len, visible);
        let _ = before;
        true
    }
}

/// Standard app chrome — "fluid HUD": a deep gradient backdrop with faint scan lines, a rounded
/// glass frame with a soft accent glow, an angled amber→violet title tag and a fading light beam
/// under the header. The window itself stays fully opaque.
/// Returns the content top (device px) and the inner padding.
pub fn chrome(f: &mut Frame, title: &str, status: &str) -> (i32, i32) {
    let (w, h) = (f.cv.w as i32, f.cv.h as i32);
    let th = f.theme.clone();
    // app windows are always opaque: whatever is behind them must never show through
    // ([look] opacity applies to the floating side bar only)
    f.cv.clear(th.bg, 1.0);
    f.cv.rrect_v(0, 0, w, h, 0.0, th.glass_lo, th.bg, 1.0);
    f.cv.scanlines(0, 0, w, h, f.px(4.0).max(3), th.cyan, 0.018);
    let r = f.px(th.radius) as f32;
    let inset = f.px(6.0);
    let accent = if f.focused { th.amber } else { th.dim };
    f.cv.glow(inset, inset, w - 2 * inset, h - 2 * inset, r, f.px(5.0), accent, if f.focused { 0.10 } else { 0.04 });
    f.cv.rrect_v(inset, inset, w - 2 * inset, h - 2 * inset, r, th.glass, th.glass_lo, 0.55);
    f.cv.rstroke(inset, inset, w - 2 * inset, h - 2 * inset, r, f.hair(), accent, if f.focused { 0.55 } else { 0.25 });
    let pad = f.px(18.0);
    let pt = th.font_pt;
    let lh = f.line_h(pt);
    let bar_h = lh + f.px(10.0);
    let tag = format!("  {title}  ");
    let tw = f.text_w(&tag, pt, true) + f.px(14.0);
    f.cv.glow(pad, pad, tw - f.px(8.0), bar_h, f.px(3.0) as f32, f.px(6.0), th.amber, 0.18);
    f.cv.tag(pad, pad, tw, bar_h, th.amber, th.violet, 1.0);
    f.text(pad, pad + f.px(5.0), &tag, pt, th.bg, true);
    if !status.is_empty() {
        let max = w - 2 * pad - tw - f.px(24.0);
        let s = f.fit(status, pt * 0.92, false, max);
        let sw = f.text_w(&s, pt * 0.92, false);
        let (sx, sh) = (w - pad - sw - f.px(14.0), f.line_h(pt * 0.92) + f.px(6.0));
        let sy = pad + (bar_h - sh) / 2;
        f.cv.rrect(sx - f.px(4.0), sy, sw + f.px(18.0), sh, sh as f32 / 2.0, th.glass, 0.9);
        f.cv.rstroke(sx - f.px(4.0), sy, sw + f.px(18.0), sh, sh as f32 / 2.0, f.hair(), th.cyan, 0.45);
        f.text(sx + f.px(5.0), sy + f.px(3.0), &s, pt * 0.92, th.cyan, false);
    }
    f.cv.beam(pad, pad + bar_h + f.px(4.0), w - 2 * pad, f.hair().max(1), th.amber, th.cyan, 0.8);
    (pad + bar_h + f.px(14.0), pad)
}

/// Footer key hints: dim text on a faint glass strip along the bottom edge.
pub fn footer(f: &mut Frame, hints: &str) {
    let th = f.theme.clone();
    let pad = f.px(18.0);
    let pt = th.font_pt * 0.88;
    let lh = f.line_h(pt);
    let w = f.cv.w as i32;
    let y = f.cv.h as i32 - pad - lh;
    let s = f.fit(hints, pt, false, w - 2 * pad - f.px(16.0));
    f.cv.beam(pad, y - f.px(6.0), w - 2 * pad, 1, th.cyan, th.violet, 0.35);
    f.text(pad + f.px(8.0), y, &s, pt, th.dim, false);
}

/// Highlight for the selected row of a list: a soft rounded gradient with a glowing marker.
pub fn selection(f: &mut Frame, x: i32, y: i32, w: i32, h: i32) {
    let th = f.theme.clone();
    let r = (h as f32 / 2.0).min(f.px(th.radius * 0.75) as f32);
    f.cv.rrect_v(x, y, w, h, r, crate::canvas::lerp(th.glass, th.amber, 0.22), crate::canvas::lerp(th.glass_lo, th.amber, 0.10), 0.95);
    f.cv.rstroke(x, y, w, h, r, f.hair(), th.amber, 0.35);
    let mh = (h - f.px(10.0)).max(f.px(4.0));
    f.cv.glow(x + f.px(5.0), y + (h - mh) / 2, f.px(3.0), mh, f.px(1.5) as f32, f.px(4.0), th.amber, 0.5);
    f.cv.rrect(x + f.px(5.0), y + (h - mh) / 2, f.px(3.0), mh, f.px(1.5) as f32, th.amber, 1.0);
}

/// A text field background: rounded glass, cyan edge (brighter when focused).
pub fn field(f: &mut Frame, x: i32, y: i32, w: i32, h: i32, focused: bool) {
    let th = f.theme.clone();
    let r = (h as f32 / 2.0).min(f.px(th.radius) as f32);
    if focused {
        f.cv.glow(x, y, w, h, r, f.px(5.0), th.cyan, 0.12);
    }
    f.cv.rrect_v(x, y, w, h, r, th.glass, th.glass_lo, 1.0);
    f.cv.rstroke(x, y, w, h, r, f.hair(), th.cyan, if focused { 0.7 } else { 0.25 });
}

/// A rounded panel (cards, dialogs).
pub fn panel(f: &mut Frame, x: i32, y: i32, w: i32, h: i32, accent: crate::Rgb) {
    let th = f.theme.clone();
    let r = f.px(th.radius) as f32;
    f.cv.glow(x, y, w, h, r, f.px(6.0), accent, 0.10);
    f.cv.rrect_v(x, y, w, h, r, th.glass, th.glass_lo, 1.0);
    f.cv.rstroke(x, y, w, h, r, f.hair(), accent, 0.5);
}

/// A horizontal meter (0..=1): rounded track with a gradient fill (cyan → amber → crimson as it
/// fills when `hot`, cyan → violet otherwise).
pub fn meter(f: &mut Frame, x: i32, y: i32, w: i32, h: i32, v: f32, hot: bool) {
    let th = f.theme.clone();
    let v = v.clamp(0.0, 1.0);
    let r = h as f32 / 2.0;
    f.cv.rrect(x, y, w, h, r, th.glass, 1.0);
    f.cv.rstroke(x, y, w, h, r, 1, th.dim, 0.25);
    let fw = ((w as f32) * v).round() as i32;
    if fw > 0 {
        let end = if !hot {
            th.violet
        } else if v < 0.6 {
            crate::canvas::lerp(th.cyan, th.amber, v / 0.6)
        } else {
            crate::canvas::lerp(th.amber, th.crimson, (v - 0.6) / 0.4)
        };
        let fw = fw.max(h.min(w));
        f.cv.glow(x, y, fw, h, r, f.px(3.0), end, 0.22);
        f.cv.rrect_h(x, y, fw, h, r, th.cyan, end, 0.95);
    }
}

/// A sparkline of values in 0..=1 (oldest first) inside (x, y, w, h).
pub fn sparkline(f: &mut Frame, x: i32, y: i32, w: i32, h: i32, vals: &[f32], c: crate::Rgb) {
    if vals.len() < 2 || w <= 0 || h <= 0 {
        return;
    }
    let n = vals.len();
    for col in 0..w {
        let i = col as usize * (n - 1) / (w.max(2) - 1) as usize;
        let v = vals[i.min(n - 1)].clamp(0.0, 1.0);
        let bh = ((h as f32) * v).round() as i32;
        if bh > 0 {
            // filled area with a brighter top edge
            f.cv.blend(x + col, y + h - bh, 1, bh, c, 0.18);
            f.cv.blend(x + col, y + h - bh, 1, f.px(1.5).max(1), c, 0.95);
        }
    }
}

/// A clickable rectangle in device pixels (store it during `draw`, test it in `App::click`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn hit(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
}

/// A push button: a pill with a gradient when selected (amber → violet, glowing), glass with an
/// accent edge otherwise; `enabled` = false greys it out. Returns its rectangle.
pub fn button(f: &mut Frame, x: i32, top: i32, label: &str, selected: bool, enabled: bool) -> Rect {
    let th = f.theme.clone();
    let pt = th.font_pt;
    let (padx, pady) = (f.px(14.0), f.px(6.0));
    let tw = f.text_w(label, pt, true);
    let r = Rect { x, y: top, w: tw + 2 * padx, h: f.line_h(pt) + 2 * pady };
    let rad = r.h as f32 / 2.0;
    let t = f.hair();
    if selected && enabled {
        f.cv.glow(r.x, r.y, r.w, r.h, rad, f.px(6.0), th.amber, 0.30);
        f.cv.rrect_v(r.x, r.y, r.w, r.h, rad, th.amber, crate::canvas::lerp(th.amber, th.violet, 0.55), 1.0);
        f.text(x + padx, top + pady, label, pt, th.bg, true);
    } else {
        f.cv.rrect_v(r.x, r.y, r.w, r.h, rad, th.glass, th.glass_lo, 1.0);
        f.cv.rstroke(r.x, r.y, r.w, r.h, rad, t, if enabled { th.amber } else { th.dim }, if enabled { 0.65 } else { 0.3 });
        f.text_a(x + padx, top + pady, label, pt, if enabled { th.text } else { th.dim }, true, if enabled { 1.0 } else { 0.6 });
    }
    r
}
