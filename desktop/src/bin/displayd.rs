//! shadow-displayd — monitors, scale, text size, refresh rate, docking and idle, on niri.
//!
//! * Every monitor gets the scale that suits its real pixel density as soon as it appears
//!   (laptop panel and external monitors alike); a scale pinned in config.toml [display.scale]
//!   always wins. If niri ends up on a different scale than intended, the text size is corrected
//!   instead: shadow apps do it themselves, GTK apps get text-scaling-factor.
//! * The laptop panel switches 120 Hz on AC / 60 Hz on battery (spec §3.1), following
//!   shadow-powerd's request when it makes one.
//! * Dock mode: closing the lid with an external monitor on turns the panel off instead of
//!   locking; opening it turns the panel back on.
//! * Idle: locks after [session] lock_after_min, turns monitors off after screen_off_after_min.
//!
//! Event-driven: wl_output events, ext-idle-notify, a 5 s check of the power source.
//!
//!   shadow-displayd                run (started by niri)
//!   shadow-displayd status         what it decided for each monitor
//!   shadow-displayd apply          apply once and exit
//!   shadow-displayd lid-close|lid-open   (niri switch-events)
use shadow_desktop::{runtime_dir, spawn, sys};
use shadow_niri::{self as niri, Output};
use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay_client_toolkit::reexports::calloop::EventLoop;
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::client::{
    globals::registry_queue_init,
    protocol::{wl_output, wl_seat},
    Connection, Dispatch, Proxy, QueueHandle,
};
use smithay_client_toolkit::{
    delegate_registry,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{Capability, SeatHandler, SeatState},
};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};
use wayland_protocols::ext::idle_notify::v1::client::{
    ext_idle_notification_v1::{self, ExtIdleNotificationV1},
    ext_idle_notifier_v1::ExtIdleNotifierV1,
};

#[derive(Clone, Debug, PartialEq)]
struct Cfg {
    pinned: BTreeMap<String, f64>,
    policy: String,
    hz_ac: f64,
    hz_battery: f64,
    lock_min: u32,
    off_min: u32,
}

fn cfg() -> Cfg {
    let c = shadow_ui::theme::config();
    let d = c.get("display").and_then(|d| d.as_table());
    let num = |t: Option<&shadow_ui::theme::TomlTable>, k: &str, def: f64| t.and_then(|t| t.get(k)).and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64))).unwrap_or(def);
    let mut pinned = BTreeMap::new();
    if let Some(s) = d.and_then(|d| d.get("scale")).and_then(|s| s.as_table()) {
        for (k, v) in s {
            if let Some(f) = v.as_float().or_else(|| v.as_integer().map(|i| i as f64)) {
                pinned.insert(k.clone(), f.clamp(0.5, 4.0));
            }
        }
    }
    let s = c.get("session").and_then(|d| d.as_table());
    Cfg {
        pinned,
        policy: d.and_then(|d| d.get("refresh_policy")).and_then(|v| v.as_str()).unwrap_or("auto").to_string(),
        hz_ac: num(d, "refresh_ac", 120.0),
        hz_battery: num(d, "refresh_battery", 60.0),
        lock_min: num(s, "lock_after_min", 10.0).max(0.0) as u32,
        off_min: num(s, "screen_off_after_min", 12.0).max(0.0) as u32,
    }
}

/// fake EDID sizes for testing nested sessions: SHADOW_FAKE_MM="winit=301x188"
fn mm_of(o: &Output) -> Option<(i32, i32)> {
    if let Ok(f) = std::env::var("SHADOW_FAKE_MM") {
        for part in f.split(',') {
            if let Some((n, s)) = part.split_once('=') {
                if n == o.name {
                    let (w, h) = s.split_once('x')?;
                    return Some((w.parse().ok()?, h.parse().ok()?));
                }
            }
        }
    }
    o.mm
}

fn ideal(o: &Output) -> Option<f64> {
    let m = o.mode()?;
    shadow_ui::scale::ideal_scale(&o.name, (m.w, m.h), mm_of(o)?)
}

fn wanted_hz(c: &Cfg, ac: bool) -> Option<f64> {
    match c.policy.as_str() {
        "fixed" | "off" => None,
        "ac" => Some(c.hz_ac),
        "battery" => Some(c.hz_battery),
        _ => Some(sys::wanted_refresh().map(|r| r as f64).unwrap_or(if ac { c.hz_ac } else { c.hz_battery })),
    }
}

/// Apply scale + refresh to every enabled monitor. Returns the state report.
fn reconcile(c: &Cfg) -> String {
    let Ok(outs) = niri::outputs() else { return "niri not reachable\n".into() };
    let ac = sys::power().ac;
    let mut report = String::new();
    for o in outs.iter().filter(|o| o.enabled()) {
        let want_scale = c.pinned.get(&o.name).copied().or_else(|| ideal(o));
        if let (Some(t), Some(cur)) = (want_scale, o.scale) {
            if (t - cur).abs() > 0.005 {
                if let Err(e) = niri::msg(&["output", &o.name, "scale", &format!("{t}")]) {
                    eprintln!("shadow-displayd: {}: scale {t}: {e}", o.name);
                }
            }
        }
        if o.internal() {
            if let (Some(hz), Some(cur)) = (wanted_hz(c, ac), o.mode()) {
                let best = o.modes.iter().filter(|m| m.w == cur.w && m.h == cur.h).min_by_key(|m| ((m.mhz as f64 - hz * 1000.0).abs() * 10.0) as i64);
                if let Some(b) = best {
                    if b.mhz != cur.mhz {
                        let mode = format!("{}x{}@{:.3}", b.w, b.h, b.mhz as f64 / 1000.0);
                        if let Err(e) = niri::msg(&["output", &o.name, "mode", &mode]) {
                            eprintln!("shadow-displayd: {}: mode {mode}: {e}", o.name);
                        }
                    }
                }
            }
        }
    }
    // read back what niri actually did; correct text size where the scale isn't the ideal one
    let after = niri::outputs().unwrap_or(outs);
    let focused = niri::focused_output();
    let docked = after.iter().any(|o| o.enabled() && !o.internal());
    let mut gtk_factor = 1.0;
    for o in &after {
        let id = ideal(o);
        let corr = o.scale.map(|s| shadow_ui::scale::text_correction(s, id)).unwrap_or(1.0);
        let m = o.mode().map(|m| format!("{}x{}@{:.0}", m.w, m.h, m.mhz as f64 / 1000.0)).unwrap_or_default();
        report.push_str(&format!(
            "{} enabled={} scale={} ideal={} pinned={} text={:.2} mode={} internal={}\n",
            o.name,
            o.enabled() as u8,
            o.scale.map(|s| format!("{s}")).unwrap_or("-".into()),
            id.map(|s| format!("{s}")).unwrap_or("unknown".into()),
            c.pinned.contains_key(&o.name) as u8,
            corr,
            m,
            o.internal() as u8
        ));
        if focused.as_deref() == Some(o.name.as_str()) {
            gtk_factor = corr;
        }
    }
    report.push_str(&format!("docked={} ac={} policy={}\n", docked as u8, ac as u8, c.policy));
    gtk_text_factor(gtk_factor);
    report
}

fn gtk_text_factor(f: f64) {
    if which("gsettings").is_none() {
        return;
    }
    let cur = std::process::Command::new("gsettings").args(["get", "org.gnome.desktop.interface", "text-scaling-factor"]).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).and_then(|s| s.parse::<f64>().ok());
    if cur.map_or(true, |c| (c - f).abs() > 0.01) {
        let _ = std::process::Command::new("gsettings").args(["set", "org.gnome.desktop.interface", "text-scaling-factor", &format!("{f:.2}")]).status();
    }
}

fn which(p: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH").and_then(|ps| std::env::split_paths(&ps).map(|d| d.join(p)).find(|f| f.is_file()))
}

fn write_state(report: &str) {
    let _ = std::fs::write(runtime_dir().join("display.state"), report);
}

fn lid(close: bool) -> i32 {
    let Ok(outs) = niri::outputs() else { return 1 };
    let internal: Vec<&Output> = outs.iter().filter(|o| o.internal()).collect();
    let external_on = outs.iter().any(|o| !o.internal() && o.enabled());
    if close {
        if external_on {
            for o in &internal {
                let _ = niri::msg(&["output", &o.name, "off"]);
            }
        } else {
            spawn("shadow-lock");
        }
    } else {
        for o in &internal {
            let _ = niri::msg(&["output", &o.name, "on"]);
        }
    }
    let _ = std::process::Command::new("shadow-displayd").arg("apply").status();
    0
}

// ====================================================================== daemon
struct D {
    registry: RegistryState,
    output_state: OutputState,
    seat_state: SeatState,
    notifier: Option<ExtIdleNotifierV1>,
    notes: Vec<ExtIdleNotificationV1>,
    seat: Option<wl_seat::WlSeat>,
    cfg: Cfg,
    dirty_at: Option<Instant>,
    last_ac: Option<bool>,
    last_wish: Option<u32>,
    cfg_mtime: Option<std::time::SystemTime>,
}

#[derive(Clone, Copy)]
struct IdleKind(u8); // 0 = lock, 1 = monitors off

impl D {
    fn arm_idle(&mut self, qh: &QueueHandle<D>) {
        for n in self.notes.drain(..) {
            n.destroy();
        }
        let (Some(nf), Some(seat)) = (&self.notifier, &self.seat) else { return };
        if self.cfg.lock_min > 0 {
            self.notes.push(nf.get_idle_notification(self.cfg.lock_min * 60_000, seat, qh, IdleKind(0)));
        }
        if self.cfg.off_min > 0 {
            self.notes.push(nf.get_idle_notification(self.cfg.off_min * 60_000, seat, qh, IdleKind(1)));
        }
    }
}

fn cfg_mtime() -> Option<std::time::SystemTime> {
    std::fs::metadata(shadow_ui::theme::state_dir().join("config.toml")).and_then(|m| m.modified()).ok()
}

fn daemon() -> Result<(), String> {
    let conn = Connection::connect_to_env().map_err(|e| format!("wayland: {e}"))?;
    let (globals, queue) = registry_queue_init::<D>(&conn).map_err(|e| e.to_string())?;
    let qh = queue.handle();
    let mut ev: EventLoop<D> = EventLoop::try_new().map_err(|e| e.to_string())?;
    let mut d = D {
        registry: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        seat_state: SeatState::new(&globals, &qh),
        notifier: globals.bind::<ExtIdleNotifierV1, _, _>(&qh, 1..=1, ()).ok(),
        notes: vec![],
        seat: None,
        cfg: cfg(),
        dirty_at: Some(Instant::now()),
        last_ac: None,
        last_wish: None,
        cfg_mtime: cfg_mtime(),
    };
    WaylandSource::new(conn.clone(), queue).insert(ev.handle()).map_err(|e| e.to_string())?;
    ev.handle()
        .insert_source(Timer::from_duration(Duration::from_millis(500)), |_, _, d: &mut D| {
            // power source / powerd wish / config edits -> reconcile
            let ac = sys::power().ac;
            let wish = sys::wanted_refresh();
            let mt = cfg_mtime();
            if Some(ac) != d.last_ac || wish != d.last_wish || mt != d.cfg_mtime {
                d.last_ac = Some(ac);
                d.last_wish = wish;
                if mt != d.cfg_mtime {
                    d.cfg_mtime = mt;
                    d.cfg = cfg();
                }
                d.dirty_at.get_or_insert_with(Instant::now);
            }
            if d.dirty_at.is_some_and(|t| t.elapsed() >= Duration::from_millis(400)) {
                d.dirty_at = None;
                write_state(&reconcile(&d.cfg));
            }
            TimeoutAction::ToDuration(Duration::from_millis(if d.dirty_at.is_some() { 450 } else { 5000 }))
        })
        .map_err(|e| e.to_string())?;
    let qh2 = qh.clone();
    let mut armed_cfg = None;
    loop {
        ev.dispatch(None, &mut d).map_err(|e| e.to_string())?;
        if armed_cfg.as_ref() != Some(&(d.cfg.lock_min, d.cfg.off_min, d.seat.is_some())) {
            armed_cfg = Some((d.cfg.lock_min, d.cfg.off_min, d.seat.is_some()));
            d.arm_idle(&qh2);
        }
        let _ = conn.flush();
    }
}

impl OutputHandler for D {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.dirty_at.get_or_insert_with(Instant::now);
    }
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.dirty_at.get_or_insert_with(Instant::now);
    }
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.dirty_at.get_or_insert_with(Instant::now);
    }
}

impl SeatHandler for D {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        if self.seat.is_none() {
            self.seat = Some(seat);
        }
    }
    fn new_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, _: Capability) {}
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, _: Capability) {}
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

delegate_registry!(D);
impl ProvidesRegistryState for D {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}
smithay_client_toolkit::delegate_dispatch2!(D);

impl Dispatch<ExtIdleNotifierV1, ()> for D {
    fn event(_: &mut Self, _: &ExtIdleNotifierV1, _: <ExtIdleNotifierV1 as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<ExtIdleNotificationV1, IdleKind> for D {
    fn event(_: &mut Self, _: &ExtIdleNotificationV1, ev: ext_idle_notification_v1::Event, k: &IdleKind, _: &Connection, _: &QueueHandle<Self>) {
        if let ext_idle_notification_v1::Event::Idled = ev {
            match k.0 {
                0 => spawn("shadow-lock"),
                _ => {
                    let _ = niri::action(&["power-off-monitors"]);
                }
            }
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("status") => {
            print!("{}", std::fs::read_to_string(runtime_dir().join("display.state")).unwrap_or_else(|_| "shadow-displayd has not run yet\n".into()));
            0
        }
        Some("apply") => {
            let r = reconcile(&cfg());
            write_state(&r);
            print!("{r}");
            0
        }
        Some("lid-close") => lid(true),
        Some("lid-open") => lid(false),
        Some("-h") | Some("--help") => {
            println!("shadow-displayd [status | apply | lid-close | lid-open]");
            0
        }
        _ => match daemon() {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("shadow-displayd: {e}");
                1
            }
        },
    };
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_policy() {
        let mut c = Cfg { pinned: BTreeMap::new(), policy: "auto".into(), hz_ac: 120.0, hz_battery: 60.0, lock_min: 10, off_min: 12 };
        std::env::set_var("SHADOW_SYS_ROOT", "/nonexistent");
        assert_eq!(wanted_hz(&c, true), Some(120.0));
        assert_eq!(wanted_hz(&c, false), Some(60.0));
        c.policy = "fixed".into();
        assert_eq!(wanted_hz(&c, false), None);
    }
}
