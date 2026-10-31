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

/// Standard app chrome: obsidian background, amber border + corner brackets, title strip.
/// Returns the content top (device px) and the inner padding.
pub fn chrome(f: &mut Frame, title: &str, status: &str) -> (i32, i32) {
    let (w, h) = (f.cv.w as i32, f.cv.h as i32);
    let th = f.theme.clone();
    // app windows are always opaque: whatever is behind them must never show through
    // ([look] opacity applies to the floating side bar only)
    f.cv.clear(th.bg, 1.0);
    let t = f.hair();
    f.cv.border(0, 0, w, h, t, th.amber, if f.focused { 0.9 } else { 0.35 });
    f.cv.brackets(f.px(6.0), f.px(6.0), w - 2 * f.px(6.0), h - 2 * f.px(6.0), f.px(14.0), f.px(2.0), th.amber, 0.9);
    let pad = f.px(18.0);
    let pt = th.font_pt;
    let lh = f.line_h(pt);
    let bar_h = lh + f.px(8.0);
    f.cv.fill(pad, pad, w - 2 * pad, bar_h, th.panel, 1.0);
    let tag = format!(" ◢ {title} ");
    let tw = f.text_w(&tag, pt, true);
    f.cv.fill(pad, pad, tw, bar_h, th.amber, 1.0);
    f.text(pad, pad + f.px(4.0), &tag, pt, th.bg, true);
    if !status.is_empty() {
        let max = w - 2 * pad - tw - f.px(16.0);
        let s = f.fit(status, pt, false, max);
        let sw = f.text_w(&s, pt, false);
        f.text(w - pad - sw - f.px(8.0), pad + f.px(4.0), &s, pt, th.cyan, false);
    }
    f.cv.fill(pad, pad + bar_h, w - 2 * pad, t, th.amber, 0.8);
    (pad + bar_h + f.px(10.0), pad)
}

/// Footer key hints in dim text along the bottom edge.
pub fn footer(f: &mut Frame, hints: &str) {
    let th = f.theme.clone();
    let pad = f.px(18.0);
    let pt = th.font_pt * 0.9;
    let lh = f.line_h(pt);
    let w = f.cv.w as i32;
    let y = f.cv.h as i32 - pad - lh;
    let s = f.fit(hints, pt, false, w - 2 * pad);
    f.text(pad, y, &s, pt, th.dim, false);
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

/// A push button: `[ label ]` with its key hint underlined in amber. `selected` = keyboard focus,
/// `enabled` = false greys it out. Returns its rectangle.
pub fn button(f: &mut Frame, x: i32, top: i32, label: &str, selected: bool, enabled: bool) -> Rect {
    let th = f.theme.clone();
    let pt = th.font_pt;
    let (padx, pady) = (f.px(12.0), f.px(5.0));
    let tw = f.text_w(label, pt, true);
    let r = Rect { x, y: top, w: tw + 2 * padx, h: f.line_h(pt) + 2 * pady };
    let t = f.hair();
    if selected && enabled {
        f.cv.fill(r.x, r.y, r.w, r.h, th.amber, 1.0);
        f.text(x + padx, top + pady, label, pt, th.bg, true);
    } else {
        f.cv.fill(r.x, r.y, r.w, r.h, th.panel, 1.0);
        f.cv.border(r.x, r.y, r.w, r.h, t, if enabled { th.amber } else { th.dim }, if enabled { 0.8 } else { 0.4 });
        f.text_a(x + padx, top + pady, label, pt, if enabled { th.text } else { th.dim }, true, if enabled { 1.0 } else { 0.6 });
    }
    r
}
