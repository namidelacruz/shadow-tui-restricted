//! Premultiplied ARGB8888 software canvas.
use crate::font::Glyph;

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
    [m(c.2), m(c.1), m(c.0), a8 as u8] // BGRA in memory = ARGB8888 little-endian
}

impl Canvas<'_> {
    pub fn clear(&mut self, c: Rgb, a: f32) {
        let px = premul(c, a);
        for p in self.buf.chunks_exact_mut(4) {
            p.copy_from_slice(&px);
        }
    }

    pub fn fill(&mut self, x: i32, y: i32, w: i32, h: i32, c: Rgb, a: f32) {
        let px = premul(c, a);
        let (x0, y0) = (x.max(0) as usize, y.max(0) as usize);
        let x1 = ((x + w).max(0) as usize).min(self.w);
        let y1 = ((y + h).max(0) as usize).min(self.h);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        for yy in y0..y1 {
            for p in self.buf[(yy * self.w + x0) * 4..(yy * self.w + x1) * 4].chunks_exact_mut(4) {
                p.copy_from_slice(&px);
            }
        }
    }

    #[inline]
    fn blend_px(&mut self, x: usize, y: usize, c: Rgb, a: f32) {
        if x >= self.w || y >= self.h || a <= 0.0 {
            return;
        }
        let i = (y * self.w + x) * 4;
        let s = premul(c, a);
        let ia = 1.0 - a.min(1.0);
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

    /// 1-device-pixel-or-thicker rectangle outline.
    pub fn border(&mut self, x: i32, y: i32, w: i32, h: i32, t: i32, c: Rgb, a: f32) {
        let t = t.max(1);
        self.blend(x, y, w, t, c, a);
        self.blend(x, y + h - t, w, t, c, a);
        self.blend(x, y + t, t, h - 2 * t, c, a);
        self.blend(x + w - t, y + t, t, h - 2 * t, c, a);
    }

    /// Tactical corner brackets.
    pub fn brackets(&mut self, x: i32, y: i32, w: i32, h: i32, len: i32, t: i32, c: Rgb, a: f32) {
        let (l, t) = (len.min(w / 2).min(h / 2), t.max(1));
        for (cx, cy, dx, dy) in [(x, y, 1, 1), (x + w - 1, y, -1, 1), (x, y + h - 1, 1, -1), (x + w - 1, y + h - 1, -1, -1)] {
            let hx = if dx > 0 { cx } else { cx - l + 1 };
            let vy = if dy > 0 { cy } else { cy - l + 1 };
            let hy = if dy > 0 { cy } else { cy - t + 1 };
            let vx = if dx > 0 { cx } else { cx - t + 1 };
            self.blend(hx, hy, l, t, c, a);
            self.blend(vx, vy, t, l, c, a);
        }
    }

    // ── fluid shapes: anti-aliased rounded boxes, gradients, glow ──────────────────────────────

    /// Rounded box with per-pixel anti-aliasing. `col(row_fraction)` gives the colour of each row
    /// (vertical gradients), `cov_mul` scales the coverage (alpha).
    fn rbox(&mut self, x: i32, y: i32, w: i32, h: i32, r: f32, a: f32, col: &dyn Fn(f32, f32) -> Rgb) {
        if w <= 0 || h <= 0 || a <= 0.0 {
            return;
        }
        let r = r.clamp(0.0, (w.min(h) as f32) / 2.0);
        let (hw, hh) = (w as f32 / 2.0, h as f32 / 2.0);
        let (cx, cy) = (x as f32 + hw, y as f32 + hh);
        let ri = r.ceil() as i32;
        for yy in y.max(0)..(y + h).min(self.h as i32) {
            let fy = (yy - y) as f32 / (h.max(2) - 1) as f32;
            let fx = |xx: i32| (xx - x) as f32 / (w.max(2) - 1) as f32;
            let edge_row = yy < y + ri || yy >= y + h - ri;
            if !edge_row {
                // straight sides: full-coverage span
                for xx in x.max(0)..(x + w).min(self.w as i32) {
                    self.blend_px(xx as usize, yy as usize, col(fx(xx), fy), a);
                }
                continue;
            }
            for xx in x.max(0)..(x + w).min(self.w as i32) {
                let in_corner_x = xx < x + ri || xx >= x + w - ri;
                let cov = if !in_corner_x {
                    1.0
                } else {
                    let px = (xx as f32 + 0.5 - cx).abs() - (hw - r);
                    let py = (yy as f32 + 0.5 - cy).abs() - (hh - r);
                    let d = (px.max(0.0).powi(2) + py.max(0.0).powi(2)).sqrt() - r;
                    (0.5 - d).clamp(0.0, 1.0)
                };
                if cov > 0.0 {
                    self.blend_px(xx as usize, yy as usize, col(fx(xx), fy), a * cov);
                }
            }
        }
    }

    /// Filled rounded rectangle.
    pub fn rrect(&mut self, x: i32, y: i32, w: i32, h: i32, r: f32, c: Rgb, a: f32) {
        self.rbox(x, y, w, h, r, a, &|_, _| c);
    }

    /// Rounded rectangle with a vertical gradient from `top` to `bot`.
    pub fn rrect_v(&mut self, x: i32, y: i32, w: i32, h: i32, r: f32, top: Rgb, bot: Rgb, a: f32) {
        self.rbox(x, y, w, h, r, a, &|_, t| lerp(top, bot, t));
    }

    /// Rounded rectangle with a horizontal gradient from `left` to `right`.
    pub fn rrect_h(&mut self, x: i32, y: i32, w: i32, h: i32, r: f32, left: Rgb, right: Rgb, a: f32) {
        self.rbox(x, y, w, h, r, a, &|t, _| lerp(left, right, t));
    }

    /// Rounded outline `t` device px thick.
    pub fn rstroke(&mut self, x: i32, y: i32, w: i32, h: i32, r: f32, t: i32, c: Rgb, a: f32) {
        let t = t.max(1);
        let (hw, hh) = (w as f32 / 2.0, h as f32 / 2.0);
        let (cx, cy) = (x as f32 + hw, y as f32 + hh);
        let r = r.clamp(0.0, (w.min(h) as f32) / 2.0);
        let tf = t as f32;
        let sd = |px: f32, py: f32| {
            let qx = (px - cx).abs() - (hw - r);
            let qy = (py - cy).abs() - (hh - r);
            (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - r
        };
        let ri = r.ceil() as i32 + t + 1;
        for yy in y.max(0)..(y + h).min(self.h as i32) {
            let edge_row = yy < y + ri || yy >= y + h - ri;
            let xs: Box<dyn Iterator<Item = i32>> = if edge_row {
                Box::new(x.max(0)..(x + w).min(self.w as i32))
            } else {
                Box::new((x.max(0)..(x + t + 1).min(self.w as i32)).chain((x + w - t - 1).max(0)..(x + w).min(self.w as i32)))
            };
            for xx in xs {
                let d = sd(xx as f32 + 0.5, yy as f32 + 0.5);
                // inside the outer edge and outside the inner one
                let cov = (0.5 - d).clamp(0.0, 1.0) * (0.5 + d + tf).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.blend_px(xx as usize, yy as usize, c, a * cov);
                }
            }
        }
    }

    /// Soft glow around a rounded rectangle, fading over `spread` device px.
    pub fn glow(&mut self, x: i32, y: i32, w: i32, h: i32, r: f32, spread: i32, c: Rgb, a: f32) {
        let s = spread.max(1);
        let (hw, hh) = (w as f32 / 2.0, h as f32 / 2.0);
        let (cx, cy) = (x as f32 + hw, y as f32 + hh);
        let r = r.clamp(0.0, (w.min(h) as f32) / 2.0);
        for yy in (y - s).max(0)..(y + h + s).min(self.h as i32) {
            for xx in (x - s).max(0)..(x + w + s).min(self.w as i32) {
                let inside_x = xx >= x + r as i32 && xx < x + w - r as i32;
                let inside_y = yy >= y + r as i32 && yy < y + h - r as i32;
                if (inside_x && yy >= y && yy < y + h) || (inside_y && xx >= x && xx < x + w) {
                    continue;
                }
                let qx = (xx as f32 + 0.5 - cx).abs() - (hw - r);
                let qy = (yy as f32 + 0.5 - cy).abs() - (hh - r);
                let d = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() - r;
                if d > 0.0 && d < s as f32 {
                    let k = 1.0 - d / s as f32;
                    self.blend_px(xx as usize, yy as usize, c, a * k * k);
                }
            }
        }
    }

    /// A light beam: a horizontal line that fades out towards both ends, `c1` → `c2`.
    pub fn beam(&mut self, x: i32, y: i32, w: i32, t: i32, c1: Rgb, c2: Rgb, a: f32) {
        if w <= 0 {
            return;
        }
        for xx in x.max(0)..(x + w).min(self.w as i32) {
            let f = (xx - x) as f32 / w as f32;
            let fade = (f * 6.0).min((1.0 - f) * 6.0).min(1.0);
            let c = lerp(c1, c2, f);
            for yy in y.max(0)..(y + t.max(1)).min(self.h as i32) {
                self.blend_px(xx as usize, yy as usize, c, a * fade);
            }
        }
    }

    /// Angled tag: a box whose right edge is cut at 60°, horizontal gradient `c1` → `c2`.
    pub fn tag(&mut self, x: i32, y: i32, w: i32, h: i32, c1: Rgb, c2: Rgb, a: f32) {
        let cut = h as f32 * 0.55;
        for yy in y.max(0)..(y + h).min(self.h as i32) {
            let ry = (yy - y) as f32 + 0.5;
            // right edge moves left as we go down: x_end = x + w - cut * ry/h
            let xe = x as f32 + w as f32 - cut * ry / h as f32;
            for xx in x.max(0)..(xe.ceil() as i32).min(self.w as i32) {
                let cov = (xe - xx as f32).clamp(0.0, 1.0);
                let c = lerp(c1, c2, (xx - x) as f32 / w.max(1) as f32);
                self.blend_px(xx as usize, yy as usize, c, a * cov);
            }
        }
    }

    /// Faint horizontal scan lines every `step` px (HUD texture).
    pub fn scanlines(&mut self, x: i32, y: i32, w: i32, h: i32, step: i32, c: Rgb, a: f32) {
        let mut yy = y;
        while yy < y + h {
            self.blend(x, yy, w, 1, c, a);
            yy += step.max(2);
        }
    }

    pub fn glyph(&mut self, g: &Glyph, x: i32, baseline: i32, c: Rgb, alpha: f32) {
        let gx = x + g.left;
        let gy = baseline + g.top;
        for row in 0..g.h {
            let yy = gy + row as i32;
            if yy < 0 || yy >= self.h as i32 {
                continue;
            }
            for col in 0..g.w {
                let cov = g.bmp[row * g.w + col];
                if cov == 0 {
                    continue;
                }
                let xx = gx + col as i32;
                if xx < 0 || xx >= self.w as i32 {
                    continue;
                }
                self.blend_px(xx as usize, yy as usize, c, alpha * cov as f32 / 255.0);
            }
        }
    }
}

pub fn lerp(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    (m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(c: &Canvas, x: usize, y: usize) -> [u8; 4] {
        let i = (y * c.w + x) * 4;
        [c.buf[i], c.buf[i + 1], c.buf[i + 2], c.buf[i + 3]]
    }

    #[test]
    fn rounded_corners_are_cut_and_antialiased() {
        let mut b = vec![0u8; 40 * 40 * 4];
        let mut c = Canvas { buf: &mut b, w: 40, h: 40 };
        c.rrect(0, 0, 40, 40, 10.0, (255, 255, 255), 1.0);
        assert_eq!(px(&c, 0, 0)[3], 0, "corner is transparent");
        assert_eq!(px(&c, 20, 20)[3], 255, "centre is solid");
        assert_eq!(px(&c, 20, 0)[3], 255, "straight edge is solid");
        let edge = px(&c, 2, 3)[3];
        assert!(edge > 0 && edge < 255, "the curve is anti-aliased ({edge})");
        c.clear((0, 0, 0), 0.0);
        c.rstroke(0, 0, 40, 40, 10.0, 2, (255, 255, 255), 1.0);
        assert_eq!(px(&c, 20, 20)[3], 0, "an outline is hollow");
        assert!(px(&c, 20, 0)[3] > 200);
    }

    #[test]
    fn gradients_and_lerp() {
        assert_eq!(lerp((0, 0, 0), (200, 100, 50), 0.5), (100, 50, 25));
        let mut b = vec![0u8; 10 * 10 * 4];
        let mut c = Canvas { buf: &mut b, w: 10, h: 10 };
        c.rrect_v(0, 0, 10, 10, 0.0, (0, 0, 0), (0, 0, 250), 1.0);
        assert!(px(&c, 5, 9)[0] > px(&c, 5, 0)[0], "bottom is bluer (BGRA)");
    }
}
