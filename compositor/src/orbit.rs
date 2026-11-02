//! The orbital ring: pure geometry and state transitions, no Wayland types.
//!
//! The workspace is a cylindrical space (r, θ, z). The focused window sits on the centre focal plane
//! (0, 0, 0), parallel to the glass. The others are nodes on an ellipse
//!   x = Rx·sin θᵢ,  y = Y_offset,  z = −Rz·cos θᵢ
//! where θᵢ = 2π·i/(N+1) spreads the N background windows evenly around the centre slot (θ = 0).
//! They are projected with a simple pinhole camera, so nearer nodes appear lower and larger.

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
}

/// cubic-bezier(0.2, 0.0, 0.0, 1.0) — fast start, soft landing
pub fn ease(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    let (x1, y1, x2, y2) = (0.2, 0.0, 0.0, 1.0);
    // solve x(s) = t for s by bisection, then return y(s)
    let bez = |s: f64, a: f64, b: f64| 3.0 * (1.0 - s) * (1.0 - s) * s * a + 3.0 * (1.0 - s) * s * s * b + s * s * s;
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..30 {
        let mid = (lo + hi) / 2.0;
        if bez(mid, x1, x2) < t { lo = mid } else { hi = mid }
    }
    bez((lo + hi) / 2.0, y1, y2)
}

pub struct Geo {
    /// usable area (output minus the HUD bars), logical px
    pub area: R,
    pub rx: f64,
    pub rz: f64,
    pub y_off: f64,
    pub node_w: f64,
    pub node_h: f64,
}

impl Geo {
    /// screen rect and depth-scale of a ring node at angle θ
    pub fn node(&self, theta: f64) -> (R, f64) {
        let a = &self.area;
        let (rx, rz) = (self.rx * a.w, self.rz * a.w);
        let f = 1.6 * a.w; // focal length
        let x = rx * theta.sin();
        let z = -rz * theta.cos();
        let s = f / (f - z); // z toward the viewer is positive
        let cx = a.x + a.w / 2.0 + x * s;
        let cy = a.y + a.h / 2.0 + self.y_off * a.h * s;
        let (w, h) = (self.node_w * s, self.node_h * s);
        // keep every node fully on screen (between the HUD bars)
        let x = (cx - w / 2.0).clamp(a.x + 6.0, (a.x + a.w - w - 6.0).max(a.x + 6.0));
        let y = (cy - h / 2.0).clamp(a.y + 6.0, (a.y + a.h - h - 6.0).max(a.y + 6.0));
        (R { x, y, w, h }, s)
    }

    /// centre stage rect for a width preset (%) or fullscreen
    pub fn center(&self, width_pct: u32, height_pct: u32, full: bool) -> R {
        let a = &self.area;
        if full {
            return *a;
        }
        let w = (a.w * width_pct as f64 / 100.0).round();
        let h = (a.h * height_pct as f64 / 100.0).round();
        R { x: (a.x + (a.w - w) / 2.0).round(), y: (a.y + (a.h - h) / 2.0).round(), w, h }
    }
}

/// angle of ring slot `i` (1-based) with `n` background windows
pub fn slot_angle(i: usize, n: usize) -> f64 {
    std::f64::consts::TAU * i as f64 / (n + 1) as f64
}

/// The window order: index 0 is centre stage; 1..n are the ring slots θ₁..θₙ.
#[derive(Default, Debug)]
pub struct Ring {
    pub order: Vec<u64>,
}

impl Ring {
    /// a new process claims the centre and pushes the previous one to θ₁
    pub fn spawn(&mut self, id: u64) {
        self.order.retain(|x| *x != id);
        self.order.insert(0, id);
    }
    pub fn remove(&mut self, id: u64) {
        self.order.retain(|x| *x != id);
    }
    /// Mod+Tab / Mod+J: the node at θ₁ rotates into the centre
    pub fn next(&mut self) {
        if self.order.len() > 1 {
            self.order.rotate_left(1);
        }
    }
    /// Mod+Shift+Tab / Mod+K: the node at θₙ rotates into the centre
    pub fn prev(&mut self) {
        if self.order.len() > 1 {
            self.order.rotate_right(1);
        }
    }
    pub fn focus(&mut self, id: u64) -> bool {
        match self.order.iter().position(|x| *x == id) {
            Some(i) if i > 0 => {
                self.order.rotate_left(i);
                true
            }
            _ => false,
        }
    }
    pub fn center(&self) -> Option<u64> {
        self.order.first().copied()
    }
}

/// Width stepping across the presets, remembering the preset behind a fullscreen toggle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Size {
    pub preset: usize,
    pub full: bool,
}
impl Size {
    pub fn step(&mut self, up: bool, n: usize) {
        self.full = false;
        self.preset = if up { (self.preset + 1).min(n - 1) } else { self.preset.saturating_sub(1) };
    }
    /// Mod+F twice restores the exact prior preset
    pub fn toggle_full(&mut self) {
        self.full = !self.full;
    }
}

#[cfg(test)]
mod t {
    use super::*;
    #[test]
    fn ring_transitions() {
        let mut r = Ring::default();
        r.spawn(1);
        r.spawn(2);
        r.spawn(3);
        assert_eq!(r.order, vec![3, 2, 1]); // newest centre, previous at θ₁
        r.next();
        assert_eq!(r.center(), Some(2));
        r.prev();
        assert_eq!(r.center(), Some(3));
        assert!(r.focus(1));
        assert_eq!(r.center(), Some(1));
        r.remove(1);
        assert_eq!(r.center(), Some(3));
    }
    #[test]
    fn sizes_and_geometry() {
        let mut s = Size { preset: 1, full: false };
        s.toggle_full();
        s.toggle_full();
        assert_eq!(s, Size { preset: 1, full: false });
        s.step(true, 3);
        s.step(true, 3);
        assert_eq!(s.preset, 2);
        s.step(false, 3);
        assert_eq!(s.preset, 1);
        let g = Geo { area: R { x: 0.0, y: 19.0, w: 1706.0, h: 1028.0 }, rx: 0.4, rz: 0.42, y_off: 0.24, node_w: 300.0, node_h: 176.0 };
        let c = g.center(65, 86, false);
        assert!((c.x + c.w / 2.0 - 853.0).abs() < 1.0 && (c.y + c.h / 2.0 - (19.0 + 514.0)).abs() < 1.0);
        assert_eq!(g.center(65, 86, true), g.area);
        // symmetric slots: left and right nodes mirror each other, and the back node is smaller
        let (a, _) = g.node(slot_angle(1, 3));
        let (b, _) = g.node(slot_angle(3, 3));
        assert!(((a.x + a.w / 2.0 - 853.0) + (b.x + b.w / 2.0 - 853.0)).abs() < 0.01);
        for i in 1..=6 {
            let (r, _) = g.node(slot_angle(i, 6));
            assert!(r.x >= g.area.x && r.x + r.w <= g.area.x + g.area.w && r.y >= g.area.y && r.y + r.h <= g.area.y + g.area.h);
        }
        let (_, s_front) = g.node(std::f64::consts::PI);
        let (_, s_back) = g.node(0.0);
        assert!(s_front > 1.0 && s_back < 1.0);
        assert!((ease(0.0)).abs() < 1e-6 && (ease(1.0) - 1.0).abs() < 1e-6 && ease(0.5) > 0.5);
    }
}
