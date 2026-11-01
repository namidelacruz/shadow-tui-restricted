//! shadow-flyout — the panels that float beside the side bar's buttons.
//!
//!   shadow-flyout apps|settings|system [--output NAME] [--top Y] [--max-h H]
//!
//! Started by shadow-bar when you click APPS, SETTINGS or SYSTEM; the same button closes it again.
//! It floats next to the button (a layer surface: opaque card, rounded, nothing else moves), takes
//! the keyboard while open and closes on Esc, after an action, or when focus moves elsewhere.
//!   apps      type to search · ↑↓ · Enter / click launches · "All apps…" opens the launcher
//!   settings  brightness · volume (Enter mutes) · power mode · Wi-Fi · displays · side bar · all settings
//!   system    task manager · update · security console · lock · log out · suspend · reboot · shut down
//!             (log out / reboot / shut down ask for a second press)
//! Tab switches page.
#[path = "../../../bar/src/sys.rs"]
#[allow(dead_code)]
mod hud;

use shadow_apps::desktop::{self, Entry};
use shadow_apps::{cmd_out, spawn};
use shadow_ui::widgets::{field, meter, selection, Input, Rect};
use shadow_ui::{run, Anchor, App, Ctx, Frame, Key, Keysym, Layer, LayerKeyboard, SurfaceKind};

const PAGES: [&str; 3] = ["apps", "settings", "system"];
const WIDTH: u32 = 360;
const MODES: [&str; 4] = ["auto", "quiet", "balanced", "performance"];

#[derive(Clone, PartialEq)]
enum Kind {
    /// run a command and close
    Run(String),
    /// run only on a second press
    Confirm(String),
    /// a value with − / + (id)
    Slider(&'static str),
    /// a choice cycled with ← → (id)
    Cycle(&'static str),
    /// toggle without closing
    Toggle(String),
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

fn runtime() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into())).join("shadow")
}

fn have(cmd: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
}

/// 0..=1 brightness of the first backlight
fn brightness() -> Option<f32> {
    let root = std::path::PathBuf::from(std::env::var_os("SHADOW_SYS_ROOT").unwrap_or_else(|| "/".into()));
    let d = std::fs::read_dir(root.join("sys/class/backlight")).ok()?.flatten().next()?.path();
    let r = |f: &str| std::fs::read_to_string(d.join(f)).ok()?.trim().parse::<f32>().ok();
    Some((r("brightness")? / r("max_brightness")?.max(1.0)).clamp(0.0, 1.0))
}

/// (0..=1.5 volume, muted) of the default sink
fn volume() -> Option<(f32, bool)> {
    let (c, o) = cmd_out(&["wpctl", "get-volume", "@DEFAULT_AUDIO_SINK@"]);
    if c != 0 {
        return None;
    }
    let v = o.split_whitespace().nth(1)?.parse::<f32>().ok()?;
    Some((v, o.contains("MUTED")))
}

fn power_mode() -> String {
    let (c, o) = cmd_out(&["shadow-powerd", "mode"]);
    let m = o.lines().find_map(|l| l.strip_prefix("mode")).and_then(|v| v.trim_start().strip_prefix(':')).and_then(|v| v.split_whitespace().next()).map(String::from);
    match m {
        Some(m) if c == 0 => m,
        _ => "unavailable".into(),
    }
}

fn uptime() -> String {
    let s = std::fs::read_to_string("/proc/uptime").ok().and_then(|u| u.split_whitespace().next()?.parse::<f64>().ok()).unwrap_or(0.0) as u64;
    if s >= 86400 {
        format!("{}d {}h", s / 86400, s % 86400 / 3600)
    } else {
        format!("{}h{:02}", s / 3600, s % 3600 / 60)
    }
}

struct Flyout {
    page: usize,
    output: Option<String>,
    top: i32,
    max_h: i32,
    items: Vec<Item>,
    all: Vec<Entry>,
    hist: std::collections::HashMap<String, i32>,
    input: Input,
    sel: usize,
    armed: Option<usize>,
    rows: Vec<(usize, Rect)>,
    minus_plus: Vec<(usize, Rect, i32)>,
    had_focus: bool,
    info: String,
    info_err: bool,
}

impl Flyout {
    fn build(&mut self) {
        self.armed = None;
        self.info_err = false;
        self.items = match PAGES[self.page] {
            "apps" => {
                let q = self.input.text.trim().to_lowercase();
                let mut v: Vec<(i32, &Entry)> = self
                    .all
                    .iter()
                    .filter_map(|e| {
                        let base = if e.tag == "shadow" { 6 } else { 0 };
                        desktop::score(&q, &format!("{} {} {}", e.name, e.keywords, e.detail)).map(|s| (s * 4 + self.hist.get(&e.id).copied().unwrap_or(0).min(50) + base, e))
                    })
                    .collect();
                v.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.to_lowercase().cmp(&b.1.name.to_lowercase())));
                let mut items: Vec<Item> = v
                    .into_iter()
                    .take(9)
                    .map(|(_, e)| {
                        let cmd = if e.terminal { format!("shadow-term -e {}", e.cmd) } else { e.cmd.clone() };
                        Item { label: e.name.clone(), detail: e.detail.clone(), kind: Kind::Run(format!("@{}\u{1}{cmd}", e.id)) }
                    })
                    .collect();
                items.push(item("All apps…", "the full launcher", Kind::Run("shadow-launch".into())));
                items
            }
            "settings" => {
                let mut v = vec![];
                if let Some(b) = brightness() {
                    v.push(item("Brightness", format!("{:.0} %", b * 100.0), Kind::Slider("brightness")));
                }
                if let Some((vol, muted)) = volume() {
                    v.push(item("Volume", if muted { "muted".to_string() } else { format!("{:.0} %", vol * 100.0) }, Kind::Slider("volume")));
                }
                v.push(item("Power mode", power_mode(), Kind::Cycle("mode")));
                let w = hud::wifi();
                v.push(item("Wi-Fi", if w.ssid.is_empty() { "not connected".into() } else { w.ssid }, Kind::Run("shadow-net".into())));
                v.push(item("Displays", "scale, text size, refresh", Kind::Run("shadow-config Displays".into())));
                v.push(item("Side bar", "hide / show (Mod+Shift+B)", Kind::Toggle("pkill -USR1 -x shadow-bar".into())));
                v.push(item("All settings…", "shadow-config", Kind::Run("shadow-config".into())));
                v
            }
            _ => {
                let mut v = vec![
                    item("Task manager", "CPU · RAM · NPU · GPU · processes", Kind::Run("shadow-tasks".into())),
                    item("Update", "check on demand, changed files only", Kind::Run("shadow-update".into())),
                ];
                if have("shadow-sec") {
                    v.push(item("Security console", "precheck · enable modules (password)", Kind::Run("shadow-term --hold -e shadow-sec console".into())));
                }
                v.extend([
                    item("Lock", "Mod+Alt+L", Kind::Run("shadow-lock".into())),
                    item("Log out", "leave the session", Kind::Confirm("niri msg action quit --skip-confirmation".into())),
                    item("Suspend", "", Kind::Run("systemctl suspend".into())),
                    item("Reboot", "", Kind::Confirm("systemctl reboot".into())),
                    item("Shut down", "", Kind::Confirm("systemctl poweroff".into())),
                ]);
                v
            }
        };
        let p = hud::power();
        let left = match hud::time_left(&p, p.watts) {
            Some((m, false)) => format!(" · {} left", hud::fmt_minutes(m)),
            Some((m, true)) => format!(" · {} to full", hud::fmt_minutes(m)),
            None => String::new(),
        };
        let host = std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default().trim().to_string();
        let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default().trim().to_string();
        self.info = match PAGES[self.page] {
            "system" => format!("{host} · {kernel} · up {}{}", uptime(), p.battery.map(|b| format!(" · {b}%{left}")).unwrap_or_default()),
            "settings" => "← → adjust · Enter opens / mutes".into(),
            _ => "type to search".into(),
        };
        self.sel = self.sel.min(self.items.len().saturating_sub(1));
    }

    fn adjust(&mut self, i: usize, dir: i32, ctx: &mut Ctx) {
        let Some(it) = self.items.get(i) else { return };
        match &it.kind {
            Kind::Slider("brightness") => {
                // logind SetBrightness (no root needed), brightnessctl as fallback
                let _ = cmd_out(&["shadow-displayd", "brightness", if dir > 0 { "5%+" } else { "5%-" }]);
            }
            Kind::Slider("volume") => {
                let _ = cmd_out(&["wpctl", "set-volume", "-l", "1.0", "@DEFAULT_AUDIO_SINK@", if dir > 0 { "5%+" } else { "5%-" }]);
            }
            Kind::Cycle("mode") if it.detail != "unavailable" => {
                let cur = MODES.iter().position(|m| *m == it.detail).unwrap_or(0) as i32;
                let next = MODES[(cur + dir).rem_euclid(MODES.len() as i32) as usize];
                let _ = cmd_out(&["shadow-powerd", "mode", next]);
            }
            _ => return,
        }
        let keep = self.sel;
        self.build();
        self.sel = keep;
        ctx.redraw();
    }

    fn activate(&mut self, i: usize, ctx: &mut Ctx) {
        let Some(it) = self.items.get(i).cloned() else { return };
        match it.kind {
            Kind::Run(cmd) if cmd.starts_with("systemctl ") => {
                if self.power(&cmd, ctx) {
                    ctx.quit();
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
                ctx.quit();
            }
            Kind::Confirm(cmd) => {
                if self.armed == Some(i) {
                    self.armed = None;
                    if self.power(&cmd, ctx) {
                        ctx.quit();
                    }
                } else {
                    self.armed = Some(i);
                    ctx.redraw();
                }
            }
            Kind::Toggle(cmd) => {
                spawn(&cmd);
                ctx.quit();
            }
            Kind::Slider("volume") => {
                let _ = cmd_out(&["wpctl", "set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"]);
                self.build();
                self.sel = i;
                ctx.redraw();
            }
            Kind::Slider(_) | Kind::Cycle(_) => self.adjust(i, 1, ctx),
        }
    }

    /// Suspend / reboot / shut down: run and wait, so a refusal (an update holding an inhibitor,
    /// another user logged in, no polkit agent) is shown instead of silently doing nothing.
    fn power(&mut self, cmd: &str, ctx: &mut Ctx) -> bool {
        let argv: Vec<&str> = cmd.split_whitespace().collect();
        let (c, o) = cmd_out(&argv);
        if c == 0 {
            return true;
        }
        let why = o.lines().find(|l| !l.trim().is_empty()).unwrap_or("refused").trim().to_string();
        let hint = if why.contains("inhibit") {
            " — wait for it to finish"
        } else if why.contains("authentication agent") || why.contains("Interactive authentication required") {
            " — no polkit agent to ask for the password (re-login, or install mate-polkit)"
        } else {
            ""
        };
        self.info = format!("✗ {why}{hint}");
        self.info_err = true;
        ctx.redraw();
        false
    }

    /// expected logical height, to keep the panel on screen
    fn est_height(&self) -> i32 {
        let lh = 11.0 * 1.333 * 1.3;
        let row = (lh + 12.0) as i32;
        let head = (lh + 30.0) as i32;
        let search = if PAGES[self.page] == "apps" { (lh + 18.0) as i32 } else { 0 };
        28 + head + search + row * self.items.len() as i32 + (lh as i32 + 16)
    }
}

impl App for Flyout {
    fn kind(&self) -> SurfaceKind {
        let h = self.est_height();
        let top = self.top.min(self.max_h - h - 12).max(8);
        SurfaceKind::Layer {
            namespace: "shadow-flyout".into(),
            layer: Layer::Top,
            anchor: Anchor::TOP | Anchor::RIGHT,
            size: (WIDTH, h as u32),
            // exclusive zone 0: placed beside the bar's reserved strip, not under it
            margin: (top, 10, 0, 0),
            exclusive: 0,
            keyboard: LayerKeyboard::Exclusive,
        }
    }

    fn pick_output(&mut self, _outs: &[shadow_ui::OutputDesc]) -> Option<String> {
        self.output.clone()
    }

    fn focus(&mut self, focused: bool, ctx: &mut Ctx) {
        if focused {
            self.had_focus = true;
        } else if self.had_focus {
            ctx.quit();
        }
    }

    fn key(&mut self, k: &Key, ctx: &mut Ctx) {
        let n = self.items.len();
        match k.sym {
            Keysym::Escape => ctx.quit(),
            Keysym::Tab | Keysym::ISO_Left_Tab => {
                let d = if k.shift || k.sym == Keysym::ISO_Left_Tab { 2 } else { 1 };
                self.page = (self.page + d) % PAGES.len();
                self.sel = 0;
                self.input.clear();
                self.build();
                let _ = std::fs::write(runtime().join("flyout"), format!("{} {}\n", PAGES[self.page], std::process::id()));
                ctx.redraw();
            }
            Keysym::Up => {
                self.sel = (self.sel + n - 1) % n.max(1);
                self.armed = None;
                ctx.redraw();
            }
            Keysym::Down => {
                self.sel = (self.sel + 1) % n.max(1);
                self.armed = None;
                ctx.redraw();
            }
            Keysym::Left if PAGES[self.page] != "apps" => self.adjust(self.sel, -1, ctx),
            Keysym::Right if PAGES[self.page] != "apps" => self.adjust(self.sel, 1, ctx),
            Keysym::Return | Keysym::KP_Enter => self.activate(self.sel, ctx),
            _ if PAGES[self.page] == "apps" => {
                if self.input.key(k) {
                    self.sel = 0;
                    self.build();
                    ctx.redraw();
                }
            }
            _ => {}
        }
    }

    fn click(&mut self, x: i32, y: i32, ctx: &mut Ctx) {
        if let Some(&(i, _, d)) = self.minus_plus.iter().find(|(_, r, _)| r.hit(x, y)) {
            self.sel = i;
            self.adjust(i, d, ctx);
            return;
        }
        if let Some(&(i, _)) = self.rows.iter().find(|(_, r)| r.hit(x, y)) {
            self.sel = i;
            self.activate(i, ctx);
        }
    }

    fn scroll(&mut self, lines: i32, ctx: &mut Ctx) {
        if PAGES[self.page] == "settings" {
            self.adjust(self.sel, -lines.signum(), ctx);
        } else {
            let n = self.items.len().max(1) as i32;
            self.sel = (self.sel as i32 + lines).clamp(0, n - 1) as usize;
            ctx.redraw();
        }
    }

    fn draw(&mut self, f: &mut Frame) {
        let th = f.theme.clone();
        let pt = th.font_pt;
        let spt = pt * 0.84;
        let lh = f.line_h(pt);
        let slh = f.line_h(spt);
        let pad = f.px(16.0);
        let edge = f.px(4.0);
        let row_h = lh + f.px(12.0);
        let w = f.cv.w as i32;
        // content-sized
        let head = lh + f.px(30.0);
        let search = if PAGES[self.page] == "apps" { lh + f.px(18.0) } else { 0 };
        let need = edge * 2 + pad + head + search + row_h * self.items.len() as i32 + f.line_h(spt) + f.px(22.0);
        let need_l = (need as f32 / f.scale).ceil() as u32;
        if need_l != f.logical.1 {
            f.want_size = Some((WIDTH, need_l));
        }
        let h = f.cv.h as i32;
        let rad = f.px(th.radius + 4.0) as f32;
        f.cv.clear((0, 0, 0), 0.0);
        f.cv.glow(edge, edge, w - 2 * edge, h - 2 * edge, rad, edge, th.amber, 0.18);
        // opaque card: what is behind never shows through
        f.cv.rrect_v(edge, edge, w - 2 * edge, h - 2 * edge, rad, th.glass, th.bg, 1.0);
        f.cv.scanlines(edge, edge + f.px(8.0), w - 2 * edge, h - 2 * edge - f.px(16.0), f.px(4.0).max(3), th.cyan, 0.02);
        f.cv.rstroke(edge, edge, w - 2 * edge, h - 2 * edge, rad, f.hair(), th.amber, 0.55);
        // header: page tag + page dots
        let x0 = edge + pad;
        let mut y = edge + f.px(14.0);
        let tag = format!("  {}  ", PAGES[self.page].to_uppercase());
        let tw = f.text_w(&tag, pt, true) + f.px(12.0);
        f.cv.tag(x0, y, tw, lh + f.px(6.0), th.amber, th.violet, 1.0);
        f.text(x0, y + f.px(3.0), &tag, pt, th.bg, true);
        for (i, _) in PAGES.iter().enumerate() {
            let dx = w - edge - pad - (PAGES.len() - i) as i32 * f.px(14.0);
            let r = f.px(4.0);
            let cy = y + (lh + f.px(6.0)) / 2 - r;
            if i == self.page {
                f.cv.glow(dx, cy, 2 * r, 2 * r, r as f32, f.px(4.0), th.cyan, 0.5);
                f.cv.rrect(dx, cy, 2 * r, 2 * r, r as f32, th.cyan, 1.0);
            } else {
                f.cv.rstroke(dx, cy, 2 * r, 2 * r, r as f32, f.hair(), th.dim, 0.8);
            }
        }
        y += lh + f.px(14.0);
        f.cv.beam(x0, y, w - 2 * x0, f.hair(), th.amber, th.cyan, 0.7);
        y += f.px(10.0);
        if PAGES[self.page] == "apps" {
            field(f, x0, y, w - 2 * x0, lh + f.px(10.0), true);
            let px = x0 + f.px(10.0);
            if self.input.text.is_empty() {
                f.text_a(px, y + f.px(5.0), "search apps…", pt, th.dim, false, 0.8);
                f.cv.fill(px, y + f.px(5.0), f.hair().max(f.px(2.0)), lh, th.amber, 1.0);
            } else {
                self.input.draw(f, px, y + f.px(5.0), pt, true);
            }
            y += search;
        }
        self.rows.clear();
        self.minus_plus.clear();
        for (i, it) in self.items.clone().iter().enumerate() {
            let rr = Rect { x: x0 - f.px(4.0), y, w: w - 2 * x0 + f.px(8.0), h: row_h - f.px(4.0) };
            if i == self.sel {
                selection(f, rr.x, rr.y, rr.w, rr.h);
            }
            let ty = y + (rr.h - lh) / 2;
            let tx = x0 + f.px(12.0);
            let armed = self.armed == Some(i);
            let label = if armed { format!("{} — press again", it.label) } else { it.label.clone() };
            let col = if armed { th.crimson } else if matches!(it.kind, Kind::Run(ref c) if !c.starts_with('@')) && PAGES[self.page] == "apps" { th.amber } else { th.text };
            // slider geometry: [label value] [meter] (−) (+)
            let bs = rr.h - f.px(10.0);
            let slider_w = f.px(8.0) * 3 + bs * 2 + f.px(64.0);
            let reserve = match it.kind {
                Kind::Slider(_) => slider_w + f.text_w(&it.detail, spt, false) + f.px(18.0),
                Kind::Cycle(_) => f.text_w(&format!("‹ {} ›", it.detail), pt, false) + f.px(24.0),
                _ => f.px(16.0),
            };
            let label = f.fit(&label, pt, i == self.sel || armed, rr.x + rr.w - tx - reserve);
            let lw = f.text(tx, ty, &label, pt, col, i == self.sel || armed);
            match &it.kind {
                Kind::Slider(id) => {
                    // − meter +
                    let px_ = rr.x + rr.w - f.px(8.0) - bs;
                    let mx = px_ - f.px(8.0) - bs;
                    let mw = f.px(64.0);
                    let mstart = mx - f.px(8.0) - mw;
                    let v = match *id {
                        "brightness" => brightness().unwrap_or(0.0),
                        _ => volume().map(|(v, m)| if m { 0.0 } else { v }).unwrap_or(0.0),
                    };
                    meter(f, mstart, y + rr.h / 2 - f.px(3.0), mw, f.px(6.0), v.min(1.0), false);
                    for (bx, sym, d) in [(mx, "−", -1), (px_, "+", 1)] {
                        let by = y + (rr.h - bs) / 2;
                        f.cv.rrect_v(bx, by, bs, bs, bs as f32 / 2.0, th.glass, th.glass_lo, 1.0);
                        f.cv.rstroke(bx, by, bs, bs, bs as f32 / 2.0, f.hair(), th.cyan, 0.6);
                        let sw = f.text_w(sym, pt, true);
                        f.text(bx + (bs - sw) / 2, by + (bs - lh) / 2, sym, pt, th.cyan, true);
                        self.minus_plus.push((i, Rect { x: bx, y: by, w: bs, h: bs }, d));
                    }
                    let d = f.fit(&it.detail, spt, false, mstart - tx - lw - f.px(16.0));
                    f.text(tx + lw + f.px(10.0), ty + (lh - slh) / 2, &d, spt, th.dim, false);
                }
                Kind::Cycle(_) => {
                    let s = format!("‹ {} ›", it.detail);
                    let sw = f.text_w(&s, pt, false);
                    f.text(rr.x + rr.w - f.px(12.0) - sw, ty, &s, pt, th.cyan, false);
                }
                _ => {
                    if !it.detail.is_empty() {
                        let d = f.fit(&it.detail, spt, false, rr.x + rr.w - tx - lw - f.px(24.0));
                        f.text(tx + lw + f.px(10.0), ty + (lh - slh) / 2, &d, spt, th.dim, false);
                    }
                }
            }
            self.rows.push((i, rr));
            y += row_h;
        }
        y += f.px(4.0);
        f.cv.beam(x0, y, w - 2 * x0, 1, th.cyan, th.violet, 0.35);
        let info = f.fit(&self.info, spt, false, w - 2 * x0);
        f.text(x0, y + f.px(6.0), &info, spt, if self.info_err { th.crimson } else { th.dim }, self.info_err);
    }
}

impl Drop for Flyout {
    fn drop(&mut self) {
        // only clear the marker if it is still ours
        let f = runtime().join("flyout");
        if std::fs::read_to_string(&f).is_ok_and(|t| t.split_whitespace().nth(1) == Some(&std::process::id().to_string())) {
            let _ = std::fs::remove_file(f);
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let val = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let page = args.first().and_then(|p| PAGES.iter().position(|x| x == p)).unwrap_or(0);
    // one flyout at a time
    let _ = std::fs::create_dir_all(runtime());
    if let Some(pid) = std::fs::read_to_string(runtime().join("flyout")).ok().and_then(|t| t.split_whitespace().nth(1).and_then(|p| p.parse::<i32>().ok())) {
        if pid != std::process::id() as i32 {
            // SAFETY: plain kill(2)
            unsafe { libc::kill(pid, libc::SIGTERM) };
        }
    }
    let _ = std::fs::write(runtime().join("flyout"), format!("{} {}\n", PAGES[page], std::process::id()));
    let mut all: Vec<Entry> = vec![];
    for (id, name, detail, cmd) in [
        ("term", "Terminal", "shadow-term", "shadow-term"),
        ("files", "Files", "shadow-files", "shadow-files"),
        ("net", "Network", "shadow-net", "shadow-net"),
        ("tasks", "Task manager", "shadow-tasks", "shadow-tasks"),
    ] {
        all.push(Entry { id: format!("shadow:{id}"), name: name.into(), detail: detail.into(), tag: "shadow", cmd: cmd.into(), terminal: false, keywords: "shadow".into(), cats: "Shadow".into(), app_ids: vec![] });
    }
    all.extend(desktop::load_apps());
    let mut fl = Flyout {
        page,
        output: val("--output").filter(|o| !o.is_empty()),
        top: val("--top").and_then(|v| v.parse().ok()).unwrap_or(60),
        max_h: val("--max-h").and_then(|v| v.parse().ok()).unwrap_or(1080),
        items: vec![],
        all,
        hist: desktop::load_history(),
        input: Input::default(),
        sel: 0,
        armed: None,
        rows: vec![],
        minus_plus: vec![],
        had_focus: false,
        info: String::new(),
        info_err: false,
    };
    fl.build();
    let r = run(Box::new(fl), None);
    if let Err(e) = r {
        let _ = std::fs::remove_file(runtime().join("flyout"));
        eprintln!("shadow-flyout: {e}");
        std::process::exit(1);
    }
}
