//! The app area (9.0): pure geometry and window order, no Wayland types.
//!
//! Shadow's widgets are fixed to the left and right edges of the screen (layer surfaces with
//! exclusive zones); the space between them is the app area. Open apps tile inside it — one fills
//! it, two sit side by side, three are a main tile and a stack, four a 2×2, more a grid. A newly
//! opened app takes the main tile. Minimised apps become square tiles in a scrollable grid: a band
//! along the bottom of the area while apps are open, the whole area when none are.
//! (The module keeps its 8.x name; the orbital ring it held is gone.)

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct R {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl R {
    pub fn lerp(a: R, b: R, t: f64) -> R {
        R { x: a.x + (b.x - a.x) * t, y: a.y + (b.y - a.y) * t, w: a.w + (b.w - a.w) * t, h: a.h + (b.h - a.h) * t }
    }
    pub fn inset(&self, m: f64) -> R {
        R { x: self.x + m, y: self.y + m, w: (self.w - 2.0 * m).max(1.0), h: (self.h - 2.0 * m).max(1.0) }
    }
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// Title bar height over every window (10.0), logical px.
pub const TITLE_H: f64 = 30.0;
/// a title bar button's width (as on Windows 11)
pub const TITLE_BTN_W: f64 = 46.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TitleBtn {
    Bar,
    Min,
    Max,
    Close,
}

/// The title bar above `content`, and its buttons right to left: close, maximise, minimise.
pub fn title_bar(content: R) -> (R, [(TitleBtn, R); 3]) {
    let t = R { x: content.x, y: content.y - TITLE_H, w: content.w, h: TITLE_H };
    let w = TITLE_BTN_W.min((t.w / 4.0).max(16.0));
    let b = |i: f64| R { x: t.x + t.w - w * (i + 1.0), y: t.y, w, h: t.h };
    (t, [(TitleBtn::Close, b(0.0)), (TitleBtn::Max, b(1.0)), (TitleBtn::Min, b(2.0))])
}

/// what of a title bar is at (x, y)
pub fn title_hit(content: R, x: f64, y: f64) -> Option<TitleBtn> {
    let (t, btns) = title_bar(content);
    if !t.contains(x, y) {
        return None;
    }
    Some(btns.iter().find(|(_, r)| r.contains(x, y)).map(|(b, _)| *b).unwrap_or(TitleBtn::Bar))
}

/// cubic-bezier(0.2, 0.0, 0.0, 1.0) — fast start, soft landing
pub fn ease(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    let (x1, y1, x2, y2) = (0.2, 0.0, 0.0, 1.0);
    let bez = |s: f64, a: f64, b: f64| 3.0 * (1.0 - s) * (1.0 - s) * s * a + 3.0 * (1.0 - s) * s * s * b + s * s * s;
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..30 {
        let mid = (lo + hi) / 2.0;
        if bez(mid, x1, x2) < t { lo = mid } else { hi = mid }
    }
    bez((lo + hi) / 2.0, y1, y2)
}

/// Rects for `n` open apps in `area`. `main` is the main tile's share of the width (2–3 apps).
pub fn tile(area: R, n: usize, gap: f64, main: f64) -> Vec<R> {
    let a = area;
    let g = gap;
    let main = main.clamp(0.3, 0.75);
    match n {
        0 => vec![],
        1 => vec![a],
        2 => {
            let w0 = ((a.w - g) * main).round();
            vec![R { x: a.x, y: a.y, w: w0, h: a.h }, R { x: a.x + w0 + g, y: a.y, w: a.w - w0 - g, h: a.h }]
        }
        3 => {
            let w0 = ((a.w - g) * main).round();
            let h1 = ((a.h - g) / 2.0).round();
            let x1 = a.x + w0 + g;
            let w1 = a.w - w0 - g;
            vec![R { x: a.x, y: a.y, w: w0, h: a.h }, R { x: x1, y: a.y, w: w1, h: h1 }, R { x: x1, y: a.y + h1 + g, w: w1, h: a.h - h1 - g }]
        }
        _ => {
            let cols = (n as f64).sqrt().ceil() as usize;
            let rows = n.div_ceil(cols);
            let cw = (a.w - g * (cols - 1) as f64) / cols as f64;
            let rh = (a.h - g * (rows - 1) as f64) / rows as f64;
            (0..n).map(|i| {
                let (c, r) = (i % cols, i / cols);
                // the last row spreads its tiles over the full width
                let in_row = if r == rows - 1 { n - cols * (rows - 1) } else { cols };
                let cw = if in_row < cols { (a.w - g * (in_row - 1) as f64) / in_row as f64 } else { cw };
                R { x: (a.x + c as f64 * (cw + g)).round(), y: (a.y + r as f64 * (rh + g)).round(), w: cw.round(), h: rh.round() }
            }).collect()
        }
    }
}

/// The minimised-app grid inside `band`: square tiles of `side` (shrunk to fit at least one per
/// row), scrolled by `scroll` px. Returns every tile's rect (some may lie outside the band) and
/// the furthest the grid can scroll.
pub fn grid(band: R, n: usize, side: f64, gap: f64, scroll: f64) -> (Vec<R>, f64) {
    if n == 0 {
        return (vec![], 0.0);
    }
    let side = side.min(band.w).min(band.h).max(24.0);
    let cols = (((band.w + gap) / (side + gap)).floor() as usize).max(1);
    let rows = n.div_ceil(cols);
    // centre the used columns
    let used = cols.min(n) as f64 * (side + gap) - gap;
    let x0 = band.x + ((band.w - used) / 2.0).max(0.0);
    let content = rows as f64 * (side + gap) - gap;
    let max_scroll = (content - band.h).max(0.0);
    let s = scroll.clamp(0.0, max_scroll);
    let v = (0..n).map(|i| R { x: (x0 + (i % cols) as f64 * (side + gap)).round(), y: (band.y + (i / cols) as f64 * (side + gap) - s).round(), w: side, h: side }).collect();
    (v, max_scroll)
}

/// Which windows are open in the app area, which are minimised, and which has the keyboard.
#[derive(Default, Debug)]
pub struct Ring {
    /// open apps in tile order: index 0 is the main tile
    pub order: Vec<u64>,
    /// minimised apps, most recently minimised first
    pub min: Vec<u64>,
    /// keyboard focus (always an open app)
    pub focus: Option<u64>,
}

impl Ring {
    /// a new app takes the main tile and the keyboard; past `max_open` the oldest open app is
    /// minimised (returned)
    pub fn spawn(&mut self, id: u64, max_open: usize) -> Option<u64> {
        self.order.retain(|x| *x != id);
        self.min.retain(|x| *x != id);
        self.order.insert(0, id);
        self.focus = Some(id);
        if self.order.len() > max_open.max(1) {
            let out = self.order.pop();
            if let Some(o) = out {
                self.min.insert(0, o);
            }
            return out;
        }
        None
    }
    pub fn remove(&mut self, id: u64) {
        let pos = self.order.iter().position(|x| *x == id);
        self.order.retain(|x| *x != id);
        self.min.retain(|x| *x != id);
        if self.focus == Some(id) {
            self.focus = pos.and_then(|p| self.order.get(p.min(self.order.len().saturating_sub(1))).copied()).or(self.order.first().copied());
        }
    }
    /// Mod+Tab: the next open app gets the keyboard
    pub fn next(&mut self) {
        self.cycle(1);
    }
    pub fn prev(&mut self) {
        self.cycle(-1);
    }
    fn cycle(&mut self, d: i64) {
        let n = self.order.len() as i64;
        if n == 0 {
            return;
        }
        let i = self.focus.and_then(|f| self.order.iter().position(|x| *x == f)).unwrap_or(0) as i64;
        self.focus = Some(self.order[((i + d).rem_euclid(n)) as usize]);
    }
    /// Give `id` the keyboard; a minimised app is restored into the main tile first.
    pub fn focus(&mut self, id: u64) -> bool {
        self.focus_with(id, usize::MAX).0
    }
    /// as focus(); restoring past `max_open` minimises the oldest open app (returned)
    pub fn focus_with(&mut self, id: u64, max_open: usize) -> (bool, Option<u64>) {
        if self.min.contains(&id) {
            self.min.retain(|x| *x != id);
            let out = self.spawn(id, max_open);
            return (true, out);
        }
        if self.order.contains(&id) && self.focus != Some(id) {
            self.focus = Some(id);
            return (true, None);
        }
        (false, None)
    }
    /// Mod+M: minimise an open app; the keyboard moves to the next open one
    pub fn minimize(&mut self, id: u64) -> bool {
        let Some(pos) = self.order.iter().position(|x| *x == id) else { return false };
        self.order.remove(pos);
        self.min.insert(0, id);
        if self.focus == Some(id) {
            self.focus = self.order.get(pos.min(self.order.len().saturating_sub(1))).copied();
        }
        true
    }
    /// Mod+Shift+M: bring back the most recently minimised app
    pub fn restore_last(&mut self, max_open: usize) -> Option<u64> {
        let id = *self.min.first()?;
        self.focus_with(id, max_open);
        Some(id)
    }
    /// Mod+Shift+Return: swap the focused app into the main tile
    pub fn promote(&mut self) {
        if let Some(f) = self.focus {
            if let Some(p) = self.order.iter().position(|x| *x == f) {
                self.order.remove(p);
                self.order.insert(0, f);
            }
        }
    }
    pub fn center(&self) -> Option<u64> {
        self.focus
    }
    pub fn all(&self) -> impl Iterator<Item = &u64> {
        self.order.iter().chain(self.min.iter())
    }
}

/// Fullscreen / maximised per window (the 8.x width presets became the main-tile share).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Size {
    pub preset: usize,
    pub full: bool,
    /// fills the app area above the other tiles, title bar kept (Mod+Up / the □ button)
    pub max: bool,
}
impl Size {
    pub fn step(&mut self, up: bool, n: usize) {
        self.full = false;
        self.preset = if up { (self.preset + 1).min(n - 1) } else { self.preset.saturating_sub(1) };
    }
    pub fn toggle_full(&mut self) {
        self.full = !self.full;
    }
}

#[cfg(test)]
mod title_t {
    use super::*;
    #[test]
    fn title_bar_buttons_sit_right_to_left() {
        let c = R { x: 100.0, y: 130.0, w: 600.0, h: 400.0 };
        let (t, b) = title_bar(c);
        assert_eq!((t.y, t.h, t.w), (100.0, TITLE_H, 600.0));
        assert_eq!(b[0].0, TitleBtn::Close);
        assert!((b[0].1.x + b[0].1.w - 700.0).abs() < 0.01, "close is at the right edge");
        assert!(b[2].1.x < b[1].1.x && b[1].1.x < b[0].1.x);
        assert_eq!(title_hit(c, 690.0, 110.0), Some(TitleBtn::Close));
        assert_eq!(title_hit(c, 200.0, 110.0), Some(TitleBtn::Bar));
        assert_eq!(title_hit(c, 200.0, 140.0), None, "the content is not the bar");
    }
}

#[cfg(test)]
mod t {
    use super::*;

    #[test]
    fn open_minimise_restore() {
        let mut r = Ring::default();
        r.spawn(1, 4);
        r.spawn(2, 4);
        r.spawn(3, 4);
        assert_eq!(r.order, vec![3, 2, 1], "the newest app takes the main tile");
        assert_eq!(r.center(), Some(3));
        r.next();
        assert_eq!(r.center(), Some(2));
        assert!(r.minimize(2));
        assert_eq!((r.order.clone(), r.min.clone()), (vec![3, 1], vec![2]));
        assert_eq!(r.center(), Some(1), "the keyboard moves to the next open app");
        assert!(r.focus(2), "a click on a minimised tile restores it");
        assert_eq!((r.order.clone(), r.min.clone(), r.center()), (vec![2, 3, 1], vec![], Some(2)));
        // past max_open the oldest open app is minimised
        assert_eq!(r.spawn(4, 3), Some(1));
        assert_eq!(r.min, vec![1]);
        assert_eq!(r.restore_last(3), Some(1));
        assert_eq!(r.order[0], 1);
        r.remove(1);
        assert_eq!(r.center(), Some(4));
        r.remove(4);
        r.remove(2);
        r.remove(3);
        assert_eq!(r.center(), None);
    }

    #[test]
    fn tiles_fill_the_area_without_overlap() {
        let a = R { x: 300.0, y: 20.0, w: 1100.0, h: 1000.0 };
        for n in 1..=9 {
            let t = tile(a, n, 12.0, 0.6);
            assert_eq!(t.len(), n);
            for (i, r) in t.iter().enumerate() {
                assert!(r.x >= a.x - 0.5 && r.y >= a.y - 0.5 && r.x + r.w <= a.x + a.w + 1.0 && r.y + r.h <= a.y + a.h + 1.0, "n={n} tile {i} {r:?} leaves the area");
                for s in &t[i + 1..] {
                    let overlap = r.x < s.x + s.w - 0.5 && s.x < r.x + r.w - 0.5 && r.y < s.y + s.h - 0.5 && s.y < r.y + r.h - 0.5;
                    assert!(!overlap, "n={n}: {r:?} overlaps {s:?}");
                }
            }
            let used: f64 = t.iter().map(|r| r.w * r.h).sum();
            assert!(used > a.w * a.h * 0.9, "n={n} wastes space");
        }
        assert_eq!(tile(a, 1, 12.0, 0.6)[0], a);
    }

    #[test]
    fn grid_scrolls_and_clamps() {
        let band = R { x: 0.0, y: 800.0, w: 1000.0, h: 260.0 };
        let (t, max) = grid(band, 20, 120.0, 14.0, 0.0);
        assert_eq!(t.len(), 20);
        assert!(t.iter().all(|r| (r.w - r.h).abs() < 0.01), "tiles are square");
        assert!(max > 0.0, "20 tiles in two rows of seven need scrolling");
        let (t2, _) = grid(band, 20, 120.0, 14.0, 1e9);
        assert!((t2[19].y + t2[19].h - (band.y + band.h)).abs() < 1.0, "scrolled to the end, the last row sits at the bottom");
        assert_eq!(grid(band, 3, 120.0, 14.0, 50.0).1, 0.0, "one row never scrolls");
        assert!((ease(0.0)).abs() < 1e-6 && (ease(1.0) - 1.0).abs() < 1e-6 && ease(0.5) > 0.5);
    }
}
