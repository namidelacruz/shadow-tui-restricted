//! Kinetic unfolding: how windows open, close, minimise and come back (9.0).
//!
//! A window's rectangle is cut into a tessellation — a proportional grid of cells, each split into
//! two triangles by the construction lines drawn over it. The cells unfold one after another,
//! starting where the app came from (the widget in the side column that opened it, its tile in
//! the minimised grid, the hidden hub) and spreading outward, each cell swinging open on the edge
//! that faces the origin. Closing and minimising run the same wave backwards, folding the
//! window up toward the point it leaves for. Pure geometry here; render.rs draws it.
use crate::orbit::R;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Open,
    Close,
    Minimize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hinge {
    Left,
    Right,
    Top,
    Bottom,
}

#[derive(Clone, Debug)]
pub struct Fold {
    pub kind: Kind,
    pub start: Instant,
    pub dur: Duration,
    /// where it unfolds from / folds into, logical
    pub origin: (f64, f64),
    /// the window's rect when a close / minimise started (it keeps folding there)
    pub at: Option<R>,
}

impl Fold {
    pub fn new(kind: Kind, origin: (f64, f64), at: Option<R>, ms: u64) -> Fold {
        Fold { kind, start: Instant::now(), dur: Duration::from_millis(ms), origin, at }
    }
    /// 0..1
    pub fn t(&self) -> f64 {
        (self.start.elapsed().as_secs_f64() / self.dur.as_secs_f64().max(1e-3)).min(1.0)
    }
    pub fn done(&self) -> bool {
        self.t() >= 1.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cell {
    pub r: R,
    /// 0 = folded flat on its hinge, 1 = open
    pub p: f64,
    pub hinge: Hinge,
}

/// the tessellation of a rect: about 150 logical px per cell, 3–9 across, 2–7 down
pub fn grid_dims(r: R) -> (usize, usize) {
    (((r.w / 150.0).round() as usize).clamp(3, 9), ((r.h / 150.0).round() as usize).clamp(2, 7))
}

fn smooth(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// Every cell of `r` and how far it is open at overall time `t` (0..1).
/// Opening: cells near `origin` open first. Closing: cells far from it fold first, so the window
/// collapses toward the origin.
pub fn cells(r: R, origin: (f64, f64), t: f64, opening: bool) -> Vec<Cell> {
    let (cols, rows) = grid_dims(r);
    let (cw, ch) = (r.w / cols as f64, r.h / rows as f64);
    let centres: Vec<(f64, f64)> = (0..rows).flat_map(|j| (0..cols).map(move |i| (i, j))).map(|(i, j)| (r.x + (i as f64 + 0.5) * cw, r.y + (j as f64 + 0.5) * ch)).collect();
    let far = centres.iter().map(|c| ((c.0 - origin.0).powi(2) + (c.1 - origin.1).powi(2)).sqrt()).fold(1.0, f64::max);
    let spread = 0.6; // share of the time the wave takes to cross the window
    centres.iter().enumerate().map(|(k, c)| {
        let (i, j) = (k % cols, k / cols);
        let d = ((c.0 - origin.0).powi(2) + (c.1 - origin.1).powi(2)).sqrt() / far;
        let p = if opening {
            smooth((t - d * spread) / (1.0 - spread))
        } else {
            1.0 - smooth((t - (1.0 - d) * spread) / (1.0 - spread))
        };
        let (dx, dy) = (c.0 - origin.0, c.1 - origin.1);
        let hinge = if dx.abs() * ch >= dy.abs() * cw {
            if dx >= 0.0 { Hinge::Left } else { Hinge::Right }
        } else if dy >= 0.0 {
            Hinge::Top
        } else {
            Hinge::Bottom
        };
        // cells share edges exactly (rounded the same way on both sides)
        let x0 = (r.x + i as f64 * cw).round();
        let x1 = (r.x + (i + 1) as f64 * cw).round();
        let y0 = (r.y + j as f64 * ch).round();
        let y1 = (r.y + (j + 1) as f64 * ch).round();
        Cell { r: R { x: x0, y: y0, w: x1 - x0, h: y1 - y0 }, p, hinge }
    }).collect()
}

/// the construction lines' strength over the animation: they draw in, hold, and fade
pub fn lines_alpha(t: f64) -> f64 {
    (t * std::f64::consts::PI).sin().max(0.0).powf(0.7)
}

#[cfg(test)]
mod t {
    use super::*;
    #[test]
    fn wave_starts_at_the_origin_and_completes() {
        let r = R { x: 100.0, y: 100.0, w: 900.0, h: 600.0 };
        let o = (100.0, 100.0); // top-left corner
        let c = cells(r, o, 0.3, true);
        let (cols, rows) = grid_dims(r);
        assert_eq!(c.len(), cols * rows);
        assert!(c[0].p > c[c.len() - 1].p, "the cell at the origin opens first");
        assert!(cells(r, o, 0.0, true).iter().all(|c| c.p == 0.0));
        assert!(cells(r, o, 1.0, true).iter().all(|c| (c.p - 1.0).abs() < 1e-9));
        // closing: far cells fold first, everything is folded at the end
        let cl = cells(r, o, 0.3, false);
        assert!(cl[0].p > cl[cl.len() - 1].p);
        assert!(cells(r, o, 1.0, false).iter().all(|c| c.p.abs() < 1e-9));
        // the cells tile the rect exactly
        let area: f64 = c.iter().map(|c| c.r.w * c.r.h).sum();
        assert!((area - r.w * r.h).abs() < 1.0);
        // hinges face the origin
        assert_eq!(c[1].hinge, Hinge::Left);
        assert_eq!(c[cols].hinge, Hinge::Top);
        assert!(lines_alpha(0.0) < 1e-6 && lines_alpha(0.5) > 0.99 && lines_alpha(1.0) < 1e-6);
    }
}
