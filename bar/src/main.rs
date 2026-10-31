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
use shadow_ui::{calloop::channel, run, Anchor, App, Ctx, Frame, Layer, OutputDesc, Rgb, SurfaceKind};

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
}

struct Bar {
    width: u32,
    model: Model,
    outs: Vec<niri::Output>,
    rows: Vec<Row>,
    shown: bool,
    on: Option<String>,
    display_mtime: Option<std::time::SystemTime>,
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
        let row = |label: &str, value: String, tone| Row { label: label.to_string(), value, tone };
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
        let bat = match (p.battery, p.watts) {
            (Some(b), Some(w)) if !p.ac || p.charging => format!("{b}% {}{w:.1} W", if p.charging { "+" } else { "−" }),
            (Some(b), _) => format!("{b}% · AC"),
            (None, _) => "AC".into(),
        };
        let low = !p.ac && p.battery.is_some_and(|b| b < 15);
        r.push(row("BAT", bat, if low { Tone::Alert } else { Tone::Normal }));
        if let Some(m) = self.outs.iter().find(|o| o.name == on).and_then(|o| o.mode()) {
            r.push(row("PANEL", format!("{} Hz", (m.mhz as f64 / 1000.0).round()), Tone::Normal));
        }
        let t = sys::package_temp();
        let temp = t.map(|t| format!("{t}°C")).unwrap_or_else(|| "--°C".into());
        r.push(row("PKG", format!("{temp} {}", sys::profile()), if t.is_some_and(|t| t >= 90) { Tone::Alert } else { Tone::Normal }));
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

impl App for Bar {
    fn kind(&self) -> SurfaceKind {
        SurfaceKind::Layer {
            namespace: "shadow-bar".into(),
            layer: Layer::Top,
            anchor: Anchor::RIGHT,
            size: (self.width, 400),
            margin: (0, MARGIN, 0, 0),
            exclusive: self.width as i32 + MARGIN,
            keyboard: false,
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
        self.update(ctx);
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
        let (w, _) = (f.cv.w as i32, f.cv.h as i32);
        let pt = th.font_pt * 0.92;
        let lpt = th.font_pt * 0.78;
        let lh = f.line_h(pt);
        let pad = f.px(12.0);
        let hair = f.hair();
        let label_w = f.text_w("FOCUS ", lpt, false);
        // height needed for the content, in device px
        let head_h = lh + f.px(10.0);
        let rows_h: i32 = self.rows.iter().map(|r| if r.label.is_empty() && r.value.is_empty() { f.px(9.0) } else { lh }).sum();
        let need = pad + head_h + f.px(8.0) + rows_h + pad;
        let need_logical = (need as f32 / f.scale).ceil() as u32;
        if need_logical != f.logical.1 {
            f.want_size = Some((self.width, need_logical));
        }
        let h = f.cv.h as i32;
        f.cv.clear((0, 0, 0), 0.0);
        f.cv.fill(0, 0, w, h, th.bg, th.bg_alpha);
        f.cv.border(0, 0, w, h, hair, th.amber, 0.55);
        f.cv.brackets(0, 0, w, h, f.px(12.0), f.px(2.0), th.amber, 0.95);
        // header
        let tag = " ◢ SHADOW ";
        let tw = f.text_w(tag, pt, true);
        f.cv.fill(pad, pad, tw, head_h - f.px(4.0), th.amber, 1.0);
        f.text(pad, pad + f.px(3.0), tag, pt, th.bg, true);
        f.text(pad + tw + f.px(6.0), pad + f.px(4.0), "RESTRICTED", lpt, th.amber, false);
        let mut y = pad + head_h + f.px(4.0);
        f.cv.fill(pad, y, w - 2 * pad, hair, th.amber, 0.6);
        y += f.px(4.0);
        let vx = pad + label_w;
        let vmax = w - vx - pad;
        for r in &self.rows {
            if r.label.is_empty() && r.value.is_empty() {
                f.cv.fill(pad, y + f.px(4.0), w - 2 * pad, hair, th.amber, 0.25);
                y += f.px(9.0);
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
        println!("shadow-bar — Shadow TUI Restricted side bar\n  started by niri (spawn-at-startup); Mod+Shift+B or `pkill -USR1 -x shadow-bar` hides/shows it\n  config: [bar] width = 230 in ~/.config/shadow-state/config.toml");
        return;
    }
    let width = shadow_ui::theme::config().get("bar").and_then(|b| b.get("width")).and_then(|w| w.as_integer()).map(|w| w.clamp(160, 480) as u32).unwrap_or(230);
    let (tx, rx) = channel::channel::<String>();
    signal_thread(tx.clone());
    if niri::available() {
        std::thread::spawn(move || {
            niri::event_stream(|line| line.is_empty() || tx.send(line.to_string()).is_ok());
        });
    }
    let bar = Bar { width, model: Model::default(), outs: vec![], rows: vec![], shown: true, on: None, display_mtime: None };
    if let Err(e) = run(Box::new(bar), Some(rx)) {
        eprintln!("shadow-bar: {e}");
        std::process::exit(1);
    }
}
