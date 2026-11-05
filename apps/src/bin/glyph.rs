//! shadow-glyph — Shadow's desktop shell (9.0): the widget columns fixed to the screen's left and
//! right edges, and the hidden hub in the middle.
//!
//!   shadow-glyph                 both columns (supervised: a crashed column is restarted)
//!   shadow-glyph column left|right
//!   shadow-glyph summon          the hidden hub: a Vitruvian star of glyphs over everything
//!   shadow-glyph toggle          hide / show the columns (Mod+Shift+B)
//!
//! The columns reserve their width (layer-shell exclusive zones), so apps tile in the space
//! between them. LEFT: the chrono dial, system, power, pinned apps. RIGHT: network, the quick
//! settings (brightness, volume, background light, window unfolding — the old side bar's
//! settings), the settings web app's pages, and the session (lock, log out, suspend, restart,
//! shut down). Opening a settings page from a column sends its row to the compositor first
//! (`origin`), so the window unfolds out of the row into full size.
#[path = "../../../bar/src/sys.rs"]
#[allow(dead_code)]
mod hud;
use shadow_apps::cfg::{get_in, set_in};
use shadow_apps::{atomic_write, cmd_out, config_path, spawn};
use shadow_ui::glyphs::G;
use shadow_ui::widgets::Rect;
use shadow_ui::{run, Anchor, App, Ctx, Frame, Key, Keysym, Layer, LayerKeyboard, Ptr, Rgb, SurfaceKind};
use std::collections::VecDeque;
use std::time::Instant;

const COL_W: u32 = 300;

fn brightness() -> Option<f32> {
    let root = std::path::PathBuf::from(std::env::var_os("SHADOW_SYS_ROOT").unwrap_or_else(|| "/".into()));
    let d = std::fs::read_dir(root.join("sys/class/backlight")).ok()?.flatten().next()?.path();
    let r = |f: &str| std::fs::read_to_string(d.join(f)).ok()?.trim().parse::<f32>().ok();
    Some((r("brightness")? / r("max_brightness")?.max(1.0)).clamp(0.0, 1.0))
}

fn volume() -> Option<(f32, bool)> {
    let (c, o) = cmd_out(&["wpctl", "get-volume", "@DEFAULT_AUDIO_SINK@"]);
    if c != 0 {
        return None;
    }
    let v = o.split_whitespace().nth(1)?.parse::<f32>().ok()?;
    Some((v, o.contains("MUTED")))
}

/// (hour, minute, second, "THU 05 NOV")
fn local_time() -> (u32, u32, u32, String) {
    #[repr(C)]
    struct Tm {
        v: [i32; 9],
        gmtoff: i64,
        zone: *const i8,
    }
    extern "C" {
        fn time(t: *mut i64) -> i64;
        fn localtime_r(t: *const i64, tm: *mut Tm) -> *mut Tm;
    }
    // SAFETY: libc calls with valid pointers
    let tm = unsafe {
        let t = time(std::ptr::null_mut());
        let mut tm: Tm = std::mem::zeroed();
        localtime_r(&t, &mut tm);
        tm
    };
    const DAYS: [&str; 7] = ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"];
    const MON: [&str; 12] = ["JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"];
    (tm.v[2] as u32, tm.v[1] as u32, tm.v[0] as u32, format!("{} {:02} {}", DAYS[tm.v[6].clamp(0, 6) as usize], tm.v[3], MON[tm.v[4].clamp(0, 11) as usize]))
}

fn net_bytes() -> (u64, u64) {
    let t = std::fs::read_to_string("/proc/net/dev").unwrap_or_default();
    let (mut rx, mut tx) = (0, 0);
    for l in t.lines().skip(2) {
        let Some((name, rest)) = l.split_once(':') else { continue };
        if name.trim() == "lo" {
            continue;
        }
        let f: Vec<u64> = rest.split_whitespace().filter_map(|x| x.parse().ok()).collect();
        rx += f.first().copied().unwrap_or(0);
        tx += f.get(8).copied().unwrap_or(0);
    }
    (rx, tx)
}

fn human(b: f64) -> String {
    if b >= 1e6 { format!("{:.1} MB/s", b / 1e6) } else if b >= 1e3 { format!("{:.0} kB/s", b / 1e3) } else { format!("{b:.0} B/s") }
}

#[derive(Default)]
struct Stats {
    cpu_prev: (u64, u64),
    cpu: VecDeque<f32>,
    ram: (f32, u64, u64),
    power: hud::Power,
    mode: String,
    wifi: hud::Wifi,
    net_prev: Option<((u64, u64), Instant)>,
    net: (f64, f64),
    temp: Option<i32>,
    bright: Option<f32>,
    vol: Option<(f32, bool)>,
}

impl Stats {
    fn sample(&mut self) {
        let stat = std::fs::read_to_string("/proc/stat").unwrap_or_default();
        if let Some(l) = stat.lines().next() {
            let v: Vec<u64> = l.split_whitespace().skip(1).filter_map(|x| x.parse().ok()).collect();
            let total: u64 = v.iter().sum();
            let idle = v.get(3).copied().unwrap_or(0) + v.get(4).copied().unwrap_or(0);
            let (pt, pi) = self.cpu_prev;
            if pt > 0 && total > pt {
                self.cpu.push_back(1.0 - (idle.saturating_sub(pi)) as f32 / (total - pt) as f32);
                while self.cpu.len() > 48 {
                    self.cpu.pop_front();
                }
            }
            self.cpu_prev = (total, idle);
        }
        let mi = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
        let kb = |k: &str| mi.lines().find_map(|l| l.strip_prefix(k)).and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok()).unwrap_or(0);
        let (total, avail) = (kb("MemTotal:"), kb("MemAvailable:"));
        self.ram = (if total > 0 { (total - avail) as f32 / total as f32 } else { 0.0 }, total - avail.min(total), total);
        self.power = hud::power();
        self.mode = hud::kv("run/shadow/power.state", "mode").unwrap_or_else(|| "auto".into());
        self.wifi = hud::wifi();
        self.temp = hud::package_temp();
        let now = Instant::now();
        let nb = net_bytes();
        if let Some(((r, t), at)) = self.net_prev {
            let dt = now.duration_since(at).as_secs_f64().max(0.1);
            self.net = (nb.0.saturating_sub(r) as f64 / dt, nb.1.saturating_sub(t) as f64 / dt);
        }
        self.net_prev = Some((nb, now));
        self.bright = brightness();
        self.vol = volume();
    }
}

/// what a click on a column row does
#[derive(Clone, Debug, PartialEq)]
enum Act {
    /// run a program; the window unfolds out of the row
    Open(String),
    /// a settings web app page
    Page(&'static str),
    Bright,
    Vol,
    Mute,
    Motion,
    Unfold,
    Mode,
    Session(&'static str),
    Hub,
}

const PINNED: [(&str, G, &str); 6] = [
    ("TERMINAL", G::Terminal, "terminal"),
    ("FILES", G::Files, "shadow-files"),
    ("BROWSER", G::Browser, "xdg-open about:blank"),
    ("LAUNCH", G::Launch, "shadow-launch"),
    ("TASKS", G::Tasks, "shadow-tasks"),
    ("CRAFT", G::Craft, "shadow-craft"),
];

const PAGES: [(&str, G, &str); 5] = [
    ("SETTINGS", G::Settings, "settings"),
    ("FIX CENTER", G::Fix, "fix"),
    ("DEFAULT APPS", G::Desktop, "defaults"),
    ("CONFIG", G::Code, "config"),
    ("CRASH LOG", G::Emergency, "crash"),
];

const SESSION: [(&str, G, &str); 5] = [
    ("LOCK", G::Lock, "lock"),
    ("LOG OUT", G::Logout, "logout"),
    ("SUSPEND", G::Suspend, "suspend"),
    ("RESTART", G::Restart, "reboot"),
    ("SHUT DOWN", G::Power, "poweroff"),
];

fn conf_text() -> String {
    std::fs::read_to_string(config_path()).unwrap_or_default()
}
fn conf_set(section: &str, key: &str, lit: &str) {
    let t = set_in(&conf_text(), section, key, lit);
    let _ = atomic_write(&config_path(), &t);
    // the compositor reads config.toml itself
    let _ = shadow_niri::compositor_command("config-reload");
}

/// tell the compositor where the next window comes from (logical px of the screen)
fn origin(r: (f64, f64, f64, f64)) {
    let _ = shadow_niri::compositor_command(&format!("origin {:.0} {:.0} {:.0} {:.0}", r.0, r.1, r.2, r.3));
}

fn terminal_cmd() -> String {
    shadow_apps::terminal()
}

struct Column {
    right: bool,
    st: Stats,
    hits: Vec<(Act, Rect)>,
    hover: Option<usize>,
    /// logical x of this column's left edge on the screen (for `origin`)
    screen_x: f64,
    scale: f32,
    confirm: Option<(&'static str, Instant)>,
    opened: Instant,
    msg: String,
    pressed: bool,
}

impl Column {
    fn new(right: bool) -> Column {
        let mut st = Stats::default();
        st.sample();
        Column { right, st, hits: vec![], hover: None, screen_x: 0.0, scale: 1.0, confirm: None, opened: Instant::now(), msg: String::new(), pressed: false }
    }

    fn act(&mut self, a: Act, r: Rect, ctx: &mut Ctx) {
        let s = self.scale as f64;
        let row = (self.screen_x + r.x as f64 / s, r.y as f64 / s, r.w as f64 / s, r.h as f64 / s);
        match a {
            Act::Open(cmd) => {
                origin(row);
                let cmd = if cmd == "terminal" { terminal_cmd() } else { cmd };
                spawn(&cmd);
            }
            Act::Page(p) => {
                origin(row);
                spawn(&format!("shadow-settings {p}"));
            }
            Act::Mute => spawn("wpctl set-mute @DEFAULT_AUDIO_SINK@ toggle"),
            Act::Motion => {
                let cur = get_in(&conf_text(), "look", "motion").unwrap_or_else(|| "\"on\"".into());
                let next = match cur.trim_matches('"') { "on" => "low", "low" => "off", _ => "on" };
                conf_set("look", "motion", &format!("\"{next}\""));
            }
            Act::Unfold => {
                let cur = get_in(&conf_text(), "layout", "unfold_ms").and_then(|v| v.parse::<u64>().ok()).unwrap_or(560);
                conf_set("layout", "unfold_ms", if cur == 0 { "560" } else { "0" });
            }
            Act::Mode => {
                const M: [&str; 6] = ["auto", "performance", "balanced", "quiet", "cool", "low-power"];
                let i = M.iter().position(|m| *m == self.st.mode).map(|i| (i + 1) % M.len()).unwrap_or(0);
                spawn(&format!("shadow power mode {}", M[i]));
                self.st.mode = M[i].into();
            }
            Act::Session(what) => {
                // the destructive ones need a second click within 3 s
                let risky = matches!(what, "reboot" | "poweroff" | "logout");
                if risky && !self.confirm.is_some_and(|(w, t)| w == what && t.elapsed().as_secs() < 3) {
                    self.confirm = Some((what, Instant::now()));
                    self.msg = format!("click {} again to confirm", what.to_uppercase());
                } else {
                    self.confirm = None;
                    self.msg.clear();
                    match what {
                        "lock" => spawn("shadow-lock"),
                        "logout" => {
                            let _ = shadow_niri::compositor_command("action quit");
                        }
                        "suspend" => spawn("systemctl suspend"),
                        "reboot" => spawn("systemctl reboot"),
                        _ => spawn("systemctl poweroff"),
                    }
                }
            }
            Act::Hub => spawn("shadow-glyph summon"),
            Act::Bright | Act::Vol => {}
        }
        ctx.redraw();
    }

    /// a slider set from a click / drag position
    fn slide(&mut self, a: &Act, r: Rect, x: i32) {
        let v = ((x - r.x) as f32 / r.w.max(1) as f32).clamp(0.0, 1.0);
        match a {
            Act::Bright => {
                spawn(&format!("shadow-displayd brightness {}%", (v * 100.0).round().max(1.0)));
                self.st.bright = Some(v);
            }
            Act::Vol => {
                spawn(&format!("wpctl set-volume @DEFAULT_AUDIO_SINK@ {v:.2}"));
                self.st.vol = Some((v, false));
            }
            _ => {}
        }
    }
}

/// a section header: a pivot node, the name in Shadow Glyph, a graduated rule
fn header(f: &mut Frame, x: i32, y: i32, w: i32, name: &str, g: G) -> i32 {
    let th = f.theme.clone();
    let pt = th.font_pt * 0.95;
    let gp = f.px(18.0);
    f.cv.glyph_icon(g, x, y, gp, th.amber, 0.95);
    let tx = x + gp + f.px(8.0);
    let lh = f.line_h(pt);
    let tw = f.title(tx, y + (gp - lh) / 2, name, pt, th.cyan, 1.0);
    let ry = y + gp / 2;
    let rx = tx + tw + f.px(8.0);
    f.cv.wline(rx as f32, ry as f32, (x + w) as f32, ry as f32, th.amber, 0.45);
    // ruler graduations
    let step = f.px(8.0).max(4);
    let mut tx2 = rx;
    let mut i = 0;
    while tx2 < x + w {
        let h = if i % 5 == 0 { f.px(5.0) } else { f.px(2.5) };
        f.cv.wline(tx2 as f32, ry as f32, tx2 as f32, (ry + h) as f32, th.amber, 0.35);
        tx2 += step;
        i += 1;
    }
    gp + f.px(10.0)
}

/// a construction slider: a graduated line, the value as a filled run and a diamond pivot
fn slider(f: &mut Frame, x: i32, y: i32, w: i32, v: Option<f32>, c: Rgb) {
    let th = f.theme.clone();
    let cy = y as f32;
    f.cv.wline(x as f32, cy, (x + w) as f32, cy, th.violet, 0.8);
    for i in 0..=10 {
        let tx = x as f32 + w as f32 * i as f32 / 10.0;
        let h = if i % 5 == 0 { f.px(5.0) } else { f.px(3.0) } as f32;
        f.cv.wline(tx, cy - h, tx, cy + h, th.amber, 0.35);
    }
    if let Some(v) = v {
        let vx = x as f32 + w as f32 * v.clamp(0.0, 1.0);
        f.cv.tline(x as f32, cy, vx, cy, f.px(2.4) as f32, c, 0.95);
        let r = f.px(5.0) as f32;
        let pts = [(vx, cy - r), (vx + r, cy), (vx, cy + r), (vx - r, cy)];
        f.cv.poly(&pts, 1.0, &|_, _| th.cyan);
    }
}

impl App for Column {
    fn kind(&self) -> SurfaceKind {
        let w = std::env::var("SHADOW_COLUMN_W").ok().and_then(|v| v.parse().ok()).unwrap_or(COL_W);
        SurfaceKind::Layer {
            namespace: if self.right { "shadow-glyph-right".into() } else { "shadow-glyph-left".into() },
            layer: Layer::Top,
            anchor: Anchor::TOP | Anchor::BOTTOM | if self.right { Anchor::RIGHT } else { Anchor::LEFT },
            size: (w, 0),
            margin: (0, 0, 0, 0),
            exclusive: w as i32,
            keyboard: LayerKeyboard::OnDemand,
        }
    }

    fn pick_output(&mut self, outs: &[shadow_ui::OutputDesc]) -> Option<String> {
        // the right column's screen position, for `origin`
        if self.right {
            if let Some(o) = outs.first() {
                let sc = o.ideal_scale().unwrap_or(1.0);
                self.screen_x = o.px.0 as f64 / sc - COL_W as f64;
            }
        }
        None
    }

    fn tick_ms(&self) -> u64 {
        2000
    }

    fn tick(&mut self, ctx: &mut Ctx) {
        self.st.sample();
        if self.confirm.is_some_and(|(_, t)| t.elapsed().as_secs() >= 3) {
            self.confirm = None;
            self.msg.clear();
        }
        ctx.redraw();
    }

    fn pointer(&mut self, ev: Ptr, x: i32, y: i32, _s: usize, ctx: &mut Ctx) {
        let h = self.hits.iter().position(|(_, r)| r.hit(x, y));
        if h != self.hover {
            self.hover = h;
            ctx.redraw();
        }
        match ev {
            Ptr::Down => self.pressed = true,
            Ptr::Up | Ptr::Leave => self.pressed = false,
            Ptr::Move => {}
        }
        // dragging a slider
        if matches!(ev, Ptr::Move) && self.pressed {
            if let Some((a, r)) = self.hits.iter().find(|(a, r)| matches!(a, Act::Bright | Act::Vol) && r.hit(x, y)).cloned() {
                self.slide(&a, r, x);
                ctx.redraw();
            }
        }
    }

    fn click(&mut self, x: i32, y: i32, ctx: &mut Ctx) {
        if let Some((a, r)) = self.hits.iter().find(|(_, r)| r.hit(x, y)).cloned() {
            if matches!(a, Act::Bright | Act::Vol) {
                self.slide(&a, r, x);
                ctx.redraw();
            } else {
                self.act(a, r, ctx);
            }
        }
    }

    fn scroll(&mut self, lines: i32, ctx: &mut Ctx) {
        // over a slider: nudge it
        if let Some(i) = self.hover {
            match self.hits[i].0 {
                Act::Bright => spawn(&format!("shadow-displayd brightness 5%{}", if lines < 0 { "+" } else { "-" })),
                Act::Vol => spawn(&format!("wpctl set-volume -l 1.0 @DEFAULT_AUDIO_SINK@ 5%{}", if lines < 0 { "+" } else { "-" })),
                _ => return,
            }
            self.st.bright = brightness();
            self.st.vol = volume();
            ctx.redraw();
        }
    }

    fn draw(&mut self, f: &mut Frame) {
        self.scale = f.scale;
        let th = f.theme.clone();
        let (w, h) = (f.cv.w as i32, f.cv.h as i32);
        f.cv.clear(th.bg, 0.0);
        // glass and the inner edge: a gold rule with graduations every 40 px
        let rise = (self.opened.elapsed().as_secs_f32() / 0.5).min(1.0);
        f.cv.fill(0, 0, w, h, th.bg, 0.82);
        let ex = if self.right { 0 } else { w - 1 };
        f.cv.fill(ex, 0, 1, h, th.amber, 0.55);
        let mut y = f.px(12.0);
        while y < h {
            let l = if (y / f.px(40.0)) % 5 == 0 { f.px(9.0) } else { f.px(5.0) };
            let x0 = if self.right { 0 } else { w - l };
            f.cv.fill(x0, y, l, 1, th.amber, 0.4);
            y += f.px(40.0);
        }
        self.hits.clear();
        let pad = f.px(18.0);
        let x = pad;
        let cw = w - 2 * pad;
        let pt = th.font_pt;
        let small = pt * 0.82;
        let lh = f.line_h(small);
        let mut y = pad;
        let hover = self.hover;
        let hl = |f: &mut Frame, r: Rect, i: usize| {
            if hover == Some(i) {
                f.cv.fill(r.x, r.y, r.w, r.h, th.amber, 0.08);
                f.cv.fill(r.x, r.y + r.h - 1, r.w, 1, th.cyan, 0.5);
            }
        };
        if !self.right {
            // ── CHRONO: a Vitruvian dial (circle in a square, hands) and the time in Shadow Glyph
            y += header(f, x, y, cw, "CHRONO", G::Clock);
            let (hh, mm, ss, date) = local_time();
            let d = f.px(84.0);
            let (cx, cy) = ((x + d / 2) as f32, (y + d / 2) as f32);
            let r = d as f32 / 2.0;
            f.cv.border(x, y, d, d, 1, th.amber, 0.35);
            for i in 0..60 {
                let a = i as f32 / 60.0 * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
                let l = if i % 5 == 0 { 0.16 } else { 0.07 };
                f.cv.wline(cx + a.cos() * r * (0.96 - l), cy + a.sin() * r * (0.96 - l), cx + a.cos() * r * 0.96, cy + a.sin() * r * 0.96, th.amber, if i % 5 == 0 { 0.8 } else { 0.35 });
            }
            let hand = |f: &mut Frame, frac: f32, len: f32, wdt: f32, c: Rgb| {
                let a = frac * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
                f.cv.tline(cx, cy, cx + a.cos() * r * len, cy + a.sin() * r * len, wdt, c, 0.95);
            };
            hand(f, ((hh % 12) as f32 + mm as f32 / 60.0) / 12.0, 0.5, f.px(2.2) as f32, th.amber);
            hand(f, (mm as f32 + ss as f32 / 60.0) / 60.0, 0.78, f.px(1.4) as f32, th.cyan);
            f.cv.node(cx, cy, f.px(2.5) as f32, th.cyan, 1.0);
            let tx = x + d + f.px(14.0);
            f.title(tx, y + f.px(4.0), &format!("{hh:02}:{mm:02}"), pt * 2.3, th.text, rise);
            let tlh = f.line_h(pt * 2.3);
            f.title(tx, y + f.px(4.0) + tlh, &date, small, th.dim, 1.0);
            y += d + f.px(22.0);

            // ── SYSTEM
            y += header(f, x, y, cw, "SYSTEM", G::System);
            let cpu = self.st.cpu.back().copied().unwrap_or(0.0);
            f.text(x, y, &format!("CPU {:>3.0}%", cpu * 100.0), small, th.text, false);
            if let Some(t) = self.st.temp {
                let s = format!("{t}°C");
                let sw = f.text_w(&s, small, false);
                f.text(x + cw - sw, y, &s, small, th.dim, false);
            }
            y += lh + f.px(2.0);
            // the history as construction bars
            let n = self.st.cpu.len().max(1);
            let bw = (cw as f32 / 48.0).max(1.0);
            let bh = f.px(26.0) as f32;
            for (i, v) in self.st.cpu.iter().enumerate() {
                let bx = x as f32 + (48 - n + i) as f32 * bw;
                let hh = (v * bh).max(1.0);
                f.cv.fill(bx as i32, (y as f32 + bh - hh) as i32, (bw - 1.0).max(1.0) as i32, hh as i32, th.amber, 0.35 + 0.5 * v);
            }
            y += bh as i32 + f.px(8.0);
            let (rf, used, total) = self.st.ram;
            f.text(x, y, &format!("RAM {:.1} / {:.1} GiB", used as f64 / 1048576.0, total as f64 / 1048576.0), small, th.text, false);
            y += lh + f.px(6.0);
            slider(f, x, y, cw, Some(rf), th.amber);
            y += f.px(24.0);

            // ── POWER
            y += header(f, x, y, cw, "POWER", G::Battery);
            let bat = self.st.power.battery.map(|b| format!("{b}%")).unwrap_or_else(|| "AC".into());
            f.title(x, y, &bat, pt * 1.6, if self.st.power.battery.is_some_and(|b| b < 15) { th.crimson } else { th.text }, 1.0);
            let src = if self.st.power.battery.is_none() { "NO BATTERY" } else if self.st.power.ac { "ON MAINS" } else { "ON BATTERY" };
            let sw = f.text_w(src, small, false);
            f.text(x + cw - sw, y + f.px(6.0), src, small, th.dim, false);
            y += f.line_h(pt * 1.6) + f.px(4.0);
            let r = Rect { x: x - f.px(4.0), y: y - f.px(3.0), w: cw + f.px(8.0), h: lh + f.px(8.0) };
            hl(f, r, self.hits.len());
            f.text(x, y, &format!("MODE · {}", self.st.mode.to_uppercase()), small, th.cyan, false);
            let s = "click to change";
            let sw = f.text_w(s, small * 0.9, false);
            f.text(x + cw - sw, y + f.px(1.0), s, small * 0.9, th.dim, false);
            self.hits.push((Act::Mode, r));
            y += lh + f.px(22.0);

            // ── PINNED: glyph cartouches, 3 × 2
            y += header(f, x, y, cw, "APPS", G::Launch);
            let gap = f.px(10.0);
            let cell = (cw - 2 * gap) / 3;
            for (i, (name, g, cmd)) in PINNED.iter().enumerate() {
                let (cx, cy) = (x + (i % 3) as i32 * (cell + gap), y + (i / 3) as i32 * (cell + gap + lh));
                let r = Rect { x: cx, y: cy, w: cell, h: cell + lh };
                let k = self.hits.len();
                let on = hover == Some(k);
                let gp = (cell as f32 * 0.72) as i32;
                f.cv.glyph_icon(*g, cx + (cell - gp) / 2, cy + (cell - gp) / 2, gp, if on { th.cyan } else { th.amber }, if on { 1.0 } else { 0.85 });
                let nw = f.title_w(name, small * 0.8);
                f.title(cx + (cell - nw) / 2, cy + cell, name, small * 0.8, if on { th.cyan } else { th.dim }, 1.0);
                self.hits.push((Act::Open(cmd.to_string()), r));
            }
            y += 2 * (cell + gap + lh);
            // the hidden hub
            let r = Rect { x, y, w: cw, h: lh + f.px(10.0) };
            let k = self.hits.len();
            hl(f, r, k);
            f.cv.border(r.x, r.y, r.w, r.h, 1, th.amber, 0.4);
            let s = "◇  HUB  ·  MOD + SPACE";
            let sw = f.title_w(s, small);
            f.title(x + (cw - sw) / 2, y + f.px(5.0), s, small, th.amber, 1.0);
            self.hits.push((Act::Hub, r));
        } else {
            // ── NETWORK
            y += header(f, x, y, cw, "NETWORK", G::Network);
            let r = Rect { x: x - f.px(4.0), y: y - f.px(3.0), w: cw + f.px(8.0), h: 2 * lh + f.px(10.0) };
            hl(f, r, self.hits.len());
            let ssid = if self.st.wifi.ssid.is_empty() { "wired / offline".to_string() } else { self.st.wifi.ssid.clone() };
            let s = f.fit(&ssid, pt, true, cw);
            f.text(x, y, &s, pt, th.text, true);
            y += f.line_h(pt);
            f.text(x, y, &format!("↓ {}   ↑ {}", human(self.st.net.0), human(self.st.net.1)), small, th.dim, false);
            self.hits.push((Act::Open("shadow-net".into()), r));
            y += lh + f.px(22.0);

            // ── QUICK: the side bar's settings
            y += header(f, x, y, cw, "QUICK", G::Brightness);
            for (label, act, v) in [("BRIGHTNESS", Act::Bright, self.st.bright), ("VOLUME", Act::Vol, self.st.vol.map(|v| v.0))] {
                let muted = act == Act::Vol && self.st.vol.is_some_and(|v| v.1);
                f.title(x, y, label, small * 0.9, th.dim, 1.0);
                let pct = v.map(|v| format!("{:.0}%{}", v * 100.0, if muted { " MUTED" } else { "" })).unwrap_or_else(|| "—".into());
                let pw = f.text_w(&pct, small, false);
                f.text(x + cw - pw, y, &pct, small, if muted { th.crimson } else { th.text }, false);
                y += lh + f.px(8.0);
                let r = Rect { x, y: y - f.px(9.0), w: cw, h: f.px(18.0) };
                hl(f, r, self.hits.len());
                slider(f, x, y, cw, v, if muted { th.dim } else { th.amber });
                self.hits.push((act, r));
                y += f.px(18.0);
            }
            let text = conf_text();
            let motion = get_in(&text, "look", "motion").map(|v| v.trim_matches('"').to_uppercase()).unwrap_or_else(|| "ON".into());
            let unfold = get_in(&text, "layout", "unfold_ms").and_then(|v| v.parse::<u64>().ok()).unwrap_or(560) > 0;
            for (label, val, act) in [("MUTE", if self.st.vol.is_some_and(|v| v.1) { "ON".to_string() } else { "OFF".to_string() }, Act::Mute), ("BACKGROUND LIGHT", motion, Act::Motion), ("UNFOLDING", (if unfold { "ON" } else { "OFF" }).to_string(), Act::Unfold)] {
                let r = Rect { x: x - f.px(4.0), y: y - f.px(3.0), w: cw + f.px(8.0), h: lh + f.px(6.0) };
                hl(f, r, self.hits.len());
                f.title(x, y, label, small * 0.9, th.text, 1.0);
                let vw = f.title_w(&val, small * 0.9);
                f.title(x + cw - vw, y, &val, small * 0.9, th.cyan, 1.0);
                self.hits.push((act, r));
                y += lh + f.px(8.0);
            }
            y += f.px(14.0);

            // ── SETTINGS: pages of the settings web app; each unfolds out of its row
            y += header(f, x, y, cw, "SETTINGS", G::Settings);
            for (name, g, page) in PAGES {
                let r = Rect { x: x - f.px(4.0), y: y - f.px(3.0), w: cw + f.px(8.0), h: lh + f.px(10.0) };
                let k = self.hits.len();
                hl(f, r, k);
                let gp = lh + f.px(4.0);
                f.cv.glyph_icon(g, x, y - f.px(1.0), gp, if hover == Some(k) { th.cyan } else { th.amber }, 0.9);
                f.title(x + gp + f.px(10.0), y + f.px(2.0), name, small, th.text, 1.0);
                let s = "↗";
                let sw = f.text_w(s, small, false);
                f.text(x + cw - sw, y + f.px(2.0), s, small, th.dim, false);
                self.hits.push((Act::Page(page), r));
                y += lh + f.px(12.0);
            }
            y += f.px(10.0);

            // ── SESSION, at the bottom edge
            let rows = SESSION.len() as i32;
            let sy = (h - pad - rows * (lh + f.px(12.0)) - f.px(34.0) - lh).max(y);
            let mut y = sy;
            y += header(f, x, y, cw, "SESSION", G::User);
            for (name, g, what) in SESSION {
                let r = Rect { x: x - f.px(4.0), y: y - f.px(3.0), w: cw + f.px(8.0), h: lh + f.px(10.0) };
                let k = self.hits.len();
                hl(f, r, k);
                let gp = lh + f.px(4.0);
                let armed = self.confirm.is_some_and(|(w, _)| w == what);
                let c = if armed { th.crimson } else if hover == Some(k) { th.cyan } else { th.amber };
                f.cv.glyph_icon(g, x, y - f.px(1.0), gp, c, 0.9);
                f.title(x + gp + f.px(10.0), y + f.px(2.0), name, small, if armed { th.crimson } else { th.text }, 1.0);
                self.hits.push((Act::Session(what), r));
                y += lh + f.px(12.0);
            }
            if !self.msg.is_empty() {
                let m = f.fit(&self.msg, small * 0.9, false, cw);
                f.text(x, y, &m, small * 0.9, th.crimson, false);
            }
        }
    }
}

// ── the hidden hub ──────────────────────────────────────────────────────────────────────────────

const HUB: [(&str, G, &str); 10] = [
    ("TERMINAL", G::Terminal, "terminal"),
    ("FILES", G::Files, "shadow-files"),
    ("LAUNCH", G::Launch, "shadow-launch"),
    ("NETWORK", G::Network, "shadow-net"),
    ("TASKS", G::Tasks, "shadow-tasks"),
    ("CRAFT", G::Craft, "shadow-craft"),
    ("SETTINGS", G::Settings, "shadow-settings settings"),
    ("FIX", G::Fix, "shadow-settings fix"),
    ("UPDATE", G::Update, "shadow-update"),
    ("LOCK", G::Lock, "shadow-lock"),
];

struct Hub {
    opened: Instant,
    closing: Option<Instant>,
    sel: Option<usize>,
    nodes: Vec<Rect>,
    scale: f32,
    typed: String,
}

impl Hub {
    fn launch(&mut self, i: usize, ctx: &mut Ctx) {
        let (_, _, cmd) = HUB[i];
        if let Some(r) = self.nodes.get(i) {
            let s = self.scale as f64;
            origin((r.x as f64 / s, r.y as f64 / s, r.w as f64 / s, r.h as f64 / s));
        }
        spawn(&if cmd == "terminal" { terminal_cmd() } else { cmd.to_string() });
        self.close(ctx);
    }
    fn close(&mut self, ctx: &mut Ctx) {
        if self.closing.is_none() {
            self.closing = Some(Instant::now());
        }
        ctx.redraw();
    }
    /// 0..1: unfolding in, then folding out when closing
    fn k(&self) -> f32 {
        let open = (self.opened.elapsed().as_secs_f32() / 0.42).min(1.0);
        let close = self.closing.map(|t| 1.0 - (t.elapsed().as_secs_f32() / 0.24).min(1.0)).unwrap_or(1.0);
        open.min(close)
    }
}

impl App for Hub {
    fn kind(&self) -> SurfaceKind {
        SurfaceKind::Layer {
            namespace: "shadow-glyph-hub".into(),
            layer: Layer::Overlay,
            anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
            size: (0, 0),
            margin: (0, 0, 0, 0),
            exclusive: -1,
            keyboard: LayerKeyboard::Exclusive,
        }
    }
    fn tick_ms(&self) -> u64 {
        16
    }
    fn tick(&mut self, ctx: &mut Ctx) {
        if self.closing.is_some_and(|t| t.elapsed().as_secs_f32() > 0.25) {
            ctx.quit();
            return;
        }
        if self.opened.elapsed().as_secs_f32() < 0.5 || self.closing.is_some() {
            ctx.redraw();
        }
    }
    fn key(&mut self, k: &Key, ctx: &mut Ctx) {
        match k.sym {
            Keysym::Escape => self.close(ctx),
            Keysym::Left | Keysym::Up => self.sel = Some(self.sel.map(|s| (s + HUB.len() - 1) % HUB.len()).unwrap_or(0)),
            Keysym::Right | Keysym::Down | Keysym::Tab => self.sel = Some(self.sel.map(|s| (s + 1) % HUB.len()).unwrap_or(0)),
            Keysym::Return | Keysym::KP_Enter => {
                if let Some(s) = self.sel {
                    self.launch(s, ctx);
                    return;
                }
            }
            _ => {
                // typing goes to the launcher
                if let Some(t) = k.text.as_ref().filter(|t| t.chars().all(|c| !c.is_control()) && !t.is_empty()) {
                    self.typed.push_str(t);
                    spawn(&format!("shadow-launch --query {}", shadow_apps::sh_quote(&self.typed)));
                    self.close(ctx);
                    return;
                }
                return;
            }
        }
        ctx.redraw();
    }
    fn pointer(&mut self, _ev: Ptr, x: i32, y: i32, _s: usize, ctx: &mut Ctx) {
        let s = self.nodes.iter().position(|r| r.hit(x, y));
        if s != self.sel && s.is_some() {
            self.sel = s;
            ctx.redraw();
        }
    }
    fn click(&mut self, x: i32, y: i32, ctx: &mut Ctx) {
        match self.nodes.iter().position(|r| r.hit(x, y)) {
            Some(i) => self.launch(i, ctx),
            None => self.close(ctx),
        }
    }
    fn draw(&mut self, f: &mut Frame) {
        self.scale = f.scale;
        let th = f.theme.clone();
        let (w, h) = (f.cv.w as f32, f.cv.h as f32);
        let k = self.k();
        let e = 1.0 - (1.0 - k).powi(3);
        f.cv.clear(th.bg, 0.55 * e);
        let (cx, cy) = (w / 2.0, h / 2.0);
        let rr = h.min(w) * 0.34 * (0.6 + 0.4 * e);
        // the Vitruvian construction: circle, the square sharing its base, rays to each glyph
        let seg = 96;
        for i in 0..seg {
            if (i as f32 / seg as f32) > e {
                break; // the circle is drawn in like a compass stroke
            }
            let a0 = i as f32 / seg as f32 * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
            let a1 = (i + 1) as f32 / seg as f32 * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
            f.cv.wline(cx + a0.cos() * rr, cy + a0.sin() * rr, cx + a1.cos() * rr, cy + a1.sin() * rr, th.amber, 0.7);
        }
        let side = rr * 1.64;
        let sq = (cx - side / 2.0, cy + rr - side);
        let sa = 0.45 * e;
        for (a, b) in [((sq.0, sq.1), (sq.0 + side, sq.1)), ((sq.0 + side, sq.1), (sq.0 + side, sq.1 + side)), ((sq.0 + side, sq.1 + side), (sq.0, sq.1 + side)), ((sq.0, sq.1 + side), (sq.0, sq.1))] {
            f.cv.wline(a.0, a.1, a.0 + (b.0 - a.0) * e, a.1 + (b.1 - a.1) * e, th.amber, sa);
        }
        // the central star
        for i in 0..8 {
            let a = i as f32 / 8.0 * std::f32::consts::TAU;
            let l = if i % 2 == 0 { rr * 0.18 } else { rr * 0.09 };
            f.cv.wline(cx, cy, cx + a.cos() * l * e, cy + a.sin() * l * e, th.cyan, 0.9);
        }
        f.cv.node(cx, cy, f.px(3.0) as f32, th.cyan, 1.0);
        // the glyph nodes, each unfolding in after the one before
        self.nodes.clear();
        let n = HUB.len();
        let gp = f.px(56.0);
        for (i, (name, g, _)) in HUB.iter().enumerate() {
            let a = i as f32 / n as f32 * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
            let ki = ((k * 1.6) - i as f32 * 0.06).clamp(0.0, 1.0);
            let (nx, ny) = (cx + a.cos() * rr, cy + a.sin() * rr);
            // ray with arrow ticks toward the node
            let (ix, iy) = (cx + a.cos() * rr * 0.24, cy + a.sin() * rr * 0.24);
            let (ox, oy) = (cx + a.cos() * (rr - gp as f32 * 0.7), cy + a.sin() * (rr - gp as f32 * 0.7));
            f.cv.wline(ix, iy, ix + (ox - ix) * ki, iy + (oy - iy) * ki, th.amber, 0.5);
            let (mx, my) = (ix + (ox - ix) * 0.55, iy + (oy - iy) * 0.55);
            let (px_, py_) = (-a.sin(), a.cos());
            let t = f.px(5.0) as f32;
            f.cv.wline(mx + px_ * t, my + py_ * t, mx - px_ * t, my - py_ * t, th.amber, 0.5 * ki);
            let on = self.sel == Some(i);
            let s = (gp as f32 * (0.7 + 0.3 * ki)) as i32;
            f.cv.glyph_icon(*g, (nx - s as f32 / 2.0) as i32, (ny - s as f32 / 2.0) as i32, s, if on { th.cyan } else { th.amber }, ki);
            let tw = f.title_w(name, th.font_pt * 0.8);
            f.title((nx - tw as f32 / 2.0) as i32, (ny + s as f32 / 2.0 + f.px(4.0) as f32) as i32, name, th.font_pt * 0.8, if on { th.cyan } else { th.dim }, ki);
            self.nodes.push(Rect { x: (nx - gp as f32 / 2.0) as i32, y: (ny - gp as f32 / 2.0) as i32, w: gp, h: gp + f.px(18.0) });
        }
        let hint = "CLICK A GLYPH · ARROWS + ENTER · TYPE TO SEARCH · ESC";
        let hw = f.title_w(hint, th.font_pt * 0.75);
        f.title(((w - hw as f32) / 2.0) as i32, (cy + rr + gp as f32 * 0.5 + f.px(34.0) as f32).min(h - f.px(30.0) as f32) as i32, hint, th.font_pt * 0.75, th.dim, e);
    }
}

// ── supervisor ──────────────────────────────────────────────────────────────────────────────────

/// processes matching `pattern` that belong to this Wayland session (a dead session's leftovers
/// don't count)
fn ours(pattern: &str) -> usize {
    let me = std::process::id().to_string();
    let want = (std::env::var("XDG_RUNTIME_DIR").unwrap_or_default(), std::env::var("WAYLAND_DISPLAY").unwrap_or_default());
    cmd_out(&["pgrep", "-f", pattern]).1.lines().filter(|pid| *pid != me).filter(|pid| {
        let env = std::fs::read(format!("/proc/{pid}/environ")).unwrap_or_default();
        let get = |k: &str| env.split(|b| *b == 0).find_map(|kv| std::str::from_utf8(kv).ok()?.strip_prefix(k).map(String::from)).unwrap_or_default();
        (get("XDG_RUNTIME_DIR="), get("WAYLAND_DISPLAY=")) == want
    }).count()
}

fn columns_running() -> bool {
    ours("^(/[^ ]*/)?shadow-glyph column") > 0
}

fn supervise() -> i32 {
    use std::process::Command;
    let me = std::env::current_exe().unwrap_or_else(|_| "shadow-glyph".into());
    let start = |side: &str| Command::new(&me).args(["column", side]).spawn().ok();
    let mut kids = [start("left"), start("right")];
    let mut crashes: Vec<Instant> = vec![];
    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        for (i, side) in ["left", "right"].iter().enumerate() {
            let exited = match kids[i].as_mut() {
                Some(c) => c.try_wait().ok().flatten(),
                None => None,
            };
            if let Some(st) = exited {
                // a clean exit (toggle, session end) ends the supervisor too
                if st.success() || !shadow_niri::session_alive() {
                    return 0;
                }
                crashes.retain(|t| t.elapsed().as_secs() < 120);
                crashes.push(Instant::now());
                if crashes.len() > 5 {
                    eprintln!("shadow-glyph: the {side} column keeps crashing — giving up (see the crash log)");
                    return 1;
                }
                kids[i] = start(side);
            }
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let r = match a.as_slice() {
        [] => {
            if columns_running() {
                eprintln!("shadow-glyph: already running");
                0
            } else {
                supervise()
            }
        }
        ["column", side] => run(Box::new(Column::new(*side == "right")), None).map(|_| 0).unwrap_or_else(|e| {
            eprintln!("shadow-glyph: {e}");
            1
        }),
        ["summon", ..] => {
            if ours("^(/[^ ]*/)?shadow-glyph summon") > 0 {
                return; // already open
            }
            run(Box::new(Hub { opened: Instant::now(), closing: None, sel: None, nodes: vec![], scale: 1.0, typed: String::new() }), None).map(|_| 0).unwrap_or(1)
        }
        ["toggle"] => {
            if columns_running() {
                let _ = cmd_out(&["pkill", "-f", "shadow-glyph column"]);
                let _ = cmd_out(&["pkill", "-x", "shadow-glyph"]);
            } else {
                spawn("shadow-glyph");
            }
            0
        }
        _ => {
            eprintln!("usage: shadow-glyph [column left|right | summon | toggle]");
            2
        }
    };
    std::process::exit(r);
}
