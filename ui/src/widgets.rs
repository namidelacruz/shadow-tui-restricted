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

// ── vertex wireframe glass ────────────────────────────────────────────────────────────────────

/// Corner points of a chamfered box: the top-left and bottom-right corners are cut by `c`, the
/// other two by a quarter of it — the asymmetric "angled glass" outline used everywhere.
pub fn chamfer_pts(x: i32, y: i32, w: i32, h: i32, c: i32) -> Vec<(f32, f32)> {
    let (x, y, w, h) = (x as f32, y as f32, w as f32, h as f32);
    let c = (c as f32).min(w / 3.0).min(h / 2.0).max(0.0);
    let s = (c / 4.0).round();
    vec![(x + c, y), (x + w - s, y), (x + w, y + s), (x + w, y + h - c), (x + w - c, y + h), (x + s, y + h), (x, y + h - s), (x, y + c)]
}

/// Glossy translucent glass in a chamfered outline: a vertical glass gradient, a white gloss band
/// fading over the top ~45 %, a thin wireframe edge in `accent` and glowing vertex nodes at the
/// cut corners. `alpha` scales the glass body (edges stay crisp).
pub fn glass(f: &mut Frame, x: i32, y: i32, w: i32, h: i32, cut: i32, accent: crate::Rgb, alpha: f32, nodes: bool) {
    if w <= 2 || h <= 2 {
        return;
    }
    let th = f.theme.clone();
    let pts = chamfer_pts(x, y, w, h, cut);
    let (hi, lo) = (th.glass, th.glass_lo);
    // one pass: the glass body, a white gloss fading out by 45 % of the height, and a faint accent
    // tint along the bottom, pre-composited per pixel (premultiplied "over", body first)
    f.cv.poly_ga(&pts, &|fx, fy| {
        let body = crate::canvas::lerp(hi, lo, fy);
        let ab = alpha;
        let ag = if fy < 0.45 { 0.10 * (1.0 - fy / 0.45) * (1.0 - fy / 0.45) * (1.0 - 0.35 * fx) } else { 0.0 };
        let at = if fy > 0.7 { 0.06 * (fy - 0.7) / 0.3 } else { 0.0 };
        let mut s = [body.0 as f32 * ab, body.1 as f32 * ab, body.2 as f32 * ab];
        let mut a = ab;
        for (c, k) in [((255.0, 255.0, 255.0), ag), ((accent.0 as f32, accent.1 as f32, accent.2 as f32), at)] {
            if k > 0.0 {
                s = [c.0 * k + s[0] * (1.0 - k), c.1 * k + s[1] * (1.0 - k), c.2 * k + s[2] * (1.0 - k)];
                a = k + a * (1.0 - k);
            }
        }
        if a <= 0.0 {
            return ((0, 0, 0), 0.0);
        }
        (((s[0] / a + 0.5) as u8, (s[1] / a + 0.5) as u8, (s[2] / a + 0.5) as u8), a)
    });
    f.cv.poly_line(&pts, accent, 0.75);
    // top edge highlight
    f.cv.wline(pts[0].0, pts[0].1 + 0.5, pts[1].0, pts[1].1 + 0.5, (255, 255, 255), 0.18);
    if nodes {
        let r = f.px(1.6) as f32;
        for &i in &[0usize, 3, 4, 7] {
            f.cv.node(pts[i].0, pts[i].1, r, accent, 0.9);
        }
    }
}

/// A faint lattice of vertex dots (window backgrounds).
pub fn lattice(f: &mut Frame, x: i32, y: i32, w: i32, h: i32, c: crate::Rgb, a: f32) {
    let step = f.px(28.0).max(8);
    let mut yy = y + step / 2;
    while yy < y + h {
        let mut xx = x + step / 2;
        while xx < x + w {
            f.cv.blend(xx, yy, 1, 1, c, a);
            xx += step;
        }
        yy += step;
    }
}

/// Standard app chrome — vertex wireframe glass: the window is translucent glass
/// ([look] window_opacity, 1.0 = opaque) with a dot lattice, a chamfered wireframe frame with
/// vertex nodes, an angled amber→violet title tag and a beam under the header.
/// Returns the content top (device px) and the inner padding.
pub fn chrome(f: &mut Frame, title: &str, status: &str) -> (i32, i32) {
    let op = f.theme.window_opacity;
    chrome_with(f, title, status, op)
}

/// `chrome` that is always fully opaque — for windows that must never let what is behind them
/// show through (the launcher and the updater).
pub fn chrome_solid(f: &mut Frame, title: &str, status: &str) -> (i32, i32) {
    chrome_with(f, title, status, 1.0)
}

fn chrome_with(f: &mut Frame, title: &str, status: &str, op: f32) -> (i32, i32) {
    let (w, h) = (f.cv.w as i32, f.cv.h as i32);
    let th = f.theme.clone();
    let op = op.clamp(0.4, 1.0);
    f.cv.clear(th.bg, op);
    f.cv.rrect_v(0, 0, w, h, 0.0, th.glass_lo, th.bg, op * 0.6);
    lattice(f, 0, 0, w, h, th.cyan, 0.10);
    f.cv.scanlines(0, 0, w, h, f.px(4.0).max(3), th.cyan, 0.012);
    let inset = f.px(6.0);
    let accent = if f.focused { th.cyan } else { th.dim };
    let cut = f.px(th.chamfer + 8.0);
    glass(f, inset, inset, w - 2 * inset, h - 2 * inset, cut, accent, 0.35, true);
    let pad = f.px(18.0);
    let pt = th.font_pt;
    let lh = f.line_h(pt);
    let bar_h = lh + f.px(10.0);
    let tag = format!("  {title}  ");
    let tw = f.text_w(&tag, pt, true) + f.px(14.0);
    let tp = chamfer_pts(pad + f.px(8.0), pad, tw, bar_h, bar_h / 2);
    let (a, v) = (th.amber, th.violet);
    f.cv.glow(pad + f.px(8.0), pad, tw, bar_h, f.px(3.0) as f32, f.px(6.0), th.amber, if f.focused { 0.20 } else { 0.08 });
    f.cv.poly(&tp, 1.0, &|fx, _| crate::canvas::lerp(a, v, fx));
    f.text(pad + f.px(8.0), pad + f.px(5.0), &tag, pt, th.bg, true);
    if !status.is_empty() {
        let max = w - 2 * pad - tw - f.px(32.0);
        let s = f.fit(status, pt * 0.92, false, max);
        let sw = f.text_w(&s, pt * 0.92, false);
        let (sx, sh) = (w - pad - sw - f.px(14.0), f.line_h(pt * 0.92) + f.px(6.0));
        let sy = pad + (bar_h - sh) / 2;
        glass(f, sx - f.px(8.0), sy, sw + f.px(22.0), sh, sh / 2, th.cyan, 0.8, false);
        f.text(sx + f.px(3.0), sy + f.px(3.0), &s, pt * 0.92, th.cyan, false);
    }
    let by = pad + bar_h + f.px(4.0);
    f.cv.beam(pad, by, w - 2 * pad, f.hair().max(1), th.amber, th.cyan, 0.8);
    f.cv.node(pad as f32, by as f32, f.px(1.6) as f32, th.amber, 0.9);
    f.cv.node((w - pad) as f32, by as f32, f.px(1.6) as f32, th.cyan, 0.9);
    (pad + bar_h + f.px(14.0), pad)
}

/// Footer key hints: dim text over a wire with vertex nodes at its ends.
pub fn footer(f: &mut Frame, hints: &str) {
    let th = f.theme.clone();
    let pad = f.px(18.0);
    let pt = th.font_pt * 0.88;
    let lh = f.line_h(pt);
    let w = f.cv.w as i32;
    let y = f.cv.h as i32 - pad - lh;
    let s = f.fit(hints, pt, false, w - 2 * pad - f.px(16.0));
    let wy = (y - f.px(6.0)) as f32;
    f.cv.wline((pad + f.px(10.0)) as f32, wy, (w - pad - f.px(10.0)) as f32, wy, th.cyan, 0.30);
    f.cv.node((pad + f.px(10.0)) as f32, wy, f.px(1.2) as f32, th.cyan, 0.7);
    f.cv.node((w - pad - f.px(10.0)) as f32, wy, f.px(1.2) as f32, th.violet, 0.7);
    f.text(pad + f.px(8.0), y, &s, pt, th.dim, false);
}

/// Highlight for the selected row of a list: an angled amber glass slab with a vertex marker.
pub fn selection(f: &mut Frame, x: i32, y: i32, w: i32, h: i32) {
    let th = f.theme.clone();
    let pts = chamfer_pts(x, y, w, h, (h / 2).min(f.px(th.chamfer)));
    let (g, a, lo) = (th.glass, th.amber, th.glass_lo);
    f.cv.poly(&pts, 0.95, &|fx, _| crate::canvas::lerp(crate::canvas::lerp(g, a, 0.26), crate::canvas::lerp(lo, a, 0.04), fx));
    f.cv.poly_ga(&pts, &|_, fy| ((255, 255, 255), if fy < 0.5 { 0.07 * (1.0 - fy * 2.0) } else { 0.0 }));
    f.cv.poly_line(&pts, th.amber, 0.45);
    f.cv.node(x as f32 + f.px(7.0) as f32, (y + h / 2) as f32, f.px(2.0) as f32, th.amber, 1.0);
}

/// A text field background: chamfered glass, cyan wire (brighter + noded when focused).
pub fn field(f: &mut Frame, x: i32, y: i32, w: i32, h: i32, focused: bool) {
    let th = f.theme.clone();
    if focused {
        f.cv.glow(x, y, w, h, 2.0, f.px(5.0), th.cyan, 0.10);
    }
    glass(f, x, y, w, h, (h / 2).min(f.px(th.chamfer)), if focused { th.cyan } else { crate::canvas::lerp(th.dim, th.cyan, 0.3) }, 0.9, focused);
}

/// A chamfered glass panel (cards, dialogs).
pub fn panel(f: &mut Frame, x: i32, y: i32, w: i32, h: i32, accent: crate::Rgb) {
    let th = f.theme.clone();
    f.cv.glow(x, y, w, h, 2.0, f.px(6.0), accent, 0.08);
    glass(f, x, y, w, h, f.px(th.chamfer), accent, 0.85, true);
}

/// A horizontal meter (0..=1): a wire track with segmented cells that light up (cyan → amber →
/// crimson as it fills when `hot`, cyan → violet otherwise) and a vertex node at the level.
pub fn meter(f: &mut Frame, x: i32, y: i32, w: i32, h: i32, v: f32, hot: bool) {
    let th = f.theme.clone();
    let v = v.clamp(0.0, 1.0);
    if w <= 4 || h <= 1 {
        return;
    }
    let col = |t: f32| {
        if !hot {
            crate::canvas::lerp(th.cyan, th.violet, t)
        } else if t < 0.6 {
            crate::canvas::lerp(th.cyan, th.amber, t / 0.6)
        } else {
            crate::canvas::lerp(th.amber, th.crimson, (t - 0.6) / 0.4)
        }
    };
    let skew = (h as f32 * 0.5).max(1.0);
    let track = [(x as f32 + skew, y as f32), ((x + w) as f32, y as f32), ((x + w) as f32 - skew, (y + h) as f32), (x as f32, (y + h) as f32)];
    f.cv.poly(&track, 0.9, &|_, _| th.glass);
    f.cv.poly_line(&track, th.dim, 0.35);
    let cell = f.px(6.0).max(3);
    let gap = f.px(2.0).max(1);
    let fill_w = (w as f32 * v).round() as i32;
    let mut cx = x + gap;
    while cx + cell <= x + fill_w.max(0) && cx + cell <= x + w - gap {
        let t = (cx - x) as f32 / w as f32;
        let c = col(t);
        let (x0, x1) = (cx as f32, (cx + cell) as f32);
        let (t0, t1) = ((y + gap) as f32, (y + h - gap) as f32);
        let sk = skew * 0.6;
        f.cv.poly(&[(x0 + sk, t0), (x1 + sk, t0), (x1, t1), (x0, t1)], 0.95, &|_, _| c);
        cx += cell + gap;
    }
    if fill_w > 0 {
        let end = col(v);
        f.cv.glow(x, y, fill_w.max(h), h, 2.0, f.px(3.0), end, 0.16);
        f.cv.node((x + fill_w) as f32, (y + h / 2) as f32, f.px(1.6) as f32, end, 0.95);
    }
}

/// A sparkline of values in 0..=1 (oldest first) inside (x, y, w, h): a faint filled area under
/// an anti-aliased wire, with a vertex node on the newest value.
pub fn sparkline(f: &mut Frame, x: i32, y: i32, w: i32, h: i32, vals: &[f32], c: crate::Rgb) {
    if vals.len() < 2 || w <= 1 || h <= 0 {
        return;
    }
    let n = vals.len();
    let pt = |i: usize| -> (f32, f32) {
        let v = vals[i].clamp(0.0, 1.0);
        (x as f32 + (w - 1) as f32 * i as f32 / (n - 1) as f32, (y + h) as f32 - v * (h - 1) as f32 - 0.5)
    };
    for i in 0..n - 1 {
        let (a, b) = (pt(i), pt(i + 1));
        let base = (y + h) as f32;
        f.cv.poly_ga(&[a, b, (b.0, base), (a.0, base)], &|_, fy| (c, 0.04 + 0.16 * (1.0 - fy)));
        f.cv.wline(a.0, a.1, b.0, b.1, c, 0.95);
    }
    let last = pt(n - 1);
    f.cv.node(last.0, last.1, f.px(1.6) as f32, c, 1.0);
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

/// A push button: a chamfered glass slab — amber → violet and glowing when selected, glass with
/// an accent wire otherwise; `enabled` = false greys it out. Returns its rectangle.
pub fn button(f: &mut Frame, x: i32, top: i32, label: &str, selected: bool, enabled: bool) -> Rect {
    let th = f.theme.clone();
    let pt = th.font_pt;
    let (padx, pady) = (f.px(14.0), f.px(6.0));
    let tw = f.text_w(label, pt, true);
    let r = Rect { x, y: top, w: tw + 2 * padx, h: f.line_h(pt) + 2 * pady };
    let cut = r.h / 2;
    if selected && enabled {
        let pts = chamfer_pts(r.x, r.y, r.w, r.h, cut);
        let (a, v) = (th.amber, crate::canvas::lerp(th.amber, th.violet, 0.6));
        f.cv.glow(r.x, r.y, r.w, r.h, 2.0, f.px(6.0), th.amber, 0.26);
        f.cv.poly(&pts, 1.0, &|fx, fy| crate::canvas::lerp(a, v, (fx * 0.7 + fy * 0.3).min(1.0)));
        f.cv.poly_ga(&pts, &|_, fy| ((255, 255, 255), if fy < 0.5 { 0.22 * (1.0 - fy * 2.0) } else { 0.0 }));
        let r0 = f.px(1.6) as f32;
        f.cv.node(pts[0].0, pts[0].1, r0, (255, 240, 200), 0.9);
        f.cv.node(pts[4].0, pts[4].1, r0, th.violet, 0.9);
        f.text(x + padx, top + pady, label, pt, th.bg, true);
    } else {
        glass(f, r.x, r.y, r.w, r.h, cut, if enabled { th.amber } else { th.dim }, if enabled { 0.95 } else { 0.6 }, false);
        f.text_a(x + padx, top + pady, label, pt, if enabled { th.text } else { th.dim }, true, if enabled { 1.0 } else { 0.6 });
    }
    r
}
