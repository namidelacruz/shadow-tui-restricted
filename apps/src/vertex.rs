//! The 3D vertex background: a wireframe terrain under a slowly turning icosphere, drawn on the
//! CPU. The camera leans towards the mouse cursor, and the mesh lifts, ripples and lights up
//! (cyan → amber) around it.
//!
//! Cost: nothing while idle. A frame is drawn only on pointer motion, and the easing towards the
//! cursor stops after about 1.5 s. Each frame copies a precomputed gradient and draws about 2,000
//! anti-aliased edges.
use shadow_ui::canvas::lerp;
use shadow_ui::{Canvas, Theme};

const GX: usize = 44; // terrain columns
const GZ: usize = 26; // terrain rows (depth)
const Z0: f32 = 0.45;
const Z1: f32 = 4.6;

pub struct Field {
    terrain: Vec<[f32; 3]>,
    ico: Vec<[f32; 3]>,
    ico_e: Vec<(usize, usize)>,
    bg: Vec<u8>,
    bg_size: (usize, usize),
    /// eased cursor position (0..1 of the surface), and where it is heading
    cur: (f32, f32),
    target: (f32, f32),
    /// ripple energy (0..1) from recent movement, and its phase
    energy: f32,
    phase: f32,
    /// how much the cursor is over the surface (eases in / out)
    hover: f32,
    inside: bool,
    last: Option<(f32, f32)>,
    scratch: Vec<(f32, f32, f32, f32)>,
}

impl Default for Field {
    fn default() -> Field {
        Field::new()
    }
}

fn height(x: f32, z: f32) -> f32 {
    0.07 * (2.1 * x + 0.7 * z).sin() + 0.05 * (3.3 * z - 1.3 * x).sin() + 0.035 * (4.7 * x * 0.6 + 1.9 * z).cos() + 0.02 * (z * 1.3).sin() * (x * 6.0).cos()
}

/// Icosahedron subdivided once: 42 vertices, 120 edges, on the unit sphere.
fn icosphere() -> (Vec<[f32; 3]>, Vec<(usize, usize)>) {
    let t = (1.0 + 5f32.sqrt()) / 2.0;
    let mut v: Vec<[f32; 3]> = vec![
        [-1.0, t, 0.0],
        [1.0, t, 0.0],
        [-1.0, -t, 0.0],
        [1.0, -t, 0.0],
        [0.0, -1.0, t],
        [0.0, 1.0, t],
        [0.0, -1.0, -t],
        [0.0, 1.0, -t],
        [t, 0.0, -1.0],
        [t, 0.0, 1.0],
        [-t, 0.0, -1.0],
        [-t, 0.0, 1.0],
    ];
    let faces: [[usize; 3]; 20] = [
        [0, 11, 5],
        [0, 5, 1],
        [0, 1, 7],
        [0, 7, 10],
        [0, 10, 11],
        [1, 5, 9],
        [5, 11, 4],
        [11, 10, 2],
        [10, 7, 6],
        [7, 1, 8],
        [3, 9, 4],
        [3, 4, 2],
        [3, 2, 6],
        [3, 6, 8],
        [3, 8, 9],
        [4, 9, 5],
        [2, 4, 11],
        [6, 2, 10],
        [8, 6, 7],
        [9, 8, 1],
    ];
    let norm = |p: [f32; 3]| {
        let l = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
        [p[0] / l, p[1] / l, p[2] / l]
    };
    for p in v.iter_mut() {
        *p = norm(*p);
    }
    let mut mid = std::collections::HashMap::new();
    let mut edges = std::collections::BTreeSet::new();
    let mut m = |a: usize, b: usize, v: &mut Vec<[f32; 3]>| -> usize {
        let k = (a.min(b), a.max(b));
        *mid.entry(k).or_insert_with(|| {
            let (p, q) = (v[a], v[b]);
            v.push(norm([(p[0] + q[0]) / 2.0, (p[1] + q[1]) / 2.0, (p[2] + q[2]) / 2.0]));
            v.len() - 1
        })
    };
    for f in faces {
        let (a, b, c) = (f[0], f[1], f[2]);
        let (ab, bc, ca) = (m(a, b, &mut v), m(b, c, &mut v), m(c, a, &mut v));
        for (x, y) in [(a, ab), (ab, b), (b, bc), (bc, c), (c, ca), (ca, a), (ab, bc), (bc, ca), (ca, ab)] {
            edges.insert((x.min(y), x.max(y)));
        }
    }
    (v, edges.into_iter().collect())
}

impl Field {
    pub fn new() -> Field {
        let mut terrain = Vec::with_capacity(GX * GZ);
        for j in 0..GZ {
            // rows packed closer near the camera so the perspective reads evenly
            let tz = j as f32 / (GZ - 1) as f32;
            let z = Z0 + (Z1 - Z0) * tz.powf(1.25);
            for i in 0..GX {
                let x = -2.2 + 4.4 * i as f32 / (GX - 1) as f32;
                terrain.push([x, -0.78 + height(x, z), z]);
            }
        }
        let (ico, ico_e) = icosphere();
        Field { terrain, ico, ico_e, bg: vec![], bg_size: (0, 0), cur: (0.5, 0.5), target: (0.5, 0.5), energy: 0.0, phase: 0.0, hover: 0.0, inside: false, last: None, scratch: vec![] }
    }

    /// Pointer at (x, y) as a fraction of the surface.
    pub fn pointer(&mut self, x: f32, y: f32) {
        let (x, y) = (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0));
        if let Some((lx, ly)) = self.last {
            let d = ((x - lx).powi(2) + (y - ly).powi(2)).sqrt();
            self.energy = (self.energy + d * 5.0).min(1.0);
        }
        self.last = Some((x, y));
        self.target = (x, y);
        self.inside = true;
    }

    pub fn leave(&mut self) {
        self.inside = false;
        self.last = None;
        self.target = (0.5, 0.5);
    }

    /// True while the field is still easing (the caller should keep stepping at ~60 Hz).
    pub fn moving(&self) -> bool {
        let h = if self.inside { 1.0 } else { 0.0 };
        (self.target.0 - self.cur.0).abs() > 0.0006 || (self.target.1 - self.cur.1).abs() > 0.0006 || self.energy > 0.01 || (h - self.hover).abs() > 0.01
    }

    /// Advance the easing by one ~16 ms step; true while anything still moves.
    pub fn step(&mut self) -> bool {
        let k = 0.16;
        self.cur.0 += (self.target.0 - self.cur.0) * k;
        self.cur.1 += (self.target.1 - self.cur.1) * k;
        self.energy *= 0.94;
        self.phase += 0.20;
        let h = if self.inside { 1.0 } else { 0.0 };
        self.hover += (h - self.hover) * 0.12;
        let moving = (self.target.0 - self.cur.0).abs() > 0.0006 || (self.target.1 - self.cur.1).abs() > 0.0006 || self.energy > 0.01 || (h - self.hover).abs() > 0.01;
        if !moving {
            self.cur = self.target;
            self.energy = 0.0;
            self.hover = h;
        }
        moving
    }

    fn background(&mut self, w: usize, h: usize, th: &Theme) {
        if self.bg_size == (w, h) && !self.bg.is_empty() {
            return;
        }
        self.bg = vec![0u8; w * h * 4];
        self.bg_size = (w, h);
        let (fw, fh) = (w as f32, h as f32);
        let top = lerp(th.bg, (0x14, 0x1A, 0x2A), 0.85);
        for y in 0..h {
            let v = y as f32 / fh;
            let row = lerp(top, th.bg, v.powf(0.7));
            let scan = if y % 3 == 0 { 0.94 } else { 1.0 };
            for x in 0..w {
                let u = x as f32 / fw;
                // a soft violet / cyan haze behind the hub, and a vignette
                let dc = ((u - 0.5) * 1.6).powi(2) + ((v - 0.5) * 2.0).powi(2);
                let haze = (-dc * 2.2).exp();
                let mut c = lerp(row, th.violet, 0.16 * haze);
                c = lerp(c, th.cyan, 0.05 * (-dc * 6.0).exp());
                let vg = (1.0 - 0.45 * dc.min(1.5)).max(0.3) * scan;
                let i = (y * w + x) * 4;
                self.bg[i] = (c.2 as f32 * vg) as u8;
                self.bg[i + 1] = (c.1 as f32 * vg) as u8;
                self.bg[i + 2] = (c.0 as f32 * vg) as u8;
                self.bg[i + 3] = 255;
            }
        }
    }

    /// Draw the whole background (opaque) into `cv`.
    pub fn draw(&mut self, cv: &mut Canvas, th: &Theme) {
        let (w, h) = (cv.w, cv.h);
        if w == 0 || h == 0 {
            return;
        }
        self.background(w, h, th);
        // copy over exactly the w×h pixels (a buffer may carry padding past them)
        let n = self.bg.len().min(cv.buf.len());
        cv.buf[..n].copy_from_slice(&self.bg[..n]);
        let (fw, fh) = (w as f32, h as f32);
        let focal = fh * 0.95;
        let (cx, cy) = (fw / 2.0, fh * 0.5);
        // the camera leans towards the cursor
        let yaw = (self.cur.0 - 0.5) * 0.34;
        let pitch = 0.10 + (self.cur.1 - 0.5) * 0.14;
        let (sy_, cy_) = yaw.sin_cos();
        let (sp, cp) = pitch.sin_cos();
        let project = |p: [f32; 3]| -> (f32, f32, f32) {
            // world → camera: yaw around y, then pitch around x
            let x = p[0] * cy_ - p[2] * sy_;
            let z = p[0] * sy_ + p[2] * cy_;
            let y = p[1] * cp + z * sp;
            let z = -p[1] * sp + z * cp;
            let z = z.max(0.05);
            (cx + focal * x / z, cy - focal * y / z, z)
        };
        let (mx, my) = (self.cur.0 * fw, self.cur.1 * fh);
        let reach = fh * 0.22;
        let (energy, hover, phase) = (self.energy, self.hover, self.phase);
        // terrain: project, lift around the cursor, project again
        self.scratch.clear();
        for p in &self.terrain {
            let (sx, sy, _) = project(*p);
            let d = ((sx - mx).powi(2) + (sy - my).powi(2)).sqrt() / reach;
            let g = (-d * d).exp() * hover;
            let lift = g * 0.07 + energy * 0.09 * (-d * d * 0.6).exp() * (d * 5.0 - phase).cos() * hover;
            let (sx, sy, z) = project([p[0], p[1] + lift, p[2]]);
            self.scratch.push((sx, sy, z, g));
        }
        let fog = |z: f32| (1.0 - (z - Z0) / (Z1 - Z0)).clamp(0.0, 1.0).powf(1.3);
        let col = |f: f32, g: f32| lerp(lerp(th.violet, th.cyan, f), th.amber, (g * 1.3).min(1.0));
        for j in 0..GZ {
            for i in 0..GX {
                let a = self.scratch[j * GX + i];
                let mut edge = |b: (f32, f32, f32, f32)| {
                    let f = fog((a.2 + b.2) / 2.0);
                    let g = (a.3 + b.3) / 2.0;
                    let alpha = 0.05 + 0.34 * f + 0.55 * g;
                    cv.wline(a.0, a.1, b.0, b.1, col(f, g), alpha);
                };
                if i + 1 < GX {
                    edge(self.scratch[j * GX + i + 1]);
                }
                if j + 1 < GZ {
                    edge(self.scratch[(j + 1) * GX + i]);
                }
            }
        }
        for &(sx, sy, z, g) in &self.scratch {
            let f = fog(z);
            if g > 0.30 {
                cv.node(sx, sy, 1.2 + 1.2 * g, col(f, g), 0.4 + 0.6 * g);
            } else if f > 0.15 && sx >= 0.0 && sy >= 0.0 {
                cv.blend(sx as i32, sy as i32, 1, 1, col(f, 0.0), 0.25 + 0.5 * f);
            }
        }
        // the globe: turns with the cursor, floats over the hub
        let (ry, rx) = (yaw * 2.6 + 0.6, pitch * 2.4 + 0.35);
        let (s1, c1) = ry.sin_cos();
        let (s2, c2) = rx.sin_cos();
        let r = 0.42;
        let centre = [0.0, -0.205, 2.0];
        let pts: Vec<(f32, f32, f32, f32)> = self
            .ico
            .iter()
            .map(|v| {
                let x = v[0] * c1 - v[2] * s1;
                let z = v[0] * s1 + v[2] * c1;
                let y = v[1] * c2 - z * s2;
                let z = v[1] * s2 + z * c2;
                let (sx, sy, d) = project([centre[0] + x * r, centre[1] + y * r, centre[2] + z * r]);
                // front-facing vertices are brighter
                (sx, sy, d, (0.5 - z * 0.5).clamp(0.0, 1.0))
            })
            .collect();
        let glow = 0.25 * hover + 0.6 * energy;
        for &(a, b) in &self.ico_e {
            let (p, q) = (pts[a], pts[b]);
            let front = (p.3 + q.3) / 2.0;
            cv.wline(p.0, p.1, q.0, q.1, lerp(th.violet, th.cyan, front), 0.10 + 0.32 * front + 0.2 * glow);
        }
        for p in &pts {
            if p.3 > 0.45 {
                cv.node(p.0, p.1, 1.3, lerp(th.cyan, th.amber, glow.min(1.0)), 0.35 + 0.5 * p.3);
            }
        }
    }

    /// Static frame (cursor centred), BGRA opaque — for the wallpaper daemon and previews.
    pub fn render_static(w: usize, h: usize, th: &Theme) -> Vec<u8> {
        Field::render_at(w, h, th, None)
    }

    /// A frame with the cursor resting at `cursor` (fractions), as it looks once the easing settles.
    pub fn render_at(w: usize, h: usize, th: &Theme, cursor: Option<(f32, f32)>) -> Vec<u8> {
        let mut buf = vec![0u8; w * h * 4];
        let mut f = Field::new();
        if let Some((x, y)) = cursor {
            f.pointer(x, y);
            f.cur = (x, y);
            f.hover = 1.0;
            f.energy = 0.6;
        }
        let mut cv = Canvas { buf: &mut buf, w, h };
        f.draw(&mut cv, th);
        buf
    }

    /// Screen point of a fraction, for tests.
    pub fn cursor(&self) -> (f32, f32) {
        self.cur
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icosphere_shape() {
        let (v, e) = icosphere();
        assert_eq!((v.len(), e.len()), (42, 120));
    }

    #[test]
    fn eases_to_the_cursor_and_stops() {
        let mut f = Field::new();
        f.pointer(0.9, 0.2);
        let mut n = 0;
        while f.step() {
            n += 1;
            assert!(n < 400, "never settles");
        }
        assert_eq!(f.cursor(), (0.9, 0.2));
        assert!(n > 5, "it eases rather than jumping");
        f.leave();
        while f.step() {}
        assert_eq!(f.cursor(), (0.5, 0.5));
    }

    #[test]
    fn padded_buffer_does_not_panic() {
        // 2561×1601 (a 1.5 scale rounding up) in a pool slot rounded up to 64 bytes: 60 bytes extra
        let (w, h) = (2561usize, 1601usize);
        let mut buf = vec![0u8; w * h * 4 + 60];
        let mut f = Field::new();
        f.draw(&mut Canvas { buf: &mut buf, w, h }, &Theme::default());
        assert_eq!(buf[3], 255, "drawn");
    }

    #[test]
    fn draws_opaque_with_wires() {
        let th = Theme::default();
        let px = Field::render_static(320, 200, &th);
        assert!(px.chunks_exact(4).all(|p| p[3] == 255));
        let bright = px.chunks_exact(4).filter(|p| p[0] as u32 + p[1] as u32 + p[2] as u32 > 160).count();
        assert!(bright > 200, "the wireframe is visible ({bright})");
    }
}

#[cfg(test)]
mod bench {
    use super::*;

    /// cargo test --release -p shadow-apps vertex::bench -- --ignored --nocapture
    #[test]
    #[ignore]
    fn frame_cost() {
        let th = Theme::default();
        for (w, h) in [(1920usize, 1200usize), (2560, 1600), (3840, 2160)] {
            let mut buf = vec![0u8; w * h * 4];
            let mut f = Field::new();
            f.draw(&mut Canvas { buf: &mut buf, w, h }, &th);
            let t = std::time::Instant::now();
            for i in 0..30 {
                f.pointer(0.3 + i as f32 * 0.01, 0.6);
                f.step();
                f.draw(&mut Canvas { buf: &mut buf, w, h }, &th);
            }
            println!("{w}x{h}: {:.1} ms / frame", t.elapsed().as_secs_f64() * 1000.0 / 30.0);
        }
    }
}
