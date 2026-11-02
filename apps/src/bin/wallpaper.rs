//! shadow-wallpaper — wallpapers on every monitor, and a picker.
//!
//!   shadow-wallpaper                     the wallpaper itself (started by niri; background layer,
//!                                        one surface per monitor, follows monitors coming and going)
//!   shadow-wallpaper pick                the picker window: browse, preview, set (all or one monitor)
//!   shadow-wallpaper set PATH|default [--output NAME] [--mode fill|fit|center|tile]
//!   shadow-wallpaper render-default OUT.ppm WIDTHxHEIGHT
//!
//! Config (~/.config/shadow-state/config.toml):
//!   [wallpaper]
//!   path = "vertices"           # the 3D vertex field (default) · "default" = the generated aurora ·
//!                               # or an image (PNG / JPEG). With [hub] enabled, shadow-hub draws it.
//!   mode = "fill"               # fill (crop to cover) · fit (letterbox) · center · tile
//!   [wallpaper.outputs]
//!   eDP-1 = "/home/me/Pictures/a.jpg"   # per-monitor override
//! Images are decoded once and scaled once per monitor size; redraws only copy pixels.
use shadow_apps::home;
use shadow_apps::wall::{choice, compose, conf, decode, default_wallpaper, set, Img, Mode};
use shadow_ui::calloop::channel::{self, Sender};
use shadow_ui::widgets::{button, chrome, footer, panel, selection, Rect};
use shadow_ui::{run, App, Ctx, Frame, Key, Keysym, SurfaceKind};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

// ── the wallpaper surface ───────────────────────────────────────────────────────────────────────

struct Wall {
    text: String,
    images: HashMap<String, Arc<Img>>,
    composed: HashMap<(String, usize, usize, Mode), Arc<Vec<u8>>>,
}

impl Wall {
    fn pixels(&mut self, output: Option<&str>, w: usize, h: usize) -> Arc<Vec<u8>> {
        let (path, mode) = choice(&self.text, output);
        let key = (path.clone(), w, h, mode);
        if let Some(p) = self.composed.get(&key) {
            return p.clone();
        }
        let px = if path == "vertices" {
            shadow_apps::vertex::Field::render_static(w, h, &shadow_ui::Theme::load())
        } else if path == "default" {
            default_wallpaper(w, h)
        } else {
            let img = match self.images.get(&path) {
                Some(i) => Some(i.clone()),
                None => match decode(Path::new(&path)) {
                    Ok(i) => {
                        let i = Arc::new(i);
                        self.images.insert(path.clone(), i.clone());
                        Some(i)
                    }
                    Err(e) => {
                        eprintln!("shadow-wallpaper: {e} — using the default");
                        None
                    }
                },
            };
            match img {
                Some(i) => compose(&i, w, h, mode),
                None => default_wallpaper(w, h),
            }
        };
        let px = Arc::new(px);
        // keep only what the current monitors use
        if self.composed.len() > 8 {
            self.composed.clear();
        }
        self.composed.insert(key, px.clone());
        px
    }
}

impl App for Wall {
    fn kind(&self) -> SurfaceKind {
        SurfaceKind::Background { namespace: "shadow-wallpaper".into(), keyboard: shadow_ui::LayerKeyboard::None }
    }

    fn message(&mut self, m: String, ctx: &mut Ctx) {
        if m == "reload" {
            self.text = conf();
            self.images.clear();
            self.composed.clear();
            ctx.redraw();
        }
    }

    fn draw(&mut self, f: &mut Frame) {
        let (w, h) = (f.cv.w, f.cv.h);
        let px = self.pixels(f.output, w, h);
        if px.len() == f.cv.buf.len() {
            f.cv.buf.copy_from_slice(&px);
        }
    }
}

fn signal_thread(tx: Sender<String>) {
    // SAFETY: standard signal-mask setup before other threads exist
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGUSR1);
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        std::thread::spawn(move || loop {
            let mut sig = 0;
            if libc::sigwait(&set, &mut sig) == 0 && sig == libc::SIGUSR1 && tx.send("reload".into()).is_err() {
                return;
            }
        });
    }
}

// ── the picker ──────────────────────────────────────────────────────────────────────────────────

fn candidates() -> Vec<PathBuf> {
    let h = home();
    let mut dirs = vec![h.join("Pictures/Wallpapers"), h.join("Pictures/wallpapers"), h.join("Pictures"), h.join(".local/share/backgrounds"), PathBuf::from("/usr/share/backgrounds")];
    dirs.dedup();
    let mut v = vec![];
    fn walk(d: &Path, depth: u32, v: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() && depth > 0 && !e.file_name().to_string_lossy().starts_with('.') {
                walk(&p, depth - 1, v);
            } else if p.extension().is_some_and(|x| ["png", "jpg", "jpeg", "PNG", "JPG", "JPEG"].contains(&x.to_string_lossy().as_ref())) {
                v.push(p);
            }
        }
    }
    for d in dirs {
        walk(&d, 2, &mut v);
    }
    v.sort();
    v.dedup();
    v.truncate(500);
    v
}

#[derive(Clone, Copy, PartialEq)]
enum Btn {
    All,
    This,
    Mode,
}

struct Pick {
    items: Vec<String>,
    sel: usize,
    top: usize,
    visible: usize,
    outputs: Vec<String>,
    out_sel: usize,
    mode: Mode,
    preview: Option<(String, Arc<Img>)>,
    loading: Option<String>,
    tx: Sender<String>,
    shared: Arc<std::sync::Mutex<HashMap<String, Arc<Img>>>>,
    msg: (String, bool),
    btns: Vec<(Btn, Rect)>,
    rows: Vec<(usize, Rect)>,
    current: String,
}

impl Pick {
    fn label(p: &str) -> String {
        if p == "vertices" {
            "Vertices — 3D wireframe, follows the cursor".into()
        } else if p == "default" {
            "Aurora (generated)".into()
        } else {
            Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.into())
        }
    }

    fn load_preview(&mut self) {
        let Some(p) = self.items.get(self.sel).cloned() else { return };
        if p == "default" || p == "vertices" || self.preview.as_ref().is_some_and(|x| x.0 == p) {
            return;
        }
        if let Some(i) = self.shared.lock().ok().and_then(|m| m.get(&p).cloned()) {
            self.preview = Some((p, i));
            return;
        }
        if self.loading.as_deref() == Some(p.as_str()) {
            return;
        }
        self.loading = Some(p.clone());
        let tx = self.tx.clone();
        let shared = self.shared.clone();
        std::thread::spawn(move || {
            let msg = match decode(Path::new(&p)) {
                Ok(img) => {
                    // keep a small copy for the preview
                    let s = (640.0 / img.w as f32).min(400.0 / img.h as f32).min(1.0);
                    let (w, h) = (((img.w as f32 * s) as usize).max(1), ((img.h as f32 * s) as usize).max(1));
                    let bgra = compose(&img, w, h, Mode::Fill);
                    let mut rgb = Vec::with_capacity(w * h * 3);
                    for px in bgra.chunks_exact(4) {
                        rgb.extend_from_slice(&[px[2], px[1], px[0]]);
                    }
                    if let Ok(mut m) = shared.lock() {
                        m.insert(p.clone(), Arc::new(Img { w, h, rgb }));
                    }
                    format!("loaded {p}")
                }
                Err(e) => format!("error {e}"),
            };
            let _ = tx.send(msg);
        });
    }

    fn apply(&mut self, output: Option<String>) {
        let Some(p) = self.items.get(self.sel).cloned() else { return };
        self.msg = match set(&p, output.as_deref(), Some(self.mode)) {
            Ok(()) => {
                self.current = p.clone();
                (format!("wallpaper set{}: {}", output.map(|o| format!(" on {o}")).unwrap_or_else(|| " on all monitors".into()), Pick::label(&p)), false)
            }
            Err(e) => (e, true),
        };
    }

    fn press(&mut self, b: Btn, ctx: &mut Ctx) {
        match b {
            Btn::All => self.apply(None),
            Btn::This => {
                let o = self.outputs.get(self.out_sel).cloned();
                if o.is_some() {
                    self.apply(o);
                }
                self.out_sel = (self.out_sel + 1) % self.outputs.len().max(1);
            }
            Btn::Mode => {
                self.mode = match self.mode {
                    Mode::Fill => Mode::Fit,
                    Mode::Fit => Mode::Center,
                    Mode::Center => Mode::Tile,
                    Mode::Tile => Mode::Fill,
                };
                let _ = set(&choice(&conf(), None).0, None, Some(self.mode));
            }
        }
        ctx.redraw();
    }
}

impl App for Pick {
    fn kind(&self) -> SurfaceKind {
        SurfaceKind::Window { app_id: "shadow-wallpaper".into(), title: "Wallpaper".into(), size: (980, 620) }
    }

    fn message(&mut self, m: String, ctx: &mut Ctx) {
        if let Some(p) = m.strip_prefix("loaded ") {
            if self.loading.as_deref() == Some(p) {
                self.loading = None;
            }
            self.load_preview();
        } else if let Some(e) = m.strip_prefix("error ") {
            self.loading = None;
            self.msg = (e.to_string(), true);
        }
        ctx.redraw();
    }

    fn key(&mut self, k: &Key, ctx: &mut Ctx) {
        let n = self.items.len();
        match k.sym {
            Keysym::Escape | Keysym::q => ctx.quit(),
            Keysym::Up => self.sel = self.sel.saturating_sub(1),
            Keysym::Down => self.sel = (self.sel + 1).min(n.saturating_sub(1)),
            Keysym::Page_Up => self.sel = self.sel.saturating_sub(self.visible),
            Keysym::Page_Down => self.sel = (self.sel + self.visible).min(n.saturating_sub(1)),
            Keysym::Return | Keysym::KP_Enter => self.apply(None),
            Keysym::o => self.press(Btn::This, ctx),
            Keysym::m => self.press(Btn::Mode, ctx),
            _ => return,
        }
        self.load_preview();
        ctx.redraw();
    }

    fn click(&mut self, x: i32, y: i32, ctx: &mut Ctx) {
        if let Some(&(b, _)) = self.btns.iter().find(|(_, r)| r.hit(x, y)) {
            self.press(b, ctx);
        } else if let Some(&(i, _)) = self.rows.iter().find(|(_, r)| r.hit(x, y)) {
            self.sel = i;
            self.load_preview();
            ctx.redraw();
        }
    }

    fn scroll(&mut self, lines: i32, ctx: &mut Ctx) {
        let n = self.items.len() as i32;
        self.sel = (self.sel as i32 + lines).clamp(0, (n - 1).max(0)) as usize;
        self.load_preview();
        ctx.redraw();
    }

    fn draw(&mut self, f: &mut Frame) {
        let th = f.theme.clone();
        let status = format!("{} images · mode {}", self.items.len().saturating_sub(2), self.mode.name());
        let (top, pad) = chrome(f, "WALLPAPER", &status);
        let (w, h) = (f.cv.w as i32, f.cv.h as i32);
        let pt = th.font_pt;
        let lh = f.line_h(pt);
        let row_h = lh + f.px(8.0);
        let split = pad + (w - 2 * pad) * 2 / 5;
        let foot = f.line_h(pt * 0.88) + pad + f.px(14.0);
        // buttons along the bottom of the preview side
        let bh = lh + f.px(12.0);
        let by = h - foot - bh - f.px(6.0);
        self.visible = ((by - top - f.px(8.0)) / row_h).max(1) as usize;
        if self.sel < self.top {
            self.top = self.sel;
        }
        if self.sel >= self.top + self.visible {
            self.top = self.sel + 1 - self.visible;
        }
        self.rows.clear();
        for (i, p) in self.items.iter().enumerate().skip(self.top).take(self.visible) {
            let y = top + (i - self.top) as i32 * row_h;
            let r = Rect { x: pad, y, w: split - pad - f.px(10.0), h: row_h - f.px(2.0) };
            if i == self.sel {
                selection(f, r.x, r.y, r.w, r.h);
            }
            let cur = *p == self.current;
            let l = format!("{}{}", if cur { "● " } else { "" }, Pick::label(p));
            let l = f.fit(&l, pt, i == self.sel, r.w - f.px(20.0));
            f.text(r.x + f.px(14.0), y + f.px(4.0), &l, pt, if p == "default" || p == "vertices" { th.amber } else if cur { th.cyan } else { th.text }, i == self.sel);
            self.rows.push((i, r));
        }
        // preview
        let (px, py, pw, ph) = (split + f.px(6.0), top, w - pad - split - f.px(6.0), by - top - f.px(12.0));
        panel(f, px, py, pw, ph, th.cyan);
        let inset = f.px(8.0);
        let (iw, ih) = ((pw - 2 * inset).max(1) as usize, (ph - 2 * inset).max(1) as usize);
        let sel = self.items.get(self.sel).cloned().unwrap_or_default();
        let pix = if sel == "vertices" {
            Some(shadow_apps::vertex::Field::render_at(iw, ih, &th, Some((0.62, 0.62))))
        } else if sel == "default" {
            Some(default_wallpaper(iw, ih))
        } else {
            self.preview.as_ref().filter(|p| p.0 == sel).map(|(_, img)| compose(img, iw, ih, self.mode))
        };
        match pix {
            Some(buf) => {
                let (ox, oy) = ((px + inset) as usize, (py + inset) as usize);
                for row in 0..ih {
                    let dst = ((oy + row) * f.cv.w + ox) * 4;
                    let src = row * iw * 4;
                    if dst + iw * 4 <= f.cv.buf.len() {
                        f.cv.buf[dst..dst + iw * 4].copy_from_slice(&buf[src..src + iw * 4]);
                    }
                }
            }
            None => {
                let t = if self.loading.is_some() { "loading…" } else { "no preview" };
                f.text(px + pw / 2 - f.px(40.0), py + ph / 2, t, pt, th.dim, false);
            }
        }
        // buttons
        self.btns.clear();
        let out = self.outputs.get(self.out_sel).cloned().unwrap_or_else(|| "—".into());
        let mut bx = px;
        for (b, label) in [(Btn::All, "Set on all".to_string()), (Btn::This, format!("Only {out}")), (Btn::Mode, format!("Mode: {}", self.mode.name()))] {
            if bx > w - pad - f.px(60.0) {
                break;
            }
            let r = button(f, bx, by, &label, b == Btn::All, true);
            self.btns.push((b, r));
            bx += r.w + f.px(8.0);
        }
        if !self.msg.0.is_empty() {
            let m = f.fit(&self.msg.0, pt * 0.9, false, w - 2 * pad);
            f.text(pad, by + bh + f.px(2.0), &m, pt * 0.9, if self.msg.1 { th.crimson } else { th.cyan }, false);
        }
        footer(f, "↑↓ choose · Enter set on all · o one monitor · m mode (fill/fit/center/tile) · Esc close · add images to ~/Pictures/Wallpapers");
    }
}

fn outputs() -> Vec<String> {
    shadow_niri::outputs().map(|o| o.into_iter().filter(|x| x.enabled()).map(|x| x.name).collect()).unwrap_or_default()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let val = |k: &str| a.iter().position(|x| *x == k).and_then(|i| a.get(i + 1)).map(|s| s.to_string());
    match a.as_slice() {
        ["render-default", out, size, ..] => {
            let (w, h) = size.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))).unwrap_or((2560, 1600));
            // render-default OUT WxH [vertices [X,Y]]: the generated image, or the vertex field
            let px = match (a.get(3), a.get(4).and_then(|c| c.split_once(',')).and_then(|(x, y)| Some((x.parse().ok()?, y.parse().ok()?)))) {
                (Some(&"vertices"), cur) => shadow_apps::vertex::Field::render_at(w, h, &shadow_ui::Theme::load(), cur),
                _ => default_wallpaper(w, h),
            };
            let mut o = format!("P6\n{w} {h}\n255\n").into_bytes();
            for p in px.chunks_exact(4) {
                o.extend_from_slice(&[p[2], p[1], p[0]]);
            }
            if let Err(e) = std::fs::write(out, o) {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        ["set", path, ..] => {
            let p = if *path == "default" || *path == "vertices" { path.to_string() } else { std::fs::canonicalize(path).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| path.to_string()) };
            if let Err(e) = set(&p, val("--output").as_deref(), val("--mode").map(|m| Mode::parse(&m))) {
                eprintln!("shadow-wallpaper: {e}");
                std::process::exit(1);
            }
            println!("wallpaper: {p}");
        }
        ["pick", ..] => {
            let (tx, rx) = channel::channel::<String>();
            let t = conf();
            let (current, mode) = choice(&t, None);
            let mut items = vec!["vertices".to_string(), "default".to_string()];
            items.extend(candidates().into_iter().map(|p| p.to_string_lossy().into_owned()));
            let sel = items.iter().position(|p| *p == current).unwrap_or(0);
            let mut p = Pick { items, sel, top: 0, visible: 10, outputs: outputs(), out_sel: 0, mode, preview: None, loading: None, tx, shared: Default::default(), msg: (String::new(), false), btns: vec![], rows: vec![], current };
            p.load_preview();
            if let Err(e) = run(Box::new(p), Some(rx)) {
                eprintln!("shadow-wallpaper: {e}");
                std::process::exit(1);
            }
        }
        [] => {
            let (tx, rx) = channel::channel::<String>();
            signal_thread(tx);
            let w = Wall { text: conf(), images: HashMap::new(), composed: HashMap::new() };
            if let Err(e) = run(Box::new(w), Some(rx)) {
                eprintln!("shadow-wallpaper: {e}");
                std::process::exit(1);
            }
        }
        _ => {
            eprintln!("usage: shadow-wallpaper [pick | set PATH|vertices|default [--output NAME] [--mode fill|fit|center|tile] | render-default OUT.ppm WxH]");
            std::process::exit(2);
        }
    }
}
