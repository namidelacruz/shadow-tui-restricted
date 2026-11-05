//! shadow-compositor — the Shadow TUI Restricted display server.
//!
//!   shadow-compositor              session (DRM/KMS on a TTY, nested window inside another session)
//!   shadow-compositor --greeter    login screen for greetd (hands off to the session seamlessly)
//!   shadow-compositor --winit      force the nested backend      --udev   force DRM/KMS
mod config;
mod cursor;
#[path = "../../ui/src/glyphs.rs"]
#[allow(dead_code)]
mod glyphs;
mod hud;
mod input;
mod ipc;
mod luahost;
mod orbit;
mod pam;
mod render;
mod shaders;
mod state;
mod telemetry;
mod text;
mod udev;
mod tutorial;
mod ui;
mod winit;

use smithay::reexports::{
    calloop::{channel, generic::Generic, timer::{TimeoutAction, Timer}, EventLoop, Interest, Mode, PostAction},
    wayland_server::Display,
};
use smithay::wayland::socket::ListeningSocketSource;
use state::{Backend, ClientState, State};
use std::sync::Arc;
use std::time::Duration;

pub fn backend_apply_refresh(st: &mut State) {
    let Some(hz) = st.wanted_refresh else { return };
    match st.backend {
        Backend::Udev(_) => udev::apply_refresh(st, hz),
        _ => st.refresh_hz = st.refresh_hz.min(hz).max(1), // nested: the host decides; just report
    }
}

fn read_proc_cpu(pid: i32) -> Option<(u64, u64)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = stat.rsplit_once(')')?.1;
    let f: Vec<&str> = rest.split_whitespace().collect();
    let proc_t = f.get(11)?.parse::<u64>().ok()? + f.get(12)?.parse::<u64>().ok()?;
    let total: u64 = std::fs::read_to_string("/proc/stat").ok()?.lines().next()?.split_whitespace().skip(1).filter_map(|v| v.parse::<u64>().ok()).sum();
    Some((proc_t, total))
}

/// every 2 s: HUD text, ring sparklines, refresh policy, idle lock, memory pressure, Lua widgets
fn tick(st: &mut State) {
    let n = st.ring.order.len();
    let focused = st.ring.center().and_then(|id| st.win(id)).map(|w| if w.app_id.is_empty() { w.title.clone() } else { w.app_id.clone() }).unwrap_or_default();
    let mut extra = st.hud_extra.clone();
    if let Some(p) = &st.cfg_problem {
        extra.push(format!("CONFIG ERROR: {}", p.chars().take(40).collect::<String>()));
    }
    if let Some(l) = st.lua.as_mut() {
        extra.extend(l.widgets());
    }
    let snap = hud::snapshot(if n > 0 { 1 } else { 0 }, n, &focused, st.refresh_hz, &extra);
    let changed = st.hud.as_ref().map(|h| h.top_left != snap.top_left || h.top_right != snap.top_right || h.bottom != snap.bottom).unwrap_or(true);
    let ac = snap.power.ac;
    let bat = snap.power.battery.map(|b| b as i64).unwrap_or(-1);
    st.hud = Some(snap);
    if changed && st.hud_visible {
        st.dirty = true;
    }
    if let Some(l) = st.lua.as_ref() {
        l.set_snapshot(ac, bat, n as i64);
    }
    st.run_lua_actions();
    // ring sparklines (only background windows; the centre is live anyway)
    let center = st.ring.center();
    let page_kb = 4u64;
    for w in st.wins.iter_mut().filter(|w| Some(w.id) != center) {
        let Some(pid) = w.pid else { continue };
        if let Some((p, t)) = read_proc_cpu(pid) {
            if let Some((pp, pt)) = w.cpu_prev {
                let cpu = if t > pt { (p.saturating_sub(pp)) as f32 / (t - pt) as f32 * 100.0 * num_cpus() } else { 0.0 };
                w.spark.push(cpu.clamp(0.0, 100.0));
                if w.spark.len() > 12 {
                    w.spark.remove(0);
                }
                st.dirty = true;
            }
            w.cpu_prev = Some((p, t));
        }
        if let Some(rss) = std::fs::read_to_string(format!("/proc/{pid}/statm")).ok().and_then(|s| s.split_whitespace().nth(1).and_then(|v| v.parse::<u64>().ok())) {
            w.rss_mb = rss * page_kb / 1024;
        }
    }
    // refresh: shadow-powerd's request wins, else AC/battery thresholds from the config
    if !st.greeter_mode {
        let pol = st.cfg.display.refresh_policy.clone();
        let want = match pol.as_str() {
            "ac" => st.cfg.display.refresh_ac,
            "battery" => st.cfg.display.refresh_battery,
            "powerd" => hud::wanted_refresh().unwrap_or(if ac { st.cfg.display.refresh_ac } else { st.cfg.display.refresh_battery }),
            _ => if ac { st.cfg.display.refresh_ac } else { st.cfg.display.refresh_battery },
        };
        if Some(want) != st.wanted_refresh {
            st.wanted_refresh = Some(want);
            backend_apply_refresh(st);
        }
    }
    // idle lock
    let idle = st.cfg.lock.idle_minutes;
    if idle > 0 && !st.greeter_mode && st.ui.is_none() && st.last_input.elapsed() > Duration::from_secs(idle * 60) {
        input::lock(st);
    }
    // memory.high reached in our cgroup: drop caches before the hard ceiling
    if let Some(high) = memory_high_events() {
        if high > st.mem_high_seen {
            if st.mem_high_seen > 0 {
                tracing::warn!("memory.high reached: evicting text caches and collecting Lua garbage");
                st.text.evict();
                if let Some(l) = st.lua.as_ref() {
                    l.gc();
                }
                st.dirty = true;
            }
            st.mem_high_seen = high.max(1);
        }
    }
}

fn num_cpus() -> f32 {
    std::thread::available_parallelism().map(|n| n.get() as f32).unwrap_or(1.0)
}

fn memory_high_events() -> Option<u64> {
    let cg = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    let path = cg.lines().find_map(|l| l.strip_prefix("0::"))?;
    let ev = std::fs::read_to_string(format!("/sys/fs/cgroup{path}/memory.events")).ok()?;
    ev.lines().find_map(|l| l.strip_prefix("high ").and_then(|v| v.trim().parse().ok()))
}

fn in_path(prog: &str) -> bool {
    std::env::var_os("PATH").map(|p| std::env::split_paths(&p).any(|d| d.join(prog).is_file())).unwrap_or(false)
}

/// Start xwayland-satellite on the first free X display and export DISPLAY for every app started
/// after it. Returns the child (killed when the session ends).
fn start_xwayland() -> Option<std::process::Child> {
    if std::env::var_os("SHADOW_NO_XWAYLAND").is_some() || !in_path("xwayland-satellite") || !in_path("Xwayland") {
        tracing::info!("X11 apps: xwayland-satellite or Xwayland not installed — Wayland apps only");
        return None;
    }
    let n = (1..64).find(|n| !std::path::Path::new(&format!("/tmp/.X11-unix/X{n}")).exists() && !std::path::Path::new(&format!("/tmp/.X{n}-lock")).exists())?;
    let disp = format!(":{n}");
    match std::process::Command::new("xwayland-satellite").arg(&disp).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).spawn() {
        Ok(c) => {
            std::env::set_var("DISPLAY", &disp);
            tracing::info!("X11 apps: xwayland-satellite on {disp}");
            Some(c)
        }
        Err(e) => {
            tracing::warn!("xwayland-satellite: {e}");
            None
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version") {
        println!("shadow-compositor {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("shadow-compositor [--greeter [--demo]] [--winit|--udev]\nConfig: ~/.config/shadow-state/config.toml  Control: shadowctl");
        return;
    }
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr).init();
    let greeter = args.iter().any(|a| a == "--greeter");
    if args.iter().any(|a| a == "--demo") {
        std::env::set_var("SHADOW_GREET_DEMO", "1");
    }
    let nested = args.iter().any(|a| a == "--winit") || (!args.iter().any(|a| a == "--udev") && (std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some()));

    let mut event_loop: EventLoop<'static, State> = EventLoop::try_new().expect("event loop");
    let display: Display<State> = Display::new().expect("wayland display");
    let dh = display.handle();
    let listening = ListeningSocketSource::new_auto().expect("wayland socket");
    let socket_name = listening.socket_name().to_string_lossy().into_owned();
    event_loop.handle().insert_source(listening, |stream, _, st| {
        if let Err(e) = st.dh.insert_client(stream, Arc::new(ClientState::default())) {
            tracing::warn!("client: {e}");
        }
    }).expect("socket source");
    event_loop.handle().insert_source(Generic::new(display, Interest::READ, Mode::Level), |_, d, st| {
        // safety: the display is never dropped while the loop runs
        unsafe {
            let _ = d.get_mut().dispatch_clients(st);
        }
        Ok(PostAction::Continue)
    }).expect("display source");

    let mut state = State::new(dh, event_loop.handle(), event_loop.get_signal(), socket_name.clone(), greeter);
    let (tx, rx) = channel::channel::<ui::UiMsg>();
    event_loop.handle().insert_source(rx, |ev, _, st| {
        if let channel::Event::Msg(m) = ev {
            input::on_ui_msg(st, m);
        }
    }).expect("ui channel");
    state.ui_tx = Some(tx);

    let r = if nested { winit::init(&mut event_loop, &mut state) } else { udev::init(&mut event_loop, &mut state) };
    if let Err(e) = r {
        eprintln!("shadow-compositor: cannot start the {} backend: {e}", if nested { "nested" } else { "DRM/KMS" });
        std::process::exit(2);
    }
    // children talk to us, not to the host
    std::env::set_var("WAYLAND_DISPLAY", &socket_name);
    std::env::remove_var("DISPLAY");
    std::env::set_var("XDG_CURRENT_DESKTOP", "ShadowTUI");
    std::env::set_var("XDG_SESSION_TYPE", "wayland");
    if let Err(e) = ipc::init(&mut state) {
        tracing::warn!("control socket: {e}");
    }
    let mut xwayland: Option<std::process::Child> = None;
    if greeter {
        state.ui = Some(ui::Ui::new(ui::Mode::Greeter));
    } else {
        // X11 apps (older AppImages, Electron without Wayland, Steam, Wine) through
        // xwayland-satellite, the same helper niri uses; skipped when it isn't installed
        xwayland = start_xwayland();
        luahost::reload(&mut state);
        for c in state.cfg.autostart.commands.clone() {
            input::spawn(&state, &c);
        }
        // once-a-day update check (notification only; nothing installs without you)
        input::spawn(&state, "shadow update login-check");
        // first login on this compositor: FIELD TRAINING, the hotkey tutorial
        if tutorial::first_run() {
            state.tutorial = Some(tutorial::Tutorial::new(&state.cfg.binds));
        }
    }
    state.binds = input::compile_binds(&state);
    tick(&mut state);
    event_loop.handle().insert_source(Timer::from_duration(Duration::from_secs(2)), |_, _, st| {
        tick(st);
        TimeoutAction::ToDuration(Duration::from_secs(2))
    }).expect("timer");
    tracing::info!("shadow-compositor on {socket_name} ({})", if greeter { "greeter" } else { "session" });

    event_loop.run(None, &mut state, |st| {
        if st.ui.as_ref().map(|u| u.handoff_done()).unwrap_or(false) {
            st.signal.stop(); // greeter: greetd starts the session on this VT now
            return;
        }
        match st.backend {
            Backend::Winit(_) => winit::schedule(st),
            Backend::Udev(_) => udev::schedule(st),
            Backend::None => {}
        }
        let _ = st.dh.flush_clients();
    }).expect("event loop");

    if let Some(mut x) = xwayland {
        let _ = x.kill();
        let _ = x.wait();
    }
    let _ = std::fs::remove_file(ipc::socket_path(&socket_name));
    if !greeter {
        let _ = std::fs::remove_file(std::path::PathBuf::from(std::env::var("XDG_RUNTIME_DIR").unwrap_or_default()).join("shadow/locked"));
    }
    std::process::exit(state.exit_code);
}
