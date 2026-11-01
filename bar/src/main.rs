//! shadow-bar — the floating information bar on the right edge of the screen.
//!
//! Placement: on the laptop panel; when docked (any external monitor on) it follows the monitor
//! that has focus. One bar at a time, reserved space so windows never sit under it.
//! Mod+Shift+B (SIGUSR1) hides/shows it.
//!
//! It never polls niri: focus and workspaces come from niri's event stream, monitors from
//! wl_output events, and system values from small state files every 2 s. It repaints only when
//! something it shows has changed.
#[allow(dead_code)]
mod sys;

use shadow_niri::{self as niri, Change, Model};
use shadow_ui::{calloop::channel, run, Anchor, App, Ctx, Frame, Layer, LayerKeyboard, OutputDesc, Rgb, SurfaceKind};

const MARGIN: i32 = 12;

#[derive(Clone, PartialEq)]
enum Tone {
    Normal,
    Accent,
    Alert,
    Dim,
}

#[derive(Clone, PartialEq)]
struct Row {
    label: String,
    value: String,
    tone: Tone,
    /// a small meter under the value (0..=1), hot = cyan→amber→crimson
    meter: Option<(f32, bool)>,
}

/// The flyout pages behind the bar's buttons.
const PAGES: [(&str, &str); 3] = [("apps", "APPS"), ("settings", "SETTINGS"), ("system", "SYSTEM")];

fn runtime() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into())).join("shadow")
}

/// The flyout that is open now: (page, pid)
fn open_flyout() -> Option<(String, i32)> {
    let t = std::fs::read_to_string(runtime().join("flyout")).ok()?;
    let mut it = t.split_whitespace();
    let page = it.next()?.to_string();
    let pid: i32 = it.next()?.parse().ok()?;
    std::path::Path::new(&format!("/proc/{pid}")).exists().then_some((page, pid))
}

struct Bar {
    width: u32,
    model: Model,
    outs: Vec<niri::Output>,
    rows: Vec<Row>,
    shown: bool,
    on: Option<String>,
    display_mtime: Option<std::time::SystemTime>,
    /// smoothed battery power (W) for a steady time-left estimate
    watts: std::cell::Cell<Option<f64>>,
    /// button hit boxes (device px) and the bar's logical height, from the last draw
    btns: Vec<(&'static str, shadow_ui::widgets::Rect)>,
    logical_h: u32,
    scale: f32,
    flyout: Option<String>,
    /// the POWER dropdown
    power_open: bool,
    power_hits: Vec<(String, shadow_ui::widgets::Rect)>,
    pending: Option<(String, std::time::Instant)>,
    top_apps: Vec<(String, f32)>,
    proc_prev: (std::collections::HashMap<i32, u64>, u64),
}

const MODES: [(&str, &str); 6] = [("auto", "AUTO"), ("performance", "PERF"), ("balanced", "BAL"), ("quiet", "QUIET"), ("cool", "COOL"), ("low-power", "LOW")];

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

/// (busy, total) jiffies of all CPUs and per-process ticks — for the power dropdown's app list
fn proc_cpu() -> (std::collections::HashMap<i32, (String, u64)>, u64) {
    let total: u64 = std::fs::read_to_string("/proc/stat").ok().and_then(|s| s.lines().next().map(|l| l.split_whitespace().skip(1).filter_map(|x| x.parse::<u64>().ok()).sum())).unwrap_or(0);
    let mut m = std::collections::HashMap::new();
    if let Ok(rd) = std::fs::read_dir("/proc") {
        for e in rd.flatten() {
            let Ok(pid) = e.file_name().to_string_lossy().parse::<i32>() else { continue };
            let Ok(st) = std::fs::read_to_string(e.path().join("stat")) else { continue };
            let (Some(a), Some(b)) = (st.find('('), st.rfind(')')) else { continue };
            let f: Vec<&str> = st[b + 2..].split_whitespace().collect();
            let t = f.get(11).and_then(|x| x.parse::<u64>().ok()).unwrap_or(0) + f.get(12).and_then(|x| x.parse::<u64>().ok()).unwrap_or(0);
            m.insert(pid, (st[a + 1..b].to_string(), t));
        }
    }
    (m, total)
}

fn localtime() -> String {
    #[repr(C)]
    struct Tm {
        sec: i32,
        min: i32,
        hour: i32,
        mday: i32,
        mon: i32,
        year: i32,
        wday: i32,
        yday: i32,
        isdst: i32,
        gmtoff: i64,
        zone: *const i8,
    }
    extern "C" {
        fn time(t: *mut i64) -> i64;
        fn localtime_r(t: *const i64, tm: *mut Tm) -> *mut Tm;
    }
    // SAFETY: plain libc calls with valid pointers
    unsafe {
        let t = time(std::ptr::null_mut());
        let mut tm: Tm = std::mem::zeroed();
        if localtime_r(&t, &mut tm).is_null() {
            return String::new();
        }
        const DAYS: [&str; 7] = ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"];
        format!("{} {:02}:{:02}", DAYS[(tm.wday.rem_euclid(7)) as usize], tm.hour, tm.min)
    }
}

impl Bar {
    /// laptop panel when undocked; the focused monitor when docked
    fn target(&self) -> Option<String> {
        let enabled: Vec<&niri::Output> = self.outs.iter().filter(|o| o.enabled()).collect();
        if enabled.is_empty() {
            return None;
        }
        let docked = enabled.iter().any(|o| !o.internal());
        if docked {
            if let Some(f) = self.model.focused_output().filter(|f| enabled.iter().any(|o| o.name == *f)) {
                return Some(f.to_string());
            }
            return enabled.iter().find(|o| !o.internal()).map(|o| o.name.clone());
        }
        enabled.iter().find(|o| o.internal()).or(enabled.first()).map(|o| o.name.clone())
    }

    fn refresh_outputs(&mut self) {
        if let Ok(o) = niri::outputs() {
            self.outs = o;
        }
    }

    fn compute(&self) -> Vec<Row> {
        let mut r = vec![];
        let row = |label: &str, value: String, tone| Row { label: label.to_string(), value, tone, meter: None };
        let on = self.on.clone().or_else(|| self.target()).unwrap_or_default();
        if let Some(o) = self.outs.iter().find(|o| o.name == on) {
            let sc = o.scale.map(|s| format!(" {:.2}×", s).replace(".00×", "×").replace("0×", "×")).unwrap_or_default();
            let n = self.outs.iter().filter(|o| o.enabled()).count();
            let more = if n > 1 { format!(" +{}", n - 1) } else { String::new() };
            r.push(row("MON", format!("{}{}{}", o.name, sc, more), Tone::Normal));
        }
        match self.model.active_ws(&on) {
            Some(ws) => {
                let name = ws.name.clone().unwrap_or_else(|| ws.idx.to_string());
                let n = self.model.windows_on(ws.id);
                r.push(row("WS", format!("{name} · {n} win"), Tone::Normal));
            }
            None => r.push(row("WS", "—".into(), Tone::Dim)),
        }
        match self.model.focused() {
            Some(w) => r.push(row("FOCUS", if w.app_id.is_empty() { w.title.clone() } else { w.app_id.clone() }, Tone::Accent)),
            None => r.push(row("FOCUS", "—".into(), Tone::Dim)),
        }
        let npu = sys::npu_state();
        let tone = if npu == "SEC-ALERT" { Tone::Alert } else { Tone::Accent };
        r.push(row("NPU", npu, tone));
        if let Some(why) = sys::npu_reason() {
            r.push(row("", why.replace('-', " "), Tone::Dim));
        }
        r.push(row("SYNC", sys::sync_state(), Tone::Normal));
        r.push(row("", String::new(), Tone::Dim)); // divider
        let p = sys::power();
        // smooth the draw over ~10 s so the estimate doesn't jump with every spike
        let sm = match (self.watts.get(), p.watts) {
            (Some(o), Some(n)) => Some(o * 0.75 + n * 0.25),
            (_, n) => n,
        };
        self.watts.set(sm);
        let bat = match (p.battery, p.watts) {
            (Some(b), Some(w)) if !p.ac || p.charging => format!("{b}% {}{w:.1} W", if p.charging { "+" } else { "−" }),
            (Some(b), _) => format!("{b}% · AC"),
            (None, _) => "AC".into(),
        };
        let low = !p.ac && p.battery.is_some_and(|b| b < 15);
        let mut br = row("BAT", bat, if low { Tone::Alert } else { Tone::Normal });
        br.meter = p.battery.map(|b| (b as f32 / 100.0, false));
        r.push(br);
        match sys::time_left(&p, sm) {
            Some((m, true)) => r.push(row("", format!("{} to full", sys::fmt_minutes(m)), Tone::Accent)),
            Some((m, false)) => r.push(row("", format!("{} left", sys::fmt_minutes(m)), if m < 30 { Tone::Alert } else { Tone::Accent })),
            None if p.full => r.push(row("", "full".into(), Tone::Dim)),
            None => {}
        }
        if let Some(m) = self.outs.iter().find(|o| o.name == on).and_then(|o| o.mode()) {
            r.push(row("PANEL", format!("{} Hz", (m.mhz as f64 / 1000.0).round()), Tone::Normal));
        }
        let t = sys::package_temp();
        let temp = t.map(|t| format!("{t}°C")).unwrap_or_else(|| "--°C".into());
        let mut pr = row("PKG", format!("{temp} {}", sys::profile()), if t.is_some_and(|t| t >= 90) { Tone::Alert } else { Tone::Normal });
        pr.meter = t.map(|t| (((t - 30) as f32 / 70.0).clamp(0.0, 1.0), true));
        r.push(pr);
        r.push(row("DSP", sys::audio(), Tone::Normal));
        let w = sys::wifi();
        if w.ssid.is_empty() {
            r.push(row("WIFI", "—".into(), Tone::Dim));
        } else {
            r.push(row("WIFI", w.ssid.clone(), Tone::Normal));
            let detail = [w.dbm.map(|d| format!("{d} dBm")), Some(w.eap.clone()).filter(|e| !e.is_empty())].into_iter().flatten().collect::<Vec<_>>().join(" · ");
            if !detail.is_empty() {
                r.push(row("", detail, if w.eap.to_lowercase().contains("fail") { Tone::Alert } else { Tone::Dim }));
            }
        }
        // Lua widgets (shadow.hud) published by shadow-sessiond
        let wf = std::path::PathBuf::from(std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into())).join("shadow/widgets");
        let widgets = std::fs::read_to_string(wf).unwrap_or_default();
        let mut first = true;
        for l in widgets.lines().filter(|l| !l.trim().is_empty()).take(6) {
            if first {
                r.push(row("", String::new(), Tone::Dim));
                first = false;
            }
            let (k, v) = l.split_once(' ').unwrap_or(("", l));
            let label = k.chars().take(5).collect::<String>().to_uppercase();
            r.push(row(&label, v.to_string(), Tone::Normal));
        }
        r.push(row("", String::new(), Tone::Dim));
        r.push(row("TIME", localtime(), Tone::Normal));
        r
    }

    fn update(&mut self, ctx: &mut Ctx) {
        let rows = self.compute();
        if rows != self.rows {
            self.rows = rows;
            ctx.redraw();
        }
    }

    fn place(&mut self, ctx: &mut Ctx) {
        let t = self.target();
        if t != self.on {
            self.on = t.clone();
            ctx.move_to_output(t);
            ctx.redraw();
        }
    }
}

impl Bar {
    fn power_act(&mut self, act: &str, ctx: &mut Ctx) {
        let (kind, val) = act.split_once(':').unwrap_or((act, ""));
        let args: Vec<String> = match kind {
            "toggle" => {
                self.power_open = !self.power_open;
                if self.power_open {
                    self.sample_apps();
                }
                ctx.redraw();
                return;
            }
            "mode" => vec!["mode".into(), val.into(), "keep".into()],
            "charge" if val == "custom" => vec!["charge".into(), "custom".into(), "60".into(), "80".into()],
            "charge" => vec!["charge".into(), val.into()],
            "app" => {
                let _ = std::process::Command::new("shadow-tasks").stdin(std::process::Stdio::null()).spawn();
                return;
            }
            _ => return,
        };
        // the power daemon validates and rate-limits (charging writes go to the firmware)
        let _ = std::process::Command::new("shadow-powerd").args(&args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
        self.pending = Some((act.to_string(), std::time::Instant::now()));
        ctx.redraw();
    }

    fn sample_apps(&mut self) {
        let (m, total) = proc_cpu();
        let dt = total.saturating_sub(self.proc_prev.1).max(1) as f32;
        let mut v: Vec<(String, f32)> = vec![];
        let mut ticks = std::collections::HashMap::new();
        for (pid, (name, t)) in &m {
            if let Some(old) = self.proc_prev.0.get(pid) {
                let share = t.saturating_sub(*old) as f32 / dt;
                if share > 0.002 {
                    v.push((name.clone(), share));
                }
            }
            ticks.insert(*pid, *t);
        }
        // one line per program name
        let mut by: std::collections::HashMap<String, f32> = std::collections::HashMap::new();
        for (n, s) in v {
            *by.entry(n).or_default() += s;
        }
        let mut v: Vec<(String, f32)> = by.into_iter().collect();
        v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        v.truncate(3);
        self.top_apps = v;
        self.proc_prev = (ticks, total);
    }
}

impl App for Bar {
    fn kind(&self) -> SurfaceKind {
        SurfaceKind::Layer {
            namespace: "shadow-bar".into(),
            layer: Layer::Top,
            anchor: Anchor::RIGHT,
            size: (self.width, 400),
            margin: (0, MARGIN, 0, 0),
            exclusive: self.width as i32 + MARGIN,
            keyboard: LayerKeyboard::None,
        }
    }

    fn pick_output(&mut self, _outs: &[OutputDesc]) -> Option<String> {
        self.refresh_outputs();
        self.on = self.target();
        self.rows = self.compute();
        self.on.clone()
    }

    fn tick_ms(&self) -> u64 {
        2000
    }

    fn tick(&mut self, ctx: &mut Ctx) {
        // shadow-displayd rewrites display.state whenever it changes a scale or mode
        let f = std::path::PathBuf::from(std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into())).join("shadow/display.state");
        let mt = std::fs::metadata(f).and_then(|m| m.modified()).ok();
        if mt != self.display_mtime {
            self.display_mtime = mt;
            self.refresh_outputs();
            self.place(ctx);
        }
        if self.power_open {
            self.sample_apps();
            ctx.redraw();
        }
        if self.pending.as_ref().is_some_and(|p| p.1.elapsed().as_secs() > 6) {
            self.pending = None;
        }
        let fl = open_flyout().map(|f| f.0);
        if fl != self.flyout {
            self.flyout = fl;
            ctx.redraw();
        }
        self.update(ctx);
    }

    /// Buttons: open the page's flyout beside the button; the same button again closes it.
    fn click(&mut self, x: i32, y: i32, ctx: &mut Ctx) {
        if let Some((act, _)) = self.power_hits.iter().find(|(_, r)| r.hit(x, y)).cloned() {
            self.power_act(&act, ctx);
            return;
        }
        let Some(&(page, r)) = self.btns.iter().find(|(_, r)| r.hit(x, y)) else { return };
        let open = open_flyout();
        if let Some((_, pid)) = &open {
            // SAFETY: plain kill(2) on a pid we read from our own state file
            unsafe { libc::kill(*pid, libc::SIGTERM) };
            let _ = std::fs::remove_file(runtime().join("flyout"));
        }
        if open.as_ref().is_some_and(|(p, _)| p == page) {
            self.flyout = None;
            ctx.redraw();
            return;
        }
        let on = self.on.clone().or_else(|| self.target()).unwrap_or_default();
        let out_h = self.outs.iter().find(|o| o.name == on).and_then(|o| o.logical).map(|l| l.3).unwrap_or(1080);
        // the bar is centred vertically: its top on the output, then the button's offset
        let bar_top = (out_h - self.logical_h as i32).max(0) / 2;
        let top = bar_top + (r.y as f32 / self.scale).round() as i32;
        let _ = std::process::Command::new("shadow-flyout")
            .args([page, "--output", &on, "--top", &top.to_string(), "--max-h", &out_h.to_string()])
            .stdin(std::process::Stdio::null())
            .spawn();
        self.flyout = Some(page.to_string());
        ctx.redraw();
    }

    fn outputs_changed(&mut self, _outs: &[OutputDesc], ctx: &mut Ctx) {
        self.refresh_outputs();
        self.place(ctx);
        self.update(ctx);
    }

    fn message(&mut self, m: String, ctx: &mut Ctx) {
        if m == "toggle" {
            self.shown = !self.shown;
            ctx.set_visible(self.shown);
            if self.shown {
                self.on = None;
                self.place(ctx);
            }
            return;
        }
        let changes = self.model.apply(&m);
        if changes.iter().any(|c| matches!(c, Change::Workspace)) {
            self.place(ctx);
        }
        if !changes.is_empty() || m.contains("WindowFocusChanged") || m.contains("WindowsChanged") {
            self.update(ctx);
        }
    }

    fn draw(&mut self, f: &mut Frame) {
        let th = f.theme.clone();
        let w = f.cv.w as i32;
        let pt = th.font_pt * 0.92;
        let lpt = th.font_pt * 0.74;
        let lh = f.line_h(pt);
        let pad = f.px(14.0);
        let hair = f.hair();
        let label_w = f.text_w("FOCUS ", lpt, false);
        let head_h = lh + f.px(10.0);
        let btn_h = f.line_h(lpt) + f.px(12.0);
        let mh = f.px(5.0);
        let rows_h: i32 = self.rows.iter().map(|r| if r.label.is_empty() && r.value.is_empty() { f.px(13.0) } else { lh + r.meter.map_or(0, |_| mh + f.px(4.0)) }).sum();
        let chip_h = f.line_h(lpt) + f.px(10.0);
        let power_h = btn_h + f.px(8.0)
            + if self.power_open {
                let charge_n = sys::kv("run/shadow/power.state", "charge_avail").map(|a| a.split_whitespace().count()).unwrap_or(0) as i32;
                let lab = f.line_h(lpt) + f.px(6.0);
                lab + 3 * (chip_h + f.px(5.0))
                    + if charge_n > 0 { lab + ((charge_n + 1) / 2) * (chip_h + f.px(5.0)) } else { lab + f.line_h(lpt) }
                    + lab + 3.max(self.top_apps.len() as i32) * (f.line_h(lpt) + f.px(3.0))
                    + f.px(8.0)
            } else {
                0
            };
        let need = pad + head_h + f.px(10.0) + btn_h + f.px(14.0) + power_h + rows_h + pad;
        let need_logical = (need as f32 / f.scale).ceil() as u32;
        if need_logical != f.logical.1 {
            f.want_size = Some((self.width, need_logical));
        }
        self.logical_h = f.logical.1;
        self.scale = f.scale;
        let h = f.cv.h as i32;
        let rad = f.px(th.radius + 4.0) as f32;
        let edge = f.px(4.0);
        f.cv.clear((0, 0, 0), 0.0);
        // floating glass card
        f.cv.glow(edge, edge, w - 2 * edge, h - 2 * edge, rad, edge, th.violet, 0.16);
        f.cv.rrect_v(edge, edge, w - 2 * edge, h - 2 * edge, rad, th.glass, th.bg, th.bg_alpha);
        f.cv.scanlines(edge, edge + f.px(8.0), w - 2 * edge, h - 2 * edge - f.px(16.0), f.px(4.0).max(3), th.cyan, 0.02);
        f.cv.rstroke(edge, edge, w - 2 * edge, h - 2 * edge, rad, hair, th.amber, 0.45);
        // header tag
        let tag = "  SHADOW  ";
        let tw = f.text_w(tag, pt, true) + f.px(12.0);
        f.cv.glow(pad, pad, tw - f.px(8.0), head_h - f.px(4.0), f.px(3.0) as f32, f.px(5.0), th.amber, 0.2);
        f.cv.tag(pad, pad, tw, head_h - f.px(4.0), th.amber, th.violet, 1.0);
        f.text(pad, pad + f.px(3.0), tag, pt, th.bg, true);
        let sub = f.fit("RESTRICTED", lpt, false, w - 2 * pad - tw - f.px(4.0));
        f.text(pad + tw + f.px(4.0), pad + f.px(5.0), &sub, lpt, th.cyan, false);
        let mut y = pad + head_h + f.px(8.0);
        // buttons: apps · settings · system (the open one glows)
        let gap = f.px(6.0);
        let bw = (w - 2 * pad - 2 * gap) / 3;
        self.btns.clear();
        for (i, (page, label)) in PAGES.iter().enumerate() {
            let bx = pad + i as i32 * (bw + gap);
            let r = shadow_ui::widgets::Rect { x: bx, y, w: bw, h: btn_h };
            let on = self.flyout.as_deref() == Some(*page);
            let br = btn_h as f32 / 2.0;
            if on {
                f.cv.glow(bx, y, bw, btn_h, br, f.px(5.0), th.amber, 0.35);
                f.cv.rrect_h(bx, y, bw, btn_h, br, th.amber, th.violet, 1.0);
            } else {
                f.cv.rrect_v(bx, y, bw, btn_h, br, th.glass, th.glass_lo, 1.0);
                f.cv.rstroke(bx, y, bw, btn_h, br, hair, th.cyan, 0.45);
            }
            let bpt = lpt * 0.9;
            let l = f.fit(label, bpt, true, bw - f.px(4.0));
            let lw = f.text_w(&l, bpt, true);
            let ly = y + (btn_h - f.line_h(bpt)) / 2;
            f.text(bx + (bw - lw) / 2, ly, &l, bpt, if on { th.bg } else { th.text }, true);
            self.btns.push((page, r));
        }
        y += btn_h + f.px(8.0);
        // POWER dropdown: mode · charging · the apps drawing the most power
        self.power_hits.clear();
        let mode = sys::kv("run/shadow/power.state", "mode").unwrap_or_else(|| "—".into());
        let charge = sys::kv("run/shadow/power.state", "charge_mode").filter(|c| c != "-").map(|c| charge_label(&c).to_string()).or_else(|| sys::kv("run/shadow/power.state", "charge").filter(|c| !c.is_empty())).unwrap_or_else(|| "—".into());
        let full_once = sys::kv("run/shadow/power.state", "full_once").as_deref() == Some("1");
        {
            let r = shadow_ui::widgets::Rect { x: pad, y, w: w - 2 * pad, h: btn_h };
            let rr = btn_h as f32 / 2.0;
            f.cv.rrect_h(r.x, r.y, r.w, r.h, rr, th.glass, shadow_ui::canvas::lerp(th.glass, th.violet, 0.25), 1.0);
            f.cv.rstroke(r.x, r.y, r.w, r.h, rr, hair, if self.power_open { th.amber } else { th.violet }, 0.6);
            let label = format!("POWER {}  {} · {}", if self.power_open { "▴" } else { "▾" }, mode, if full_once { "100 % once".into() } else { charge });
            let l = f.fit(&label, lpt, true, r.w - f.px(16.0));
            let ty = r.y + (btn_h - f.line_h(lpt)) / 2;
            f.text(r.x + f.px(10.0), ty, &l, lpt, th.text, true);
            self.power_hits.push(("toggle".into(), r));
            y += btn_h + f.px(8.0);
        }
        if self.power_open {
            let lab = f.line_h(lpt) + f.px(6.0);
            let pending = self.pending.as_ref().map(|p| p.0.clone()).unwrap_or_default();
            let chip = |f: &mut Frame, x: i32, y: i32, cw: i32, label: &str, on: bool, busy: bool, act: String, hits: &mut Vec<(String, shadow_ui::widgets::Rect)>| {
                let r = shadow_ui::widgets::Rect { x, y, w: cw, h: chip_h };
                let rr = chip_h as f32 / 2.0;
                if on {
                    f.cv.glow(x, y, cw, chip_h, rr, f.px(4.0), th.amber, 0.3);
                    f.cv.rrect_h(x, y, cw, chip_h, rr, th.amber, th.violet, 1.0);
                } else {
                    f.cv.rrect(x, y, cw, chip_h, rr, th.glass_lo, 1.0);
                    f.cv.rstroke(x, y, cw, chip_h, rr, hair, if busy { th.amber } else { th.cyan }, if busy { 0.9 } else { 0.35 });
                }
                let bpt = lpt * 0.92;
                let l = f.fit(label, bpt, on, cw - f.px(8.0));
                let lw = f.text_w(&l, bpt, on);
                let ty = y + (chip_h - f.line_h(bpt)) / 2;
                f.text(x + (cw - lw) / 2, ty, &l, bpt, if on { th.bg } else { th.text }, on);
                hits.push((act, r));
            };
            let gap = f.px(5.0);
            let cw2 = (w - 2 * pad - gap) / 2;
            // modes: 2 columns
            f.text(pad, y, "MODE", lpt, th.dim, false);
            y += lab;
            let mut hits = vec![];
            for (i, (m, l)) in MODES.iter().enumerate() {
                let (cx, cy) = (pad + (i as i32 % 2) * (cw2 + gap), y + (i as i32 / 2) * (chip_h + gap));
                chip(f, cx, cy, cw2, l, mode == *m, pending == format!("mode:{m}"), format!("mode:{m}"), &mut hits);
            }
            y += 3 * (chip_h + gap);
            // charging: only what the firmware offers (+ smart / 100 % once)
            f.text(pad, y, "CHARGING", lpt, th.dim, false);
            y += lab;
            let avail: Vec<String> = sys::kv("run/shadow/power.state", "charge_avail").unwrap_or_default().split_whitespace().map(String::from).collect();
            if avail.is_empty() {
                f.text(pad, y, "not offered by this firmware", lpt, th.dim, false);
                y += f.line_h(lpt);
            } else {
                let chosen = sys::kv("run/shadow/power.state", "charge_mode").unwrap_or_default();
                for (i, m) in avail.iter().enumerate() {
                    let (cx, cy) = (pad + (i as i32 % 2) * (cw2 + gap), y + (i as i32 / 2) * (chip_h + gap));
                    let on = if m == "full" { full_once } else { chosen == *m && !full_once };
                    chip(f, cx, cy, cw2, charge_label(m), on, pending == format!("charge:{m}"), format!("charge:{m}"), &mut hits);
                }
                y += ((avail.len() as i32 + 1) / 2) * (chip_h + gap);
            }
            // apps
            f.text(pad, y, "APPS · power now", lpt, th.dim, false);
            y += lab;
            let pkg = sys::kv("run/shadow/rapl.state", "pkg_w").and_then(|v| v.parse::<f32>().ok());
            for (name, share) in self.top_apps.clone() {
                let est = pkg.map(|p| format!("~{:.1} W", p * share)).unwrap_or_else(|| format!("{:.0} % CPU", share * 100.0));
                let ew = f.text_w(&est, lpt, false);
                let n = f.fit(&name, lpt, false, w - 2 * pad - ew - f.px(10.0));
                f.text(pad + f.px(4.0), y, &n, lpt, th.text, false);
                f.text(w - pad - ew, y, &est, lpt, th.cyan, false);
                hits.push(("app:".into(), shadow_ui::widgets::Rect { x: pad, y, w: w - 2 * pad, h: f.line_h(lpt) }));
                y += f.line_h(lpt) + f.px(3.0);
            }
            if self.top_apps.is_empty() {
                f.text(pad + f.px(4.0), y, "sampling…", lpt, th.dim, false);
            }
            y += (3 - self.top_apps.len().min(3) as i32) * (f.line_h(lpt) + f.px(3.0)) + f.px(8.0);
            self.power_hits.extend(hits);
        }
        f.cv.beam(pad, y, w - 2 * pad, hair, th.amber, th.cyan, 0.7);
        y += f.px(6.0);
        let vx = pad + label_w;
        let vmax = w - vx - pad;
        for r in &self.rows {
            if r.label.is_empty() && r.value.is_empty() {
                f.cv.beam(pad, y + f.px(6.0), w - 2 * pad, hair, th.cyan, th.violet, 0.35);
                y += f.px(13.0);
                continue;
            }
            if !r.label.is_empty() {
                let llh = f.line_h(lpt);
                f.text(pad, y + (lh - llh) / 2, &r.label, lpt, th.dim, false);
            }
            let col: Rgb = match r.tone {
                Tone::Normal => th.text,
                Tone::Accent => th.cyan,
                Tone::Alert => th.crimson,
                Tone::Dim => th.dim,
            };
            let v = f.fit(&r.value, pt, false, vmax);
            f.text(vx, y, &v, pt, col, r.tone == Tone::Alert);
            y += lh;
            if let Some((m, hot)) = r.meter {
                shadow_ui::widgets::meter(f, vx, y + f.px(1.0), vmax, mh, m, hot);
                y += mh + f.px(4.0);
            }
        }
    }
}

/// SIGUSR1 -> "toggle" on the channel (blocked in every thread, received by sigwait)
fn signal_thread(tx: channel::Sender<String>) {
    // SAFETY: standard signal-mask setup before other threads exist
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGUSR1);
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        std::thread::spawn(move || loop {
            let mut sig = 0;
            if libc::sigwait(&set, &mut sig) == 0 && sig == libc::SIGUSR1 && tx.send("toggle".into()).is_err() {
                return;
            }
        });
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("shadow-bar — Shadow TUI Restricted side bar\n  started by niri (spawn-at-startup); Mod+Shift+B or `pkill -USR1 -x shadow-bar` hides/shows it\n  config: [bar] width = 250 in ~/.config/shadow-state/config.toml");
        return;
    }
    let width = shadow_ui::theme::config().get("bar").and_then(|b| b.get("width")).and_then(|w| w.as_integer()).map(|w| w.clamp(180, 480) as u32).unwrap_or(250);
    let (tx, rx) = channel::channel::<String>();
    signal_thread(tx.clone());
    if niri::available() {
        std::thread::spawn(move || {
            niri::event_stream(|line| line.is_empty() || tx.send(line.to_string()).is_ok());
        });
    }
    let bar = Bar { width, model: Model::default(), outs: vec![], rows: vec![], shown: true, on: None, display_mtime: None, watts: std::cell::Cell::new(None), btns: vec![], logical_h: 0, scale: 1.0, flyout: open_flyout().map(|f| f.0), power_open: std::env::var_os("SHADOW_BAR_POWER_OPEN").is_some(), power_hits: vec![], pending: None, top_apps: vec![], proc_prev: (Default::default(), 0) };
    if let Err(e) = run(Box::new(bar), Some(rx)) {
        eprintln!("shadow-bar: {e}");
        std::process::exit(1);
    }
}
