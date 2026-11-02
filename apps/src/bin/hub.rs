//! shadow-hub: the desktop hub. It replaces the side bar and the wallpaper in 8.0.
//!
//! Usage:
//!
//!   shadow-hub                  the desktop. It is a background layer, one surface per monitor.
//!                               It draws the 3D vertex field (or your wallpaper image) plus the hub
//!                               on the main monitor.
//!   shadow-hub summon [PAGE]    the hub over your windows. It uses the overlay layer and takes the
//!                               keyboard. Esc, or a click on empty space, closes it.
//!   shadow-hub reset-layout     puts the widgets back where they started.
//!   shadow-hub doctor           checks what the hub needs on this machine and prints what it finds.
//!
//! The desktop instance runs under a small supervisor (the same program). If the hub crashes, the
//! supervisor restarts it. After 3 crashes within 2 minutes it starts the side bar and the wallpaper
//! instead, so you never end up with an empty desktop, and tells you. Its output goes to
//! ~/.local/state/shadow-tui/hub.log.
//!
//! The hub (vertex wireframe glass):
//!   * A static core sits in the middle of the screen. Click it, or press Space, and eight option
//!     nodes slide out of it: APPS, SETTINGS, POWER, SYSTEM, NETWORK, FIX, LOOK and TASKS.
//!   * Clicking a node (or pressing Tab / its number) drops its glass panel open beside the node.
//!   * Floating widgets show CLOCK, POWER, SYSTEM (CPU / RAM / NPU), NETWORK and WORKSPACES.
//!     - Each is wired to the core by an elbow connector with vertex nodes at the joints.
//!     - Drag a widget to move it. Positions are kept in [hub.widgets] as fractions of the screen.
//!
//! The background redraws only while the pointer moves over it, plus about 1.5 s of easing. The hub
//! itself is drawn into its own layer and redrawn only when something in it changes (menus, drags,
//! the widgets' numbers every 3 s); frames where only the field moves just composite that layer.
//!
//! Config, in ~/.config/shadow-state/config.toml:
//!
//!   [hub]
//!   enabled = true       # false: the 7.x side bar + wallpaper come back at the next login
//!   output = "eDP-1"     # the monitor that shows the hub (default: the first one)
//!   reactive = true      # false: the vertex field does not follow the cursor
//!   [hub.widgets]
//!   clock = "0.150,0.200"
//!   power = "off"        # hide a widget
#[path = "../../../bar/src/sys.rs"]
#[allow(dead_code)]
mod hud;

use shadow_apps::cfg::{get_in, set_in};
use shadow_apps::desktop::{self, Entry};
use shadow_apps::vertex::Field;
use shadow_apps::wall::{self, Img, Mode};
use shadow_apps::{atomic_write, cmd_out, config_path, spawn};
use shadow_ui::calloop::channel::{self, Sender};
use shadow_ui::canvas::lerp;
use shadow_ui::widgets::{chamfer_pts, glass, meter, sparkline, Input, Rect};
use shadow_ui::{run, Anchor, App, Ctx, Frame, Key, Keysym, Layer, LayerKeyboard, Ptr, Rgb, SurfaceKind};
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::os::unix::process::CommandExt;
use std::time::Instant;

const PAGES: [&str; 8] = ["APPS", "SETTINGS", "POWER", "SYSTEM", "NETWORK", "FIX", "LOOK", "TASKS"];
const MODES: [(&str, &str); 6] = [("auto", "Auto"), ("performance", "Performance"), ("balanced", "Balanced"), ("quiet", "Quiet"), ("cool", "Cool"), ("low-power", "Low power")];
const WIDGETS: [(&str, f32, f32); 5] = [("clock", 0.15, 0.20), ("power", 0.85, 0.22), ("system", 0.15, 0.74), ("network", 0.85, 0.74), ("workspaces", 0.50, 0.90)];
/// panel width (logical px)
const PANEL_W: f32 = 340.0;

fn charge_label(m: &str) -> &'static str {
    match m {
        "smart" => "Smart",
        "standard" => "Standard",
        "adaptive" => "Adaptive",
        "express" => "Express",
        "ac" => "Primarily AC",
        "custom" => "60–80 %",
        "full" => "100 % once",
        _ => "?",
    }
}

fn have(cmd: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
}

fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

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

fn local_hm() -> (u32, u32, String) {
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
    let date = format!("{} {:02} {} {}", DAYS[tm.v[6].clamp(0, 6) as usize], tm.v[3], MON[tm.v[4].clamp(0, 11) as usize], 1900 + tm.v[5]);
    (tm.v[2] as u32, tm.v[1] as u32, date)
}

// ── live numbers for the widgets ───────────────────────────────────────────────────────────────

#[derive(Default)]
struct Stats {
    cpu_prev: (u64, u64),
    cpu: VecDeque<f32>,
    ram: (f32, u64, u64),
    npu_prev: Option<(u64, Instant)>,
    npu: Option<f32>,
    npu_hist: VecDeque<f32>,
    power: hud::Power,
    watts: Option<f64>,
    mode: String,
    wifi: hud::Wifi,
    net_prev: Option<((u64, u64), Instant)>,
    net: (f64, f64),
    workspaces: Vec<(String, bool, bool)>,
    temp: Option<i32>,
}

fn push(h: &mut VecDeque<f32>, v: f32) {
    h.push_back(v);
    while h.len() > 60 {
        h.pop_front();
    }
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

fn npu_busy() -> Option<u64> {
    for e in std::fs::read_dir("/sys/class/accel").ok()?.flatten() {
        if let Ok(v) = std::fs::read_to_string(e.path().join("device/npu_busy_time_us")) {
            return v.trim().parse().ok();
        }
    }
    None
}

fn human(b: f64) -> String {
    if b >= 1e6 {
        format!("{:.1} MB/s", b / 1e6)
    } else if b >= 1e3 {
        format!("{:.0} kB/s", b / 1e3)
    } else {
        format!("{b:.0} B/s")
    }
}

impl Stats {
    fn sample(&mut self, with_workspaces: bool) {
        let stat = std::fs::read_to_string("/proc/stat").unwrap_or_default();
        if let Some(l) = stat.lines().next() {
            let v: Vec<u64> = l.split_whitespace().skip(1).filter_map(|x| x.parse().ok()).collect();
            let total: u64 = v.iter().sum();
            let idle = v.get(3).copied().unwrap_or(0) + v.get(4).copied().unwrap_or(0);
            let (pt, pi) = self.cpu_prev;
            if pt > 0 && total > pt {
                push(&mut self.cpu, 1.0 - (idle.saturating_sub(pi)) as f32 / (total - pt) as f32);
            }
            self.cpu_prev = (total, idle);
        }
        let mi = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
        let kb = |k: &str| mi.lines().find_map(|l| l.strip_prefix(k)).and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok()).unwrap_or(0);
        let (total, avail) = (kb("MemTotal:"), kb("MemAvailable:"));
        self.ram = (if total > 0 { (total - avail) as f32 / total as f32 } else { 0.0 }, total - avail.min(total), total);
        let now = Instant::now();
        if let Some(b) = npu_busy() {
            if let Some((pb, pt)) = self.npu_prev {
                let dt = now.duration_since(pt).as_micros().max(1) as f32;
                let u = (b.saturating_sub(pb) as f32 / dt).clamp(0.0, 1.0);
                self.npu = Some(u);
                push(&mut self.npu_hist, u);
            }
            self.npu_prev = Some((b, now));
        }
        self.power = hud::power();
        if let Some(w) = self.power.watts {
            self.watts = Some(self.watts.map_or(w, |o| o * 0.8 + w * 0.2));
        }
        self.mode = hud::kv("run/shadow/power.state", "mode").unwrap_or_else(|| "—".into());
        self.wifi = hud::wifi();
        self.temp = hud::package_temp();
        let nb = net_bytes();
        if let Some(((r, t), at)) = self.net_prev {
            let dt = now.duration_since(at).as_secs_f64().max(0.1);
            self.net = (nb.0.saturating_sub(r) as f64 / dt, nb.1.saturating_sub(t) as f64 / dt);
        }
        self.net_prev = Some((nb, now));
        if with_workspaces {
            self.workspaces = shadow_niri::json(&["workspaces"])
                .ok()
                .and_then(|v| v.as_array().cloned())
                .map(|a| {
                    let mut w: Vec<(i64, String, bool, bool)> = a
                        .iter()
                        .map(|x| {
                            let idx = x.get("idx").and_then(|i| i.as_i64()).unwrap_or(0);
                            let name = x.get("name").and_then(|n| n.as_str()).map(String::from).unwrap_or_else(|| idx.to_string());
                            (idx, name, x.get("is_focused").and_then(|b| b.as_bool()).unwrap_or(false), x.get("active_window_id").is_some_and(|w| !w.is_null()))
                        })
                        .collect();
                    w.sort_by_key(|x| x.0);
                    w.into_iter().map(|(_, n, f, o)| (n, f, o)).collect()
                })
                .unwrap_or_default();
        }
    }
}

// ── panel rows ─────────────────────────────────────────────────────────────────────────────────

#[derive(Clone, PartialEq)]
enum Kind {
    /// run a command (and close in summon mode)
    Run(String),
    /// run only on a second press
    Confirm(String),
    /// value with − / +
    Slider(&'static str),
    /// one of a group of chips (two per line): action string, selected
    Chip(String, bool),
    /// internal action
    Act(String),
    /// a line of information
    Info,
}

#[derive(Clone)]
struct Item {
    label: String,
    detail: String,
    kind: Kind,
}

fn item(label: &str, detail: impl Into<String>, kind: Kind) -> Item {
    Item { label: label.into(), detail: detail.into(), kind }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    Core,
    Node(usize),
    Row(usize),
    Minus(usize),
    Plus(usize),
    Widget(usize),
    Panel,
}

struct Widget {
    id: &'static str,
    pos: (f32, f32),
    on: bool,
}

struct Hub {
    summon: bool,
    field: Field,
    wall_text: String,
    images: HashMap<String, Arc<Img>>,
    composed: HashMap<(String, usize, usize), Arc<Vec<u8>>>,
    main_output: Option<String>,
    /// [hub] reactive = false: the field stays still (slow machines, very large screens)
    reactive: bool,
    /// radial menu: open or not, and its animation (0..1)
    open: bool,
    anim: f32,
    page: Option<usize>,
    page_anim: f32,
    items: Vec<Item>,
    sel: usize,
    armed: Option<usize>,
    input: Input,
    apps: Vec<Entry>,
    hist: HashMap<String, i32>,
    msg: (String, bool),
    widgets: Vec<Widget>,
    drag: Option<(usize, i32, i32)>,
    hits: HashMap<usize, Vec<(Hit, Rect)>>,
    sizes: HashMap<usize, (i32, i32)>,
    hub_surface: usize,
    pending_ptr: Option<(i32, i32, usize)>,
    stats: Stats,
    last_sample: Instant,
    clock: (u32, u32),
    last_step: Instant,
    had_focus: bool,
    /// the hub (widgets, core, nodes, panel) rendered once into its own transparent layer; while
    /// only the vertex field moves, frames just composite it again
    overlay: Vec<u8>,
    overlay_size: (usize, usize),
    overlay_dirty: bool,
}

fn conf() -> String {
    std::fs::read_to_string(config_path()).unwrap_or_default()
}

fn load_widgets(text: &str) -> Vec<Widget> {
    WIDGETS
        .iter()
        .map(|(id, x, y)| {
            let v = get_in(text, "hub.widgets", id).map(|v| v.trim_matches('"').to_string());
            let on = v.as_deref() != Some("off");
            let pos = v.and_then(|v| v.split_once(',').and_then(|(a, b)| Some((a.trim().parse::<f32>().ok()?, b.trim().parse::<f32>().ok()?)))).map(|(a, b)| (a.clamp(0.02, 0.98), b.clamp(0.02, 0.98))).unwrap_or((*x, *y));
            Widget { id, pos, on }
        })
        .collect()
}

impl Hub {
    fn new(summon: bool, page: Option<usize>) -> Hub {
        let text = conf();
        let main_output = get_in(&text, "hub", "output").map(|v| v.trim_matches('"').to_string()).filter(|v| !v.is_empty());
        let reactive = get_in(&text, "hub", "reactive").map(|v| v.trim() != "false").unwrap_or(true);
        let mut h = Hub {
            summon,
            reactive,
            field: Field::new(),
            widgets: load_widgets(&text),
            wall_text: text,
            images: HashMap::new(),
            composed: HashMap::new(),
            main_output,
            open: summon,
            anim: if summon { 0.0 } else { 0.0 },
            page,
            page_anim: 0.0,
            items: vec![],
            sel: 0,
            armed: None,
            input: Input::default(),
            apps: desktop::load_apps(),
            hist: desktop::load_history(),
            msg: (String::new(), false),
            drag: None,
            hits: HashMap::new(),
            sizes: HashMap::new(),
            hub_surface: 0,
            pending_ptr: None,
            stats: Stats::default(),
            last_sample: Instant::now(),
            clock: (99, 99),
            last_step: Instant::now(),
            had_focus: false,
            overlay: vec![],
            overlay_size: (0, 0),
            overlay_dirty: true,
        };
        h.stats.sample(true);
        h.build();
        h
    }

    fn animating(&self) -> bool {
        let a = if self.open { 1.0 } else { 0.0 };
        let p = if self.page.is_some() { 1.0 } else { 0.0 };
        (self.anim - a).abs() > 0.001 || (self.page_anim - p).abs() > 0.001 || self.drag.is_some()
    }

    fn build(&mut self) {
        self.armed = None;
        let Some(page) = self.page else {
            self.items.clear();
            return;
        };
        let s = &self.stats;
        self.items = match PAGES[page] {
            "APPS" => {
                let q = self.input.text.trim().to_lowercase();
                let mut v: Vec<(i32, &Entry)> = self
                    .apps
                    .iter()
                    .filter_map(|e| {
                        let base = if e.tag == "shadow" { 6 } else { 0 };
                        desktop::score(&q, &format!("{} {} {}", e.name, e.keywords, e.detail)).map(|sc| (sc * 4 + self.hist.get(&e.id).copied().unwrap_or(0).min(50) + base, e))
                    })
                    .collect();
                v.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.to_lowercase().cmp(&b.1.name.to_lowercase())));
                let mut items: Vec<Item> = v
                    .into_iter()
                    .take(8)
                    .map(|(_, e)| {
                        let cmd = if e.terminal { format!("shadow-term -e {}", e.cmd) } else { e.cmd.clone() };
                        Item { label: e.name.clone(), detail: e.detail.clone(), kind: Kind::Run(format!("@{}\u{1}{cmd}", e.id)) }
                    })
                    .collect();
                items.push(item("All apps…", "the full launcher", Kind::Run("shadow-launch".into())));
                items
            }
            "SETTINGS" => {
                let mut v = vec![];
                if let Some(b) = brightness() {
                    v.push(item("Brightness", format!("{:.0} %", b * 100.0), Kind::Slider("brightness")));
                }
                if let Some((vol, muted)) = volume() {
                    v.push(item("Volume", if muted { "muted".to_string() } else { format!("{:.0} %", vol * 100.0) }, Kind::Slider("volume")));
                }
                v.push(item("Displays", "scale · text size · refresh", Kind::Run("shadow-config Displays".into())));
                v.push(item("Keyboard & input", "layouts · touchpad", Kind::Run("shadow-config Input".into())));
                v.push(item("All settings…", "shadow-config", Kind::Run("shadow-config".into())));
                v
            }
            "POWER" => {
                let mut v = vec![];
                let left = match hud::time_left(&s.power, s.watts) {
                    Some((m, false)) => format!("{} left", hud::fmt_minutes(m)),
                    Some((m, true)) => format!("{} to full", hud::fmt_minutes(m)),
                    None => if s.power.ac { "on AC".into() } else { String::new() },
                };
                v.push(item("Battery", format!("{}{}", s.power.battery.map(|b| format!("{b} % · ")).unwrap_or_default(), left), Kind::Info));
                for (m, l) in MODES {
                    v.push(item(l, "", Kind::Chip(format!("mode:{m}"), s.mode == m)));
                }
                // charging: only the modes the firmware lists (the daemon validates and rate-limits)
                let avail: Vec<String> = hud::kv("run/shadow/power.state", "charge_avail").unwrap_or_default().split_whitespace().map(String::from).collect();
                let chosen = hud::kv("run/shadow/power.state", "charge_mode").unwrap_or_default();
                let full_once = hud::kv("run/shadow/power.state", "full_once").as_deref() == Some("1");
                if avail.is_empty() {
                    v.push(item("Charging", "not offered by this firmware", Kind::Info));
                } else {
                    v.push(item("Charging", "firmware modes · max 6 changes a day", Kind::Info));
                    for m in &avail {
                        let on = if m == "full" { full_once } else { *m == chosen && !full_once };
                        v.push(item(charge_label(m), "", Kind::Chip(format!("charge:{m}"), on)));
                    }
                }
                v.push(item("Power by app", "task manager", Kind::Run("shadow-tasks".into())));
                v
            }
            "SYSTEM" => {
                let mut v = vec![item("Lock", "Mod+Alt+L", Kind::Run("shadow-lock".into())), item("Suspend", "", Kind::Run("systemctl suspend".into()))];
                v.extend([
                    item("Log out", "leave the session", Kind::Confirm("niri msg action quit --skip-confirmation".into())),
                    item("Reboot", "", Kind::Confirm("systemctl reboot".into())),
                    item("Shut down", "", Kind::Confirm("systemctl poweroff".into())),
                ]);
                if have("shadow-sec") {
                    v.push(item("Security console", "precheck · modules (password)", Kind::Run("shadow-term --hold -e shadow-sec console".into())));
                }
                v
            }
            "NETWORK" => {
                let w = &s.wifi;
                vec![
                    item("Wi-Fi", if w.ssid.is_empty() { "not connected".to_string() } else { format!("{}{}", w.ssid, w.dbm.as_ref().map(|d| format!(" · {d} dBm")).unwrap_or_default()) }, Kind::Info),
                    item("Traffic", format!("↓ {}  ↑ {}", human(s.net.0), human(s.net.1)), Kind::Info),
                    item("Networks…", "connect · VPN · hotspot", Kind::Run("shadow-net".into())),
                    item("Wi-Fi radio", "on / off", Kind::Act("wifi-toggle".into())),
                ]
            }
            "FIX" => vec![
                item("Fix center", "app problems · tasks · triage", Kind::Run("shadow-fix".into())),
                item("Update", "check now · changed files only", Kind::Run("shadow-update".into())),
                item("Password repair", "login / keyring / sudo", Kind::Run("shadow-term --hold -e shadow-passfix".into())),
            ],
            "LOOK" => {
                let (cur, _) = wall::choice(&self.wall_text, None);
                let op = shadow_ui::Theme::load().window_opacity;
                vec![
                    item("Vertices", "3D wireframe · follows the cursor", Kind::Chip("wall:vertices".into(), cur == "vertices")),
                    item("Aurora", "generated image", Kind::Chip("wall:default".into(), cur == "default")),
                    item("Wallpaper image…", "browse · per monitor", Kind::Run("shadow-wallpaper pick".into())),
                    item("Window glass", if op >= 0.999 { "opaque".to_string() } else { format!("{:.0} %", op * 100.0) }, Kind::Slider("glass")),
                    item("Reset widget layout", "", Kind::Act("reset-layout".into())),
                ]
            }
            _ => {
                let cpu = s.cpu.back().copied().unwrap_or(0.0);
                vec![
                    item("CPU", format!("{:.0} %{}", cpu * 100.0, s.temp.map(|t| format!(" · {t} °C")).unwrap_or_default()), Kind::Info),
                    item("Memory", format!("{:.1} / {:.1} GiB", s.ram.1 as f64 / 1048576.0, s.ram.2 as f64 / 1048576.0), Kind::Info),
                    item("NPU", s.npu.map(|n| format!("{:.0} %", n * 100.0)).unwrap_or_else(|| "no busy counter".into()), Kind::Info),
                    item("Task manager", "processes · end tasks", Kind::Run("shadow-tasks".into())),
                ]
            }
        };
        let n = self.items.len();
        self.sel = self.sel.min(n.saturating_sub(1));
        while self.sel + 1 < n && matches!(self.items[self.sel].kind, Kind::Info) {
            self.sel += 1;
        }
    }

    fn rebuild_keep(&mut self) {
        let keep = self.sel;
        self.build();
        self.sel = keep.min(self.items.len().saturating_sub(1));
    }

    fn set_page(&mut self, p: Option<usize>, ctx: &mut Ctx) {
        if p != self.page {
            self.page = p;
            self.page_anim = 0.0;
            self.sel = 0;
            self.input.clear();
            self.msg = (String::new(), false);
            if p.is_some() {
                self.stats.sample(false);
            }
            self.build();
        }
        ctx.wake();
        ctx.redraw();
    }

    fn adjust(&mut self, i: usize, dir: i32, ctx: &mut Ctx) {
        let Some(it) = self.items.get(i) else { return };
        match &it.kind {
            Kind::Slider("brightness") => {
                let _ = cmd_out(&["shadow-displayd", "brightness", if dir > 0 { "5%+" } else { "5%-" }]);
            }
            Kind::Slider("volume") => {
                let _ = cmd_out(&["wpctl", "set-volume", "-l", "1.0", "@DEFAULT_AUDIO_SINK@", if dir > 0 { "5%+" } else { "5%-" }]);
            }
            Kind::Slider("glass") => {
                const STEPS: [f32; 5] = [0.70, 0.78, 0.86, 0.94, 1.0];
                let cur = shadow_ui::Theme::load().window_opacity;
                let i = STEPS.iter().position(|s| (*s - cur).abs() < 0.02).unwrap_or(2) as i32;
                let v = STEPS[(i + dir).clamp(0, STEPS.len() as i32 - 1) as usize];
                let t = set_in(&conf(), "look", "window_opacity", &format!("{v:.2}"));
                let _ = atomic_write(&config_path(), &t);
                self.msg = ("new windows use it; open ones on their next start".into(), false);
            }
            _ => return,
        }
        self.rebuild_keep();
        ctx.redraw();
    }

    /// suspend / reboot / shut down: wait, so a refusal is shown rather than lost
    fn power_cmd(&mut self, cmd: &str) -> bool {
        let argv: Vec<&str> = cmd.split_whitespace().collect();
        let (c, o) = cmd_out(&argv);
        if c == 0 {
            return true;
        }
        let why = o.lines().find(|l| !l.trim().is_empty()).unwrap_or("refused").trim().to_string();
        let hint = if why.contains("inhibit") {
            " — wait for it to finish"
        } else if why.contains("authentication agent") || why.contains("Interactive authentication required") {
            " — no polkit agent to ask for the password"
        } else {
            ""
        };
        self.msg = (format!("✗ {why}{hint}"), true);
        false
    }

    fn done(&mut self, ctx: &mut Ctx) {
        if self.summon {
            ctx.quit();
        } else {
            self.page = None;
            self.open = false;
            self.input.clear();
            ctx.wake();
        }
        ctx.redraw();
    }

    fn activate(&mut self, i: usize, ctx: &mut Ctx) {
        let Some(it) = self.items.get(i).cloned() else { return };
        self.sel = i;
        match it.kind {
            Kind::Info => {}
            Kind::Run(cmd) if cmd.starts_with("systemctl ") => {
                if self.power_cmd(&cmd) {
                    self.done(ctx);
                }
            }
            Kind::Run(cmd) => {
                if let Some(rest) = cmd.strip_prefix('@') {
                    let (id, c) = rest.split_once('\u{1}').unwrap_or(("", rest));
                    desktop::record_launch(id, "");
                    spawn(c);
                } else {
                    spawn(&cmd);
                }
                self.done(ctx);
            }
            Kind::Confirm(cmd) => {
                if self.armed == Some(i) {
                    self.armed = None;
                    if cmd.starts_with("systemctl ") {
                        if self.power_cmd(&cmd) {
                            self.done(ctx);
                        }
                    } else {
                        spawn(&cmd);
                        self.done(ctx);
                    }
                } else {
                    self.armed = Some(i);
                }
            }
            Kind::Slider("volume") => {
                let _ = cmd_out(&["wpctl", "set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"]);
                self.rebuild_keep();
            }
            Kind::Slider(_) => self.adjust(i, 1, ctx),
            Kind::Chip(act, _) => {
                let (k, v) = act.split_once(':').unwrap_or((&act, ""));
                match k {
                    "mode" | "charge" => {
                        let mut args = vec![k.to_string(), v.to_string()];
                        if k == "mode" {
                            args.push("keep".into());
                        } else if v == "custom" {
                            args.extend(["60".into(), "80".into()]);
                        }
                        // the power daemon validates and rate-limits (charging writes go to firmware)
                        let a: Vec<&str> = std::iter::once("shadow-powerd").chain(args.iter().map(String::as_str)).collect();
                        let (c, o) = cmd_out(&a);
                        self.msg = if c == 0 { (format!("{} → {}", k, v), false) } else { (format!("✗ {}", o.lines().next().unwrap_or("refused")), true) };
                        self.stats.sample(false);
                    }
                    "wall" => {
                        self.msg = match wall::set(v, None, None) {
                            Ok(()) => ("wallpaper set".into(), false),
                            Err(e) => (e, true),
                        };
                        self.reload();
                    }
                    _ => {}
                }
                self.rebuild_keep();
            }
            Kind::Act(a) => {
                match a.as_str() {
                    "wifi-toggle" => {
                        let (_, o) = cmd_out(&["nmcli", "radio", "wifi"]);
                        let to = if o.trim() == "enabled" { "off" } else { "on" };
                        let (c, o) = cmd_out(&["nmcli", "radio", "wifi", to]);
                        self.msg = if c == 0 { (format!("Wi-Fi {to}"), false) } else { (format!("✗ {}", o.lines().next().unwrap_or("nmcli failed")), true) };
                    }
                    "reset-layout" => {
                        reset_layout();
                        self.widgets = load_widgets(&conf());
                        self.msg = ("widgets back in place".into(), false);
                    }
                    _ => {}
                }
                self.rebuild_keep();
            }
        }
        ctx.redraw();
    }

    fn reload(&mut self) {
        self.wall_text = conf();
        self.images.clear();
        self.composed.clear();
        self.widgets = load_widgets(&self.wall_text);
        self.reactive = get_in(&self.wall_text, "hub", "reactive").map(|v| v.trim() != "false").unwrap_or(true);
    }

    /// Wallpaper image pixels for an output, when the wallpaper is an image (not "vertices").
    fn image(&mut self, output: Option<&str>, w: usize, h: usize) -> Option<Arc<Vec<u8>>> {
        let (path, mode) = wall::choice(&self.wall_text, output);
        if path == "vertices" {
            return None;
        }
        let key = (path.clone(), w, h);
        if let Some(p) = self.composed.get(&key) {
            return Some(p.clone());
        }
        let px = if path == "default" {
            wall::default_wallpaper(w, h)
        } else {
            let img = match self.images.get(&path) {
                Some(i) => i.clone(),
                None => match wall::decode(Path::new(&path)) {
                    Ok(i) => {
                        let i = Arc::new(i);
                        self.images.insert(path.clone(), i.clone());
                        i
                    }
                    Err(e) => {
                        eprintln!("shadow-hub: {e} — using the vertex field");
                        return None;
                    }
                },
            };
            wall::compose(&img, w, h, if mode == Mode::Fill { Mode::Fill } else { mode })
        };
        if self.composed.len() > 6 {
            self.composed.clear();
        }
        let px = Arc::new(px);
        self.composed.insert(key, px.clone());
        Some(px)
    }

    fn save_widget(&self, i: usize) {
        let w = &self.widgets[i];
        let t = set_in(&conf(), "hub.widgets", w.id, &format!("\"{:.3},{:.3}\"", w.pos.0, w.pos.1));
        let _ = atomic_write(&config_path(), &t);
    }
}

fn reset_layout() {
    let mut t = conf();
    for (id, _, _) in WIDGETS {
        // keep widgets the user switched off
        if get_in(&t, "hub.widgets", id).map(|v| v.trim_matches('"').to_string()).as_deref() != Some("off") {
            t = set_in(&t, "hub.widgets", id, &format!("\"{:.3},{:.3}\"", WIDGETS.iter().find(|w| w.0 == id).unwrap().1, WIDGETS.iter().find(|w| w.0 == id).unwrap().2));
        }
    }
    let _ = atomic_write(&config_path(), &t);
}

// ── drawing ────────────────────────────────────────────────────────────────────────────────────

/// GitS-style connector: horizontal from `a`, a 45° run, then horizontal into `b`; vertex nodes at
/// the joints.
fn connector(f: &mut Frame, a: (f32, f32), b: (f32, f32), c: Rgb, alpha: f32) {
    let s = if b.0 >= a.0 { 1.0 } else { -1.0 };
    let dy = (b.1 - a.1).abs();
    let dx = (b.0 - a.0).abs();
    let p1x = if dx > dy { a.0 + s * (dx - dy) * 0.5 } else { a.0 };
    let p2x = if dx > dy { b.0 - s * (dx - dy) * 0.5 } else { b.0 };
    let pts = [a, (p1x, a.1), (p2x, b.1), b];
    let wdt = f.px(1.3) as f32;
    for i in 0..3 {
        let (p, q) = (pts[i], pts[i + 1]);
        if (p.0 - q.0).abs() + (p.1 - q.1).abs() > 0.5 {
            f.cv.tline(p.0, p.1, q.0, q.1, wdt, c, alpha);
        }
    }
    let r = f.px(1.8) as f32;
    for p in &pts[..3] {
        f.cv.node(p.0, p.1, r, c, alpha + 0.3);
    }
}

fn label_c(f: &mut Frame, cx: i32, top: i32, s: &str, pt: f32, c: Rgb, bold: bool, a: f32) {
    let w = f.text_w(s, pt, bold);
    f.text_a(cx - w / 2, top, s, pt, c, bold, a);
}

impl Hub {
    fn core_r(&self, f: &Frame) -> f32 {
        f.px(58.0) as f32
    }

    fn node_pos(&self, f: &Frame, i: usize, centre: (f32, f32)) -> (f32, f32) {
        let a = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::TAU / PAGES.len() as f32;
        let r = (f.px(165.0) as f32) * ease(self.anim);
        (centre.0 + r * a.cos() * 1.25, centre.1 + r * a.sin())
    }

    fn draw_core(&mut self, f: &mut Frame, c: (f32, f32), hits: &mut Vec<(Hit, Rect)>) {
        let th = f.theme.clone();
        let r = self.core_r(f);
        let open = ease(self.anim);
        // tick ring
        for i in 0..72 {
            let a = i as f32 * std::f32::consts::TAU / 72.0;
            let long = i % 6 == 0;
            let (r0, r1) = (r + f.px(10.0) as f32, r + f.px(if long { 18.0 } else { 14.0 }) as f32);
            f.cv.wline(c.0 + r0 * a.cos(), c.1 + r0 * a.sin(), c.0 + r1 * a.cos(), c.1 + r1 * a.sin(), if long { th.amber } else { th.cyan }, if long { 0.6 } else { 0.28 });
        }
        // octagon of glass
        let oct: Vec<(f32, f32)> = (0..8)
            .map(|i| {
                let a = std::f32::consts::TAU * (i as f32 + 0.5) / 8.0;
                (c.0 + r * a.cos(), c.1 + r * a.sin())
            })
            .collect();
        let (g, lo, am) = (th.glass, th.glass_lo, th.amber);
        f.cv.glow((c.0 - r) as i32, (c.1 - r) as i32, (2.0 * r) as i32, (2.0 * r) as i32, r, f.px(10.0), lerp(th.cyan, th.amber, open), 0.10 + 0.08 * open);
        f.cv.poly_ga(&oct, &|_, fy| (lerp(lerp(g, am, 0.10 * open), lo, fy), 0.88));
        f.cv.poly_ga(&oct, &|_, fy| ((255, 255, 255), if fy < 0.45 { 0.12 * (1.0 - fy / 0.45) } else { 0.0 }));
        f.cv.poly_line(&oct, lerp(th.cyan, th.amber, open), 0.85);
        for p in &oct {
            f.cv.node(p.0, p.1, f.px(1.7) as f32, th.cyan, 0.9);
        }
        // inner ring
        let ir = r * 0.72;
        for i in 0..48 {
            let (a0, a1) = (i as f32 * std::f32::consts::TAU / 48.0, (i as f32 + 0.6) * std::f32::consts::TAU / 48.0);
            f.cv.wline(c.0 + ir * a0.cos(), c.1 + ir * a0.sin(), c.0 + ir * a1.cos(), c.1 + ir * a1.sin(), th.violet, 0.45);
        }
        let (hh, mm, _) = local_hm();
        let pt = th.font_pt;
        let ty = (c.1 - f.line_h(pt * 1.6) as f32 * 0.62) as i32;
        label_c(f, c.0 as i32, ty, &format!("{hh:02}:{mm:02}"), pt * 1.6, th.text, true, 1.0);
        label_c(f, c.0 as i32, (c.1 + f.px(6.0) as f32) as i32, if self.open { "CLOSE" } else { "SHADOW" }, pt * 0.8, if self.open { th.amber } else { th.cyan }, true, 0.9);
        hits.push((Hit::Core, Rect { x: (c.0 - r) as i32, y: (c.1 - r) as i32, w: (2.0 * r) as i32, h: (2.0 * r) as i32 }));
    }

    fn draw_nodes(&mut self, f: &mut Frame, c: (f32, f32), hits: &mut Vec<(Hit, Rect)>) {
        let th = f.theme.clone();
        let t = ease(self.anim);
        if t < 0.02 {
            return;
        }
        let r = self.core_r(f) + f.px(20.0) as f32;
        let pt = th.font_pt * 0.92;
        for i in 0..PAGES.len() {
            let p = self.node_pos(f, i, c);
            let (dx, dy) = (p.0 - c.0, p.1 - c.1);
            let d = (dx * dx + dy * dy).sqrt().max(1.0);
            let from = (c.0 + dx / d * r, c.1 + dy / d * r);
            let on = self.page == Some(i);
            let accent = if on { th.amber } else { th.cyan };
            if d > r {
                f.cv.tline(from.0, from.1, p.0, p.1, f.px(1.2) as f32, accent, 0.45 * t);
                f.cv.node(from.0, from.1, f.px(1.6) as f32, accent, 0.8 * t);
            }
            let label = format!("{} {}", i + 1, PAGES[i]);
            let w = f.text_w(&label, pt, true) + f.px(26.0);
            let h = f.line_h(pt) + f.px(12.0);
            let (x, y) = ((p.0 - w as f32 / 2.0) as i32, (p.1 - h as f32 / 2.0) as i32);
            if on {
                let pts = chamfer_pts(x, y, w, h, h / 2);
                let (a, v) = (th.amber, lerp(th.amber, th.violet, 0.6));
                f.cv.glow(x, y, w, h, 2.0, f.px(7.0), th.amber, 0.28 * t);
                f.cv.poly(&pts, t, &|fx, _| lerp(a, v, fx));
                f.cv.poly_ga(&pts, &|_, fy| ((255, 255, 255), if fy < 0.5 { 0.2 * (1.0 - fy * 2.0) * t } else { 0.0 }));
                label_c(f, p.0 as i32, y + f.px(6.0), &label, pt, th.bg, true, t);
            } else {
                glass(f, x, y, w, h, h / 2, accent, 0.9 * t, true);
                label_c(f, p.0 as i32, y + f.px(6.0), &label, pt, th.text, true, t);
            }
            hits.push((Hit::Node(i), Rect { x, y, w, h }));
        }
    }

    fn draw_panel(&mut self, f: &mut Frame, c: (f32, f32), size: (i32, i32), hits: &mut Vec<(Hit, Rect)>) {
        let Some(page) = self.page else { return };
        let th = f.theme.clone();
        let t = ease(self.page_anim);
        let p = self.node_pos(f, page, c);
        let pt = th.font_pt;
        let lh = f.line_h(pt);
        let row_h = lh + f.px(12.0);
        let chip_h = lh + f.px(10.0);
        let pad = f.px(14.0);
        let w = f.px(PANEL_W);
        // content height
        let head = lh + f.px(22.0);
        let search = if PAGES[page] == "APPS" { lh + f.px(22.0) } else { 0 };
        let mut body = 0;
        let mut i = 0;
        while i < self.items.len() {
            if matches!(self.items[i].kind, Kind::Chip(..)) {
                let mut n = 0;
                while i < self.items.len() && matches!(self.items[i].kind, Kind::Chip(..)) {
                    n += 1;
                    i += 1;
                }
                body += ((n + 1) / 2) * (chip_h + f.px(6.0)) + f.px(4.0);
            } else {
                body += row_h;
                i += 1;
            }
        }
        let foot = lh + f.px(16.0);
        let full_h = head + search + body + foot + pad;
        // beside the node, on the side away from the core; slides open (drops down) from the node
        // panels sit outside the ring of nodes, on the node's side (top / bottom nodes: right), so
        // they never cover the other options
        let right = p.0 >= c.0 - 1.0;
        let ring = f.px(165.0 * 1.25 + 80.0) as f32;
        let x = if right { c.0 + ring } else { c.0 - ring - w as f32 } as i32 + ((1.0 - t) * f.px(if right { -24.0 } else { 24.0 }) as f32) as i32;
        // (never clamp with min > max: that panics on a monitor narrower than the panel)
        let x = x.clamp(f.px(10.0), (size.0 - w - f.px(10.0)).max(f.px(10.0)));
        let y = ((p.1 - head as f32) as i32).clamp(f.px(10.0), (size.1 - full_h - f.px(10.0)).max(f.px(10.0)));
        let h = ((full_h as f32) * t).max(head as f32 * t) as i32;
        // link from the node to the panel
        let a = (if right { p.0 + f.px(44.0) as f32 } else { p.0 - f.px(44.0) as f32 }, p.1);
        let b = (if right { x as f32 } else { (x + w) as f32 }, (y + head / 2) as f32);
        connector(f, a, b, th.amber, 0.55 * t);
        f.cv.glow(x, y, w, h, 2.0, f.px(8.0), th.cyan, 0.08 * t);
        glass(f, x, y, w, h, f.px(th.chamfer + 4.0), th.cyan, 0.97 * t, true);
        hits.push((Hit::Panel, Rect { x, y, w, h }));
        if t < 0.25 {
            return;
        }
        let bottom = y + h;
        let mut yy = y + f.px(10.0);
        // title tag
        let title = format!("  {}  ", PAGES[page]);
        let tw = f.text_w(&title, pt, true) + f.px(12.0);
        let tp = chamfer_pts(x + pad, yy, tw, lh + f.px(6.0), (lh + f.px(6.0)) / 2);
        let (am, vi) = (th.amber, th.violet);
        f.cv.poly(&tp, 1.0, &|fx, _| lerp(am, vi, fx));
        f.text(x + pad, yy + f.px(3.0), &title, pt, th.bg, true);
        f.cv.beam(x + pad + tw + f.px(8.0), yy + lh / 2 + f.px(3.0), w - 2 * pad - tw - f.px(8.0), 1, th.amber, th.cyan, 0.6);
        yy += head;
        if search > 0 && yy + search <= bottom {
            shadow_ui::widgets::field(f, x + pad, yy, w - 2 * pad, lh + f.px(12.0), true);
            if self.input.text.is_empty() {
                f.text_a(x + pad + f.px(12.0), yy + f.px(6.0), "type to search…", pt, th.dim, false, 0.8);
            } else {
                self.input.draw(f, x + pad + f.px(12.0), yy + f.px(6.0), pt, true);
            }
            yy += search;
        }
        let mut i = 0;
        let iw = w - 2 * pad;
        while i < self.items.len() {
            let it = self.items[i].clone();
            if let Kind::Chip(..) = it.kind {
                // a grid of chips, two per line
                let cw = (iw - f.px(6.0)) / 2;
                let mut k = 0;
                let mut full = false;
                while i < self.items.len() {
                    let Kind::Chip(_, on) = self.items[i].kind.clone() else { break };
                    let (cx, cy) = (x + pad + (k % 2) * (cw + f.px(6.0)), yy + (k / 2) * (chip_h + f.px(6.0)));
                    if cy + chip_h > bottom {
                        // the panel is still sliding open: nothing more fits
                        full = true;
                        break;
                    }
                    let r = Rect { x: cx, y: cy, w: cw, h: chip_h };
                    let label = self.items[i].label.clone();
                    if on {
                        let pts = chamfer_pts(r.x, r.y, r.w, r.h, r.h / 2);
                        let (a, v) = (th.amber, lerp(th.amber, th.violet, 0.5));
                        f.cv.poly(&pts, 1.0, &|fx, _| lerp(a, v, fx));
                        label_c(f, cx + cw / 2, cy + f.px(5.0), &label, pt * 0.92, th.bg, true, 1.0);
                    } else {
                        glass(f, r.x, r.y, r.w, r.h, r.h / 2, if self.sel == i { th.amber } else { lerp(th.dim, th.cyan, 0.4) }, 0.9, false);
                        label_c(f, cx + cw / 2, cy + f.px(5.0), &label, pt * 0.92, th.text, false, 1.0);
                    }
                    if self.sel == i {
                        f.cv.node(r.x as f32 + f.px(6.0) as f32, (r.y + r.h / 2) as f32, f.px(1.8) as f32, th.amber, 1.0);
                    }
                    hits.push((Hit::Row(i), r));
                    k += 1;
                    i += 1;
                }
                if full {
                    break;
                }
                yy += ((k + 1) / 2) * (chip_h + f.px(6.0)) + f.px(4.0);
                continue;
            }
            if yy + row_h > bottom {
                break;
            }
            let r = Rect { x: x + pad, y: yy, w: iw, h: row_h - f.px(2.0) };
            if self.sel == i && !matches!(it.kind, Kind::Info) {
                shadow_ui::widgets::selection(f, r.x, r.y, r.w, r.h);
            }
            let ty = yy + f.px(5.0);
            let armed = self.armed == Some(i);
            let label = if armed { format!("{} — press again", it.label) } else { it.label.clone() };
            let lc = if armed { th.crimson } else if matches!(it.kind, Kind::Info) { th.dim } else { th.text };
            let lx = r.x + f.px(16.0);
            match it.kind {
                Kind::Slider(_) => {
                    let bw = f.px(26.0);
                    let minus = Rect { x: r.x + r.w - 2 * bw - f.px(6.0), y: r.y + f.px(3.0), w: bw, h: r.h - f.px(6.0) };
                    let plus = Rect { x: r.x + r.w - bw - f.px(2.0), ..minus };
                    let dw = f.text_w(&it.detail, pt * 0.9, false);
                    let l = f.fit(&label, pt, self.sel == i, minus.x - dw - f.px(22.0) - lx);
                    f.text(lx, ty, &l, pt, lc, self.sel == i);
                    f.text(minus.x - dw - f.px(10.0), ty + f.px(1.0), &it.detail, pt * 0.9, th.cyan, false);
                    for (rr, s) in [(minus, "−"), (plus, "+")] {
                        glass(f, rr.x, rr.y, rr.w, rr.h, rr.h / 3, th.cyan, 0.9, false);
                        label_c(f, rr.x + rr.w / 2, rr.y + (rr.h - lh) / 2, s, pt, th.cyan, true, 1.0);
                    }
                    hits.push((Hit::Minus(i), minus));
                    hits.push((Hit::Plus(i), plus));
                    if let Some(v) = it.detail.trim_end_matches(" %").trim_end_matches(" % opaque").parse::<f32>().ok() {
                        meter(f, lx, r.y + r.h - f.px(4.0), (minus.x - dw - f.px(18.0) - lx).max(0), f.px(3.0), v / 100.0, false);
                    }
                }
                _ => {
                    let dmax = r.w / 2;
                    let d = f.fit(&it.detail, pt * 0.88, false, dmax);
                    let dw = f.text_w(&d, pt * 0.88, false);
                    let l = f.fit(&label, pt, self.sel == i, r.w - dw - f.px(36.0));
                    f.text(lx, ty, &l, pt, lc, self.sel == i && !matches!(it.kind, Kind::Info));
                    f.text(r.x + r.w - dw - f.px(8.0), ty + f.px(1.0), &d, pt * 0.88, if matches!(it.kind, Kind::Info) { th.text } else { th.dim }, false);
                }
            }
            if !matches!(it.kind, Kind::Info) {
                hits.push((Hit::Row(i), Rect { w: if matches!(it.kind, Kind::Slider(_)) { r.w / 2 } else { r.w }, ..r }));
            }
            yy += row_h;
            i += 1;
        }
        // footer: message or hints
        let fy = bottom - lh - f.px(10.0);
        if fy > yy - f.px(4.0) {
            let (m, err) = self.msg.clone();
            let s = if !m.is_empty() {
                m
            } else if PAGES[page] == "APPS" {
                "type to search · ↑↓ · Enter · Tab next · Esc".to_string()
            } else if self.items.iter().any(|i| matches!(i.kind, Kind::Slider(_))) {
                "↑↓ · Enter · ← → adjust · Tab next · Esc".to_string()
            } else {
                "↑↓ · Enter · Tab next page · Esc closes".to_string()
            };
            let s = f.fit(&s, pt * 0.85, false, w - 2 * pad);
            f.text(x + pad, fy, &s, pt * 0.85, if err { th.crimson } else { th.dim }, false);
        }
    }

    fn widget_rect(&self, f: &Frame, i: usize, size: (i32, i32)) -> Rect {
        let (w, h) = match self.widgets[i].id {
            "clock" => (250.0, 104.0),
            "workspaces" => (300.0, 70.0),
            _ => (250.0, 118.0),
        };
        let (w, h) = (f.px(w), f.px(h));
        let (fx, fy) = self.widgets[i].pos;
        let x = ((fx * size.0 as f32) as i32 - w / 2).clamp(f.px(8.0), (size.0 - w - f.px(8.0)).max(f.px(8.0)));
        let y = ((fy * size.1 as f32) as i32 - h / 2).clamp(f.px(8.0), (size.1 - h - f.px(8.0)).max(f.px(8.0)));
        Rect { x, y, w, h }
    }

    fn draw_widgets(&mut self, f: &mut Frame, c: (f32, f32), size: (i32, i32), hits: &mut Vec<(Hit, Rect)>) {
        let th = f.theme.clone();
        let pt = th.font_pt;
        let lh = f.line_h(pt);
        let core_r = self.core_r(f) + f.px(18.0) as f32;
        for i in 0..self.widgets.len() {
            if !self.widgets[i].on {
                continue;
            }
            let r = self.widget_rect(f, i, size);
            let id = self.widgets[i].id;
            let accent = match id {
                "power" => th.amber,
                "system" => th.violet,
                "network" => th.cyan,
                "workspaces" => th.green,
                _ => th.cyan,
            };
            // connector to the core: from the widget side facing it
            let wc = (r.x as f32 + r.w as f32 / 2.0, r.y as f32 + r.h as f32 / 2.0);
            let (dx, dy) = (c.0 - wc.0, c.1 - wc.1);
            let (a, b) = if dx.abs() * 0.6 > dy.abs() {
                let s = dx.signum();
                ((if s > 0.0 { (r.x + r.w) as f32 } else { r.x as f32 }, wc.1), (c.0 - s * core_r, c.1))
            } else {
                let s = dy.signum();
                ((wc.0, if s > 0.0 { (r.y + r.h) as f32 } else { r.y as f32 }), (c.0, c.1 - s * core_r))
            };
            // wires fade back while the radial menu is out, so the nodes read clearly
            let wa = 0.40 * (1.0 - 0.6 * ease(self.anim));
            if dx.abs() * 0.6 > dy.abs() {
                connector(f, a, b, accent, wa);
            } else {
                // vertical first, then into the core
                let mid = (a.0, (a.1 + b.1) / 2.0);
                f.cv.tline(a.0, a.1, mid.0, mid.1, f.px(1.3) as f32, accent, wa);
                f.cv.tline(mid.0, mid.1, b.0, b.1, f.px(1.3) as f32, accent, wa);
                f.cv.node(a.0, a.1, f.px(1.8) as f32, accent, 0.7);
                f.cv.node(mid.0, mid.1, f.px(1.8) as f32, accent, 0.7);
            }
            let dragging = self.drag.is_some_and(|d| d.0 == i);
            f.cv.glow(r.x, r.y, r.w, r.h, 2.0, f.px(6.0), accent, if dragging { 0.25 } else { 0.08 });
            glass(f, r.x, r.y, r.w, r.h, f.px(th.chamfer), accent, 0.82, true);
            hits.push((Hit::Widget(i), r));
            let (x, mut y) = (r.x + f.px(14.0), r.y + f.px(9.0));
            let iw = r.w - f.px(28.0);
            let small = pt * 0.82;
            f.text_a(x, y, &id.to_uppercase(), small, accent, true, 0.9);
            let (tw, gap, lhs) = (f.text_w(&id.to_uppercase(), small, true), f.px(8.0), f.line_h(small));
            f.cv.beam(x + tw + gap, y + lhs / 2, iw - tw - gap, 1, accent, th.violet, 0.4);
            y += f.line_h(small) + f.px(4.0);
            let s = &self.stats;
            match id {
                "clock" => {
                    let (hh, mm, date) = local_hm();
                    f.text(x, y, &format!("{hh:02}:{mm:02}"), pt * 2.4, th.text, true);
                    y += f.line_h(pt * 2.4);
                    f.text(x, y, &date, small, th.dim, false);
                }
                "power" => {
                    let b = s.power.battery;
                    let big = b.map(|b| format!("{b} %")).unwrap_or_else(|| if s.power.ac { "AC".into() } else { "—".into() });
                    let bw = f.text(x, y, &big, pt * 1.7, th.amber, true);
                    let left = match hud::time_left(&s.power, s.watts) {
                        Some((m, false)) => format!("{} left", hud::fmt_minutes(m)),
                        Some((m, true)) => format!("{} to full", hud::fmt_minutes(m)),
                        None => if s.power.ac { "plugged in".into() } else { String::new() },
                    };
                    let l = f.fit(&left, small, false, iw - bw - f.px(10.0));
                    f.text(x + bw + f.px(10.0), y + f.px(6.0), &l, small, th.text, false);
                    y += f.line_h(pt * 1.7) + f.px(2.0);
                    if let Some(b) = b {
                        meter(f, x, y, iw, f.px(8.0), b as f32 / 100.0, false);
                    }
                    y += f.px(12.0);
                    let m = f.fit(&format!("mode {}{}", s.mode, s.watts.map(|w| format!(" · {w:.1} W")).unwrap_or_default()), small, false, iw);
                    f.text(x, y, &m, small, th.dim, false);
                }
                "system" => {
                    let cpu = s.cpu.back().copied().unwrap_or(0.0);
                    let col_w = iw / 3;
                    let vals = [("CPU", format!("{:.0}%", cpu * 100.0), th.cyan), ("RAM", format!("{:.0}%", s.ram.0 * 100.0), th.violet), ("NPU", s.npu.map(|n| format!("{:.0}%", n * 100.0)).unwrap_or_else(|| "—".into()), th.amber)];
                    for (k, (n, v, col)) in vals.iter().enumerate() {
                        let cx = x + k as i32 * col_w;
                        f.text_a(cx, y, n, small, th.dim, false, 1.0);
                        let vy = y + f.line_h(small);
                        f.text(cx, vy, v, pt * 1.25, *col, true);
                    }
                    y += f.line_h(small) + f.line_h(pt * 1.25) + f.px(4.0);
                    let cpu_hist: Vec<f32> = s.cpu.iter().copied().collect();
                    sparkline(f, x, y, iw, (r.y + r.h - f.px(10.0) - y).max(f.px(8.0)), &cpu_hist, th.cyan);
                }
                "network" => {
                    let ssid = if s.wifi.ssid.is_empty() { "offline / wired".to_string() } else { s.wifi.ssid.clone() };
                    let t = f.fit(&ssid, pt * 1.25, true, iw);
                    f.text(x, y, &t, pt * 1.25, th.cyan, true);
                    y += f.line_h(pt * 1.25) + f.px(2.0);
                    if let Some(d) = s.wifi.dbm.as_ref().and_then(|d| d.parse::<f32>().ok()) {
                        meter(f, x, y, iw, f.px(7.0), ((d + 90.0) / 60.0).clamp(0.0, 1.0), false);
                    }
                    y += f.px(12.0);
                    f.text(x, y, &format!("↓ {}   ↑ {}", human(s.net.0), human(s.net.1)), small, th.text, false);
                }
                _ => {
                    // workspaces: a vertex chain, one node per workspace
                    let n = s.workspaces.len().max(1);
                    let step = (iw - f.px(20.0)) / n.max(2) as i32;
                    let cy = y + lh / 2 + f.px(4.0);
                    let mut prev: Option<(f32, f32)> = None;
                    for (k, (name, focused, occupied)) in s.workspaces.iter().enumerate() {
                        let px = (x + f.px(10.0) + k as i32 * step + step / 2) as f32;
                        if let Some(p) = prev {
                            f.cv.wline(p.0, p.1, px, cy as f32, th.green, 0.5);
                        }
                        let col = if *focused { th.amber } else if *occupied { th.green } else { th.dim };
                        f.cv.node(px, cy as f32, f.px(if *focused { 4.0 } else { 2.6 }) as f32, col, 1.0);
                        let nm = f.fit(name, small, false, step.max(f.px(12.0)));
                        label_c(f, px as i32, cy + f.px(8.0), &nm, small * 0.9, if *focused { th.text } else { th.dim }, *focused, 1.0);
                        prev = Some((px, cy as f32));
                    }
                    if s.workspaces.is_empty() {
                        f.text(x, y, "niri not running", small, th.dim, false);
                    }
                }
            }
        }
    }
}

impl App for Hub {
    fn kind(&self) -> SurfaceKind {
        if self.summon {
            SurfaceKind::Layer {
                namespace: "shadow-hub-summon".into(),
                layer: Layer::Overlay,
                anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                size: (0, 0),
                margin: (0, 0, 0, 0),
                exclusive: -1,
                keyboard: LayerKeyboard::Exclusive,
            }
        } else {
            SurfaceKind::Background { namespace: "shadow-hub".into(), keyboard: LayerKeyboard::OnDemand }
        }
    }

    fn pick_output(&mut self, _outs: &[shadow_ui::OutputDesc]) -> Option<String> {
        // summon on the monitor you are working on
        shadow_niri::json(&["focused-output"]).ok().and_then(|v| v.get("name").and_then(|n| n.as_str()).map(String::from))
    }

    fn tick_ms(&self) -> u64 {
        if self.animating() || self.field.moving() {
            16
        } else {
            1000
        }
    }

    fn tick(&mut self, ctx: &mut Ctx) {
        let dt = self.last_step.elapsed().as_secs_f32().clamp(0.0, 0.1);
        self.last_step = Instant::now();
        let mut redraw = false;
        let mut field_only = false;
        if self.field.moving() {
            self.field.step();
            field_only = true;
        }
        let rate = dt / 0.22;
        let (a, p) = (if self.open { 1.0 } else { 0.0 }, if self.page.is_some() { 1.0 } else { 0.0 });
        for (v, target) in [(&mut self.anim, a), (&mut self.page_anim, p)] {
            if (*v - target).abs() > 0.001 {
                *v = if *v < target { (*v + rate).min(target) } else { (*v - rate).max(target) };
                redraw = true;
            }
        }
        if self.last_sample.elapsed().as_millis() >= 3000 {
            self.last_sample = Instant::now();
            self.stats.sample(true);
            if self.page.is_some_and(|p| matches!(PAGES[p], "POWER" | "NETWORK" | "TASKS")) {
                self.rebuild_keep();
            }
            redraw = true;
        }
        let (h, m, _) = local_hm();
        if (h, m) != self.clock {
            self.clock = (h, m);
            redraw = true;
        }
        if redraw || self.drag.is_some() {
            self.overlay_dirty = true;
        }
        if redraw || field_only {
            ctx.redraw();
        }
    }

    fn message(&mut self, m: String, ctx: &mut Ctx) {
        if m == "reload" {
            self.reload();
            self.overlay_dirty = true;
            ctx.redraw();
        }
    }

    fn focus(&mut self, focused: bool, ctx: &mut Ctx) {
        if focused {
            self.had_focus = true;
        } else if self.summon && self.had_focus {
            ctx.quit();
        }
    }

    fn pointer(&mut self, ev: Ptr, x: i32, y: i32, surface: usize, ctx: &mut Ctx) {
        let Some(&(w, h)) = self.sizes.get(&surface) else {
            self.pending_ptr = Some((x, y, surface));
            return;
        };
        match ev {
            Ptr::Move => {
                if !self.summon && self.reactive {
                    let was = self.field.moving();
                    self.field.pointer(x as f32 / w.max(1) as f32, y as f32 / h.max(1) as f32);
                    if !was {
                        ctx.wake();
                    }
                }
                if let Some((i, ox, oy)) = self.drag {
                    self.overlay_dirty = true;
                    if surface == self.hub_surface {
                        self.widgets[i].pos = (((x - ox) as f32 / w as f32).clamp(0.03, 0.97), ((y - oy) as f32 / h as f32).clamp(0.03, 0.97));
                    }
                }
                ctx.wake();
            }
            Ptr::Leave => {
                self.field.leave();
                ctx.wake();
            }
            Ptr::Up => {
                if let Some((i, _, _)) = self.drag.take() {
                    self.overlay_dirty = true;
                    self.save_widget(i);
                    ctx.redraw();
                }
            }
            Ptr::Down => {}
        }
    }

    fn click(&mut self, x: i32, y: i32, ctx: &mut Ctx) {
        self.overlay_dirty = true;
        // the hub lives on one surface; clicks elsewhere only move the field
        let hits = self.hits.get(&self.hub_surface).cloned().unwrap_or_default();
        // topmost first: panel rows, nodes, core, widgets
        let hit = hits.iter().rev().find(|(_, r)| r.hit(x, y)).map(|(h, r)| (*h, *r));
        match hit {
            Some((Hit::Core, _)) => {
                self.open = !self.open;
                if !self.open {
                    self.page = None;
                    if self.summon {
                        ctx.quit();
                    }
                }
                ctx.wake();
            }
            Some((Hit::Node(i), _)) => {
                let p = if self.page == Some(i) { None } else { Some(i) };
                self.set_page(p, ctx);
            }
            Some((Hit::Row(i), _)) => self.activate(i, ctx),
            Some((Hit::Minus(i), _)) => {
                self.sel = i;
                self.adjust(i, -1, ctx)
            }
            Some((Hit::Plus(i), _)) => {
                self.sel = i;
                self.adjust(i, 1, ctx)
            }
            Some((Hit::Panel, _)) => {}
            Some((Hit::Widget(i), r)) => {
                let cx = r.x + r.w / 2;
                let cy = r.y + r.h / 2;
                self.drag = Some((i, x - cx, y - cy));
                ctx.wake();
            }
            None => {
                if self.summon {
                    ctx.quit();
                } else if self.page.is_some() {
                    self.set_page(None, ctx);
                } else if self.open {
                    self.open = false;
                    ctx.wake();
                }
            }
        }
        ctx.redraw();
    }

    fn key(&mut self, k: &Key, ctx: &mut Ctx) {
        self.overlay_dirty = true;
        let n = self.items.len();
        match k.sym {
            Keysym::Escape => {
                if self.page.is_some() && !self.summon {
                    self.set_page(None, ctx);
                } else if self.summon {
                    ctx.quit();
                } else {
                    self.open = false;
                    ctx.wake();
                }
            }
            Keysym::Tab | Keysym::ISO_Left_Tab => {
                self.open = true;
                let back = k.shift || k.sym == Keysym::ISO_Left_Tab;
                let p = match self.page {
                    None => 0,
                    Some(p) => (p + if back { PAGES.len() - 1 } else { 1 }) % PAGES.len(),
                };
                self.set_page(Some(p), ctx);
            }
            Keysym::space if self.page.is_none() => {
                self.open = !self.open;
                ctx.wake();
            }
            Keysym::Up if n > 0 => {
                let mut s = self.sel;
                while s > 0 {
                    s -= 1;
                    if !matches!(self.items[s].kind, Kind::Info) {
                        self.sel = s;
                        break;
                    }
                }
                self.armed = None;
            }
            Keysym::Down if n > 0 => {
                let mut s = self.sel;
                while s + 1 < n {
                    s += 1;
                    if !matches!(self.items[s].kind, Kind::Info) {
                        self.sel = s;
                        break;
                    }
                }
                self.armed = None;
            }
            Keysym::Left | Keysym::Right if self.page.is_some() && matches!(self.items.get(self.sel).map(|i| &i.kind), Some(Kind::Slider(_))) => {
                self.adjust(self.sel, if k.sym == Keysym::Right { 1 } else { -1 }, ctx);
            }
            Keysym::Return | Keysym::KP_Enter if self.page.is_some() => self.activate(self.sel, ctx),
            _ => {
                if self.page.is_some_and(|p| PAGES[p] == "APPS") {
                    if self.input.key(k) {
                        self.sel = 0;
                        self.build();
                    }
                } else if let Some(d) = k.typed().and_then(|t| t.chars().next()).and_then(|c| c.to_digit(10)) {
                    if (1..=PAGES.len() as u32).contains(&d) {
                        self.open = true;
                        self.set_page(Some(d as usize - 1), ctx);
                    }
                } else if !self.summon && self.page.is_none() {
                    // typing on the desktop opens APPS with that text
                    if let Some(t) = k.typed().filter(|t| t.chars().all(|c| c.is_alphanumeric())) {
                        self.open = true;
                        self.set_page(Some(0), ctx);
                        self.input.set(t);
                        self.build();
                    }
                }
            }
        }
        ctx.wake();
        ctx.redraw();
    }

    fn draw(&mut self, f: &mut Frame) {
        let (w, h) = (f.cv.w as i32, f.cv.h as i32);
        self.sizes.insert(f.index, (w, h));
        if let Some((x, y, s)) = self.pending_ptr.take() {
            if s == f.index && !self.summon {
                self.field.pointer(x as f32 / w.max(1) as f32, y as f32 / h.max(1) as f32);
                while self.field.moving() {
                    self.field.step();
                }
            }
        }
        // which surface carries the hub: [hub] output, else the first
        let is_hub = match (&self.main_output, f.output) {
            (Some(want), Some(o)) => want == o,
            _ => f.index == 0,
        } || self.summon;
        if is_hub {
            self.hub_surface = f.index;
        }
        let th = f.theme.clone();
        if self.summon {
            // over the windows: tinted glass and a lattice, the hub opened
            f.cv.clear(th.bg, 0.62);
            shadow_ui::widgets::lattice(f, 0, 0, w, h, th.cyan, 0.14);
        } else {
            match self.image(f.output, w as usize, h as usize) {
                Some(px) if px.len() == f.cv.buf.len() => f.cv.buf.copy_from_slice(&px),
                _ => self.field.draw(&mut f.cv, &th),
            }
        }
        if !is_hub {
            return;
        }
        let (uw, uh) = (w as usize, h as usize);
        if self.overlay_dirty || self.overlay_size != (uw, uh) {
            let mut layer = std::mem::take(&mut self.overlay);
            layer.clear();
            layer.resize(uw * uh * 4, 0);
            {
                let mut of = Frame {
                    cv: shadow_ui::Canvas { buf: &mut layer, w: uw, h: uh },
                    scale: f.scale,
                    text: f.text,
                    fonts: &mut *f.fonts,
                    theme: f.theme,
                    output: f.output,
                    index: f.index,
                    focused: f.focused,
                    want_size: None,
                    logical: f.logical,
                };
                let c = (w as f32 / 2.0, h as f32 / 2.0);
                let mut hits = vec![];
                self.draw_widgets(&mut of, c, (w, h), &mut hits);
                self.draw_nodes(&mut of, c, &mut hits);
                self.draw_core(&mut of, c, &mut hits);
                self.draw_panel(&mut of, c, (w, h), &mut hits);
                self.hits.insert(f.index, hits);
            }
            self.overlay = layer;
            self.overlay_size = (uw, uh);
            self.overlay_dirty = false;
        }
        composite(f.cv.buf, &self.overlay);
    }
}

/// premultiplied `src` over `dst` (same size); fully transparent pixels — most of the layer — are
/// skipped
fn composite(dst: &mut [u8], src: &[u8]) {
    if dst.len() != src.len() {
        return;
    }
    for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
        let a = s[3] as u32;
        if a == 0 {
            continue;
        }
        if a == 255 {
            d.copy_from_slice(s);
            continue;
        }
        let ia = 255 - a;
        for k in 0..4 {
            d[k] = (s[k] as u32 + (d[k] as u32 * ia + 127) / 255).min(255) as u8;
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
    }
    std::thread::spawn(move || loop {
        let mut sig: libc::c_int = 0;
        // SAFETY: set built as above; sigwait blocks this thread only
        let ok = unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, libc::SIGUSR1);
            libc::sigwait(&set, &mut sig)
        };
        if ok == 0 && tx.send("reload".into()).is_err() {
            break;
        }
    });
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let page_of = |s: &str| PAGES.iter().position(|p| p.eq_ignore_ascii_case(s));
    let hub = match a.as_slice() {
        // the desktop: supervised (see `supervise`), unless this is the supervised child or a snapshot
        [] if std::env::var_os("SHADOW_HUB_CHILD").is_none() && std::env::var_os("SHADOW_SNAPSHOT").is_none() => std::process::exit(supervise()),
        [] => {
            std::panic::set_hook(Box::new(|p| {
                eprintln!("shadow-hub panicked: {p}");
            }));
            Hub::new(false, None)
        }
        ["doctor"] => std::process::exit(doctor()),
        ["--open", p, ..] => {
            let mut h = Hub::new(false, page_of(p));
            h.open = true;
            h.anim = 1.0;
            h.page_anim = if h.page.is_some() { std::env::var("SHADOW_HUB_PAGE_ANIM").ok().and_then(|v| v.parse().ok()).unwrap_or(1.0) } else { 0.0 };
            h
        }
        ["summon", rest @ ..] => {
            // a second summon closes the first (Mod+Space toggles)
            let me = std::process::id().to_string();
            let (_, o) = cmd_out(&["pgrep", "-f", "^(/[^ ]*/)?shadow-hub summon"]);
            let others: Vec<&str> = o.split_whitespace().filter(|p| *p != me).collect();
            if !others.is_empty() {
                for p in others {
                    let _ = cmd_out(&["kill", p]);
                }
                return;
            }
            let mut h = Hub::new(true, rest.first().and_then(|p| page_of(p)));
            // in a snapshot there is no animation clock: show it settled
            if std::env::var_os("SHADOW_SNAPSHOT").is_some() {
                h.anim = 1.0;
                h.page_anim = if h.page.is_some() { 1.0 } else { 0.0 };
            }
            h
        }
        ["reset-layout"] => {
            reset_layout();
            let _ = cmd_out(&["pkill", "-USR1", "-f", "^(/[^ ]*/)?shadow-hub$"]);
            println!("hub widgets back in their default places");
            return;
        }
        _ => {
            eprintln!("usage: shadow-hub [summon [apps|settings|power|system|network|fix|look|tasks] | reset-layout | doctor]");
            std::process::exit(2);
        }
    };
    let (tx, rx) = channel::channel::<String>();
    signal_thread(tx);
    if let Err(e) = run(Box::new(hub), Some(rx)) {
        eprintln!("shadow-hub: {e}");
        std::process::exit(1);
    }
}

// ── supervision, fallback and diagnostics ──────────────────────────────────────────────────────

fn log_path() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME").map(std::path::PathBuf::from).unwrap_or_else(|| shadow_apps::home().join(".local/state"));
    base.join("shadow-tui/hub.log")
}

fn log(msg: &str) {
    let p = log_path();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    // keep the log small: start over past 256 KB
    if std::fs::metadata(&p).map(|m| m.len() > 256 * 1024).unwrap_or(false) {
        let _ = std::fs::rename(&p, p.with_extension("log.old"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
        use std::io::Write;
        let _ = writeln!(f, "{} {msg}", cmd_out(&["date", "-Is"]).1.trim());
    }
}

/// niri (the session) still answers; when it doesn't, the session is over and nothing restarts
fn session_alive() -> bool {
    std::env::var_os("NIRI_SOCKET").is_some_and(|p| std::path::Path::new(&p).exists()) && cmd_out(&["niri", "msg", "version"]).0 == 0
}

extern "C" fn ignore_signal(_: libc::c_int) {}

/// Run the desktop hub as a child and look after it. Returns the exit code for this process.
fn supervise() -> i32 {
    // SIGUSR1 (reload) is meant for the child; a handler (not SIG_IGN, which the child would
    // inherit) keeps the supervisor alive when `pkill -USR1 -f shadow-hub` reaches it too
    // SAFETY: installing a signal handler that does nothing
    unsafe {
        libc::signal(libc::SIGUSR1, ignore_signal as *const () as libc::sighandler_t);
    }
    let exe = std::env::current_exe().unwrap_or_else(|_| "shadow-hub".into());
    let mut crashes: Vec<Instant> = vec![];
    loop {
        let errf = std::fs::OpenOptions::new().create(true).append(true).open(log_path());
        let mut cmd = std::process::Command::new(&exe);
        // same argv as the supervisor, so `pkill -f '^…shadow-hub$'` reaches both
        cmd.arg0("shadow-hub").env("SHADOW_HUB_CHILD", "1").stdin(std::process::Stdio::null());
        if let Ok(f) = errf {
            cmd.stderr(f);
        }
        let started = Instant::now();
        let status = match cmd.status() {
            Ok(s) => s,
            Err(e) => {
                log(&format!("cannot start the hub: {e}"));
                fallback(&format!("could not start: {e}"));
                return 1;
            }
        };
        use std::os::unix::process::ExitStatusExt;
        if status.success() {
            return 0;
        }
        // stopped by a signal (logout, `pkill shadow-hub`, the updater restarting it): not a crash
        if let Some(sig) = status.signal() {
            if sig == libc::SIGTERM || sig == libc::SIGKILL || sig == libc::SIGINT || sig == libc::SIGHUP {
                return 0;
            }
        }
        if !session_alive() {
            return 0;
        }
        log(&format!("the hub exited ({status}) after {:.0} s", started.elapsed().as_secs_f32()));
        crashes.retain(|t| t.elapsed().as_secs() < 120);
        crashes.push(Instant::now());
        if crashes.len() >= 3 {
            fallback(&format!("it stopped {} times in 2 minutes ({status})", crashes.len()));
            return 1;
        }
        std::thread::sleep(std::time::Duration::from_millis(800));
    }
}

/// The hub can't run: bring back the 7.x desktop (side bar + wallpaper) and say why.
fn fallback(why: &str) {
    log(&format!("falling back to the side bar and the wallpaper: {why}"));
    for (prog, pattern) in [("shadow-wallpaper", "^(/[^ ]*/)?shadow-wallpaper$"), ("shadow-bar", "^(/[^ ]*/)?shadow-bar$")] {
        if cmd_out(&["pgrep", "-f", pattern]).0 != 0 {
            spawn(prog);
        }
    }
    let _ = cmd_out(&[
        "notify-send",
        "-a",
        "Shadow",
        "Shadow hub stopped",
        &format!("Switched to the side bar so your desktop keeps working ({why}). Details: shadow-hub doctor, ~/.local/state/shadow-tui/hub.log"),
    ]);
}

/// `shadow-hub doctor`: what the hub needs, checked on this machine
fn doctor() -> i32 {
    let mut bad = 0;
    let mut line = |ok: Option<bool>, what: &str, detail: String| {
        let mark = match ok {
            Some(true) => "ok  ",
            Some(false) => {
                bad += 1;
                "FAIL"
            }
            None => "note",
        };
        println!("[{mark}] {what}: {detail}");
    };
    let text = conf();
    let enabled = get_in(&text, "hub", "enabled").map(|v| v.trim() != "false").unwrap_or(true);
    line(Some(enabled), "[hub] enabled", if enabled { "yes".into() } else { "no — the side bar and wallpaper are used (set enabled = true)".into() });
    let (c, v) = cmd_out(&["niri", "--version"]);
    line(Some(c == 0), "niri", if c == 0 { v.trim().to_string() } else { "not found".into() });
    line(Some(session_alive()), "niri session", if session_alive() { "running, answers niri msg".into() } else { "not reachable from here (NIRI_SOCKET unset or niri not running)".into() });
    let kdl = shadow_niri::config::path();
    let cfg = std::fs::read_to_string(&kdl).unwrap_or_default();
    let spawns = cfg.contains("spawn-at-startup \"shadow-hub\"");
    line(Some(spawns || !enabled), "niri config starts the hub", if spawns { format!("yes ({})", kdl.display()) } else { format!("no — {} still has the old startup programs; run: shadow setup niri  (then log out and in)", kdl.display()) });
    if cfg.contains("spawn-at-startup") && !cfg.contains("overview {") && spawns {
        line(None, "niri compatibility mode", "on (this niri is older; cosmetic lines left out)".into());
    }
    let (_, pids) = cmd_out(&["pgrep", "-f", "^(/[^ ]*/)?shadow-hub$"]);
    let n = pids.split_whitespace().count();
    line(Some(n >= 1 || !enabled), "hub running", match n {
        0 => "no — start it with: shadow-hub &   (or log out and in)".into(),
        1 => "yes (no supervisor: started by hand?)".into(),
        _ => format!("yes ({n} processes: supervisor + hub)"),
    });
    let others: Vec<&str> = ["shadow-bar", "shadow-wallpaper"].into_iter().filter(|p| cmd_out(&["pgrep", "-f", &format!("^(/[^ ]*/)?{p}$")]).0 == 0).collect();
    if !others.is_empty() {
        line(None, "also running", format!("{} (left over from 7.x or the fallback; Mod+Shift+B toggles the bar)", others.join(", ")));
    }
    let (wall, _) = wall::choice(&text, None);
    line(None, "background", if wall == "vertices" { "the 3D vertex field".into() } else { format!("{wall} — an image is set, so the 3D field is not shown (LOOK → Vertices, or: shadow-wallpaper set vertices)") });
    let reactive = get_in(&text, "hub", "reactive").map(|v| v.trim() != "false").unwrap_or(true);
    if !reactive {
        line(None, "[hub] reactive", "false — the field does not follow the cursor".into());
    }
    // a keyboard layout switch on Super+Space would swallow the hub's Mod+Space
    let xkb = get_in(&text, "input", "xkb_options").unwrap_or_default();
    if xkb.contains("win_space") {
        line(Some(false), "Mod+Space", format!("taken by the keyboard layout switch ({}) — remove win_space_toggle from [input] xkb_options, or use Mod+A", xkb.trim_matches('"')));
    }
    let wd = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
    line(Some(!wd.is_empty()), "Wayland display", if wd.is_empty() { "WAYLAND_DISPLAY unset (run this inside the session)".into() } else { wd });
    // frame cost at this machine's speed, for the laptop panel size
    let th = shadow_ui::Theme::load();
    let (w, h) = (2560usize, 1600usize);
    let mut buf = vec![0u8; w * h * 4];
    let mut fld = Field::new();
    fld.draw(&mut shadow_ui::Canvas { buf: &mut buf, w, h }, &th);
    let t = Instant::now();
    for i in 0..10 {
        fld.pointer(0.3 + i as f32 * 0.02, 0.6);
        fld.step();
        fld.draw(&mut shadow_ui::Canvas { buf: &mut buf, w, h }, &th);
    }
    let ms = t.elapsed().as_secs_f64() * 100.0;
    line(Some(ms < 25.0), "field frame at 2560×1600", format!("{ms:.1} ms{}", if ms >= 25.0 { " — slow here; [hub] reactive = false keeps it still" } else { "" }));
    let lp = log_path();
    let tail: Vec<String> = std::fs::read_to_string(&lp).unwrap_or_default().lines().rev().take(8).map(String::from).collect();
    if tail.is_empty() {
        line(None, "hub log", format!("{} (empty)", lp.display()));
    } else {
        println!("--- last lines of {} ---", lp.display());
        for l in tail.iter().rev() {
            println!("{l}");
        }
    }
    if bad == 0 {
        0
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widget_positions_parse_and_clamp() {
        let t = "[hub.widgets]\nclock = \"0.300,0.400\"\npower = \"off\"\nsystem = \"5,-1\"\n";
        let w = load_widgets(t);
        assert_eq!(w[0].pos, (0.3, 0.4));
        assert!(!w[1].on);
        assert_eq!(w[2].pos, (0.98, 0.02), "out-of-range values are pulled back on screen");
        assert_eq!(w[3].pos, (0.85, 0.74), "missing = default");
    }

    #[test]
    fn ease_is_monotonic_and_bounded() {
        let mut prev = 0.0;
        for i in 0..=20 {
            let v = ease(i as f32 / 20.0);
            assert!(v >= prev && (0.0..=1.0).contains(&v));
            prev = v;
        }
        assert_eq!(ease(1.0), 1.0);
    }
}
