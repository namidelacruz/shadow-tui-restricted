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
