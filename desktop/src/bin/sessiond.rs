//! shadow-sessiond — the session's event hub on niri.
//!
//!  * follows niri's event stream (focus, new windows, workspaces)
//!  * sends a 32-byte telemetry record to shadow-powerd on every focus change / launch
//!  * hosts the sandboxed Lua layer: window rules on open, bar widgets, events, key macros
//!  * keeps niri's config in step with the Lua key macros (`shadow setup niri`)
//!
//!   shadow-sessiond                 run (started by niri at login)
//!   shadow-sessiond macro KEY       run the Lua macro bound to KEY (niri binds call this)
//!   shadow-sessiond reload          reload the Lua modules
//!   shadow-sessiond status          modules, errors, focus
use shadow_desktop::{luahost::LuaHost, runtime_dir, spawn, sys, telemetry};
use shadow_niri::{self as niri, Change, Model};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc;
use std::time::{Duration, Instant};

enum Msg {
    Niri(String),
    Cmd(String, mpsc::Sender<String>),
}

fn sock_path_for_exit() -> std::path::PathBuf {
    sock()
}

fn sock() -> std::path::PathBuf {
    runtime_dir().join("sessiond.sock")
}

fn client(cmd: &str) -> i32 {
    match UnixStream::connect(sock()) {
        Ok(mut s) => {
            let _ = s.set_read_timeout(Some(Duration::from_secs(3)));
            let _ = writeln!(s, "{cmd}");
            let mut out = String::new();
            let _ = BufReader::new(s).read_line(&mut out);
            let out = out.trim();
            println!("{out}");
            if out.starts_with("error") { 1 } else { 0 }
        }
        Err(e) => {
            eprintln!("shadow-sessiond is not running ({e})");
            1
        }
    }
}

struct Hub {
    lua: Option<LuaHost>,
    model: Model,
    widgets: String,
    last_widgets: Instant,
}

impl Hub {
    fn load_lua(&mut self) {
        self.lua = match LuaHost::new() {
            Ok(h) => Some(h),
            Err(e) => {
                eprintln!("shadow-sessiond: lua host: {e}");
                None
            }
        };
        self.sync_macros();
    }

    /// Persist the Lua macro keys; when they change, regenerate niri's config so the keys exist.
    fn sync_macros(&self) {
        let keys: Vec<String> = self.lua.as_ref().map(|l| l.binds().into_iter().map(|(k, _)| k).collect()).unwrap_or_default();
        let f = shadow_ui::theme::state_dir().join("macros.list");
        let new = keys.join("\n");
        if std::fs::read_to_string(&f).unwrap_or_default().trim() != new.trim() {
            let _ = std::fs::write(&f, format!("{new}\n"));
            let _ = std::process::Command::new("shadow").args(["setup", "niri"]).stdout(std::process::Stdio::null()).status();
        }
    }

    fn run_actions(&mut self) {
        let acts = self.lua.as_mut().map(|l| l.take_actions()).unwrap_or_default();
        for a in acts {
            let (verb, rest) = a.split_once(' ').unwrap_or((&a, ""));
            let r = match verb {
                "spawn" => {
                    spawn(rest);
                    Ok(())
                }
                "notify" => {
                    let _ = std::process::Command::new("notify-send").args(["-a", "Shadow", rest]).spawn();
                    Ok(())
                }
                "focus-next" => niri::action(&["focus-column-right-or-first"]),
                "focus-prev" => niri::action(&["focus-column-left-or-last"]),
                "width-up" => niri::action(&["switch-preset-column-width"]),
                "width-down" => niri::action(&["switch-preset-column-width-back"]),
                "fullscreen" => niri::action(&["fullscreen-window"]),
                "maximize" => niri::action(&["maximize-column"]),
                "center" => niri::action(&["center-column"]),
                "overview" => niri::action(&["toggle-overview"]),
                "lock" => {
                    spawn("shadow-lock");
                    Ok(())
                }
                "hud-toggle" => {
                    let _ = std::process::Command::new("pkill").args(["-USR1", "-x", "shadow-bar"]).status();
                    Ok(())
                }
                _ => Ok(()),
            };
            if let Err(e) = r {
                eprintln!("shadow-sessiond: action {a}: {e}");
            }
        }
    }

    fn snapshot(&self) {
        if let Some(l) = &self.lua {
            let p = sys::power();
            l.set_snapshot(p.ac, p.battery.map(|b| b as i64).unwrap_or(-1), self.model.windows.len() as i64);
        }
    }

    fn on_niri(&mut self, line: &str) {
        for c in self.model.apply(line) {
            match c {
                Change::Focus { app_id, title, pid } => {
                    telemetry::send(&app_id);
                    record_ram(&app_id, pid);
                    self.snapshot();
                    if let Some(l) = self.lua.as_mut() {
                        l.event("focus", &app_id, &title);
                    }
                }
                Change::Opened { app_id, title, .. } => {
                    telemetry::send(&app_id);
                    self.snapshot();
                    if let Some(l) = self.lua.as_mut() {
                        let r = l.rule(&app_id, &title);
                        l.event("spawn", &app_id, &title);
                        // a new window has focus in niri, so column actions apply to it
                        if let Some(p) = r.preset {
                            let presets = presets();
                            if let Some(w) = presets.get(p) {
                                let _ = niri::action(&["set-column-width", &format!("{}%", (w * 100.0).round())]);
                            }
                        }
                        if r.full == Some(true) {
                            let _ = niri::action(&["fullscreen-window"]);
                        }
                        if r.floating == Some(true) {
                            let _ = niri::action(&["move-window-to-floating"]);
                        }
                    }
                }
                Change::Config { ok } => {
                    let _ = std::fs::write(runtime_dir().join("niri-config.state"), if ok { "ok\n" } else { "error\n" });
                }
                _ => {}
            }
        }
        self.run_actions();
    }

    fn widgets(&mut self) {
        self.last_widgets = Instant::now();
        self.snapshot();
        let w = self.lua.as_mut().map(|l| l.widgets()).unwrap_or_default().join("\n");
        if w != self.widgets {
            self.widgets = w.clone();
            let _ = std::fs::write(runtime_dir().join("widgets"), format!("{w}\n"));
        }
    }

    fn command(&mut self, cmd: &str) -> String {
        let (verb, rest) = cmd.split_once(' ').unwrap_or((cmd, ""));
        match verb {
            "macro" => {
                let binds = self.lua.as_ref().map(|l| l.binds()).unwrap_or_default();
                match binds.into_iter().find(|(k, _)| k.eq_ignore_ascii_case(rest.trim())) {
                    Some((_, i)) => {
                        if let Some(l) = self.lua.as_mut() {
                            l.call_bind(i);
                        }
                        self.run_actions();
                        "ok".into()
                    }
                    None => format!("error: no macro bound to {rest}"),
                }
            }
            "reload" => {
                self.load_lua();
                self.widgets();
                format!("ok {}", self.lua.as_ref().map(|l| l.modules().join(",")).unwrap_or_default())
            }
            "status" => format!(
                "modules={} windows={} focus={}",
                self.lua.as_ref().map(|l| l.modules().join(",")).unwrap_or_default(),
                self.model.windows.len(),
                self.model.focused().map(|w| w.app_id.clone()).unwrap_or_default()
            ),
            _ => "error: unknown command (macro KEY, reload, status)".into(),
        }
    }
}

/// Resident memory of the focused app (its process and children), averaged per app id into
/// ~/.config/shadow-state/apps.ram — the launcher's "~MB" estimates.
fn record_ram(app_id: &str, pid: i64) {
    if app_id.is_empty() || pid <= 0 {
        return;
    }
    let kb = |p: i64| -> u64 {
        std::fs::read_to_string(format!("/proc/{p}/status")).ok().and_then(|s| s.lines().find_map(|l| l.strip_prefix("VmRSS:").and_then(|v| v.split_whitespace().next()?.parse().ok()))).unwrap_or(0)
    };
    let mut total = kb(pid);
    if let Ok(ch) = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")) {
        total += ch.split_whitespace().filter_map(|c| c.parse().ok()).map(kb).sum::<u64>();
    }
    if total == 0 {
        return;
    }
    let mb = (total / 1024) as f64;
    let f = shadow_ui::theme::state_dir().join("apps.ram");
    let mut map: std::collections::BTreeMap<String, f64> = std::fs::read_to_string(&f).unwrap_or_default().lines().filter_map(|l| l.split_once('\t').and_then(|(k, v)| v.trim().parse().ok().map(|n| (k.to_string(), n)))).collect();
    let key = app_id.to_lowercase();
    let e = map.entry(key).or_insert(mb);
    *e = 0.7 * *e + 0.3 * mb;
    let text: String = map.iter().map(|(k, v)| format!("{k}\t{}\n", v.round() as u64)).collect();
    let tmp = f.with_extension("tmp~");
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, &f);
    }
}

/// width presets from config.toml [layout] presets (fractions or percents)
fn presets() -> Vec<f64> {
    let cfg = shadow_ui::theme::config();
    let v: Vec<f64> = cfg
        .get("layout")
        .and_then(|l| l.get("presets"))
        .and_then(|p| p.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_float().or_else(|| x.as_integer().map(|i| i as f64))).map(|x| if x > 1.0 { x / 100.0 } else { x }).collect())
        .unwrap_or_default();
    if v.is_empty() {
        vec![0.45, 0.65, 0.85]
    } else {
        v
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("macro") => std::process::exit(client(&format!("macro {}", args.get(1).cloned().unwrap_or_default()))),
        Some("reload") => std::process::exit(client("reload")),
        Some("status") => std::process::exit(client("status")),
        Some("polkit") => {
            // for the installer's check and for troubleshooting
            match (shadow_desktop::privilege::agent_running(), shadow_desktop::privilege::agent_path()) {
                (Some(a), _) => println!("running: {a}"),
                (None, Some(p)) => println!("installed, not running: {p}"),
                (None, None) => {
                    println!("none installed");
                    std::process::exit(1);
                }
            }
            return;
        }
        Some("-h") | Some("--help") => {
            println!("shadow-sessiond [macro KEY | reload | status]");
            return;
        }
        _ => {}
    }
    let (tx, rx) = mpsc::channel::<Msg>();
    let p = sock();
    if UnixStream::connect(&p).is_ok() {
        eprintln!("shadow-sessiond: already running");
        std::process::exit(0);
    }
    // privileged actions must be able to ask for a password: make sure a polkit agent runs
    eprintln!("shadow-sessiond: {}", shadow_desktop::privilege::ensure_agent());
    let _ = std::fs::remove_file(&p);
    match UnixListener::bind(&p) {
        Ok(l) => {
            let tx = tx.clone();
            std::thread::spawn(move || {
                for s in l.incoming().flatten() {
                    let mut line = String::new();
                    let mut r = BufReader::new(match s.try_clone() {
                        Ok(c) => c,
                        Err(_) => continue,
                    });
                    if r.read_line(&mut line).is_err() {
                        continue;
                    }
                    let (rtx, rrx) = mpsc::channel();
                    if tx.send(Msg::Cmd(line.trim().to_string(), rtx)).is_err() {
                        return;
                    }
                    let reply = rrx.recv_timeout(Duration::from_secs(3)).unwrap_or_else(|_| "error: timeout".into());
                    let mut s = s;
                    let _ = writeln!(s, "{reply}");
                }
            });
        }
        Err(e) => eprintln!("shadow-sessiond: control socket: {e}"),
    }
    if niri::available() {
        let tx = tx.clone();
        let sock = std::env::var_os("NIRI_SOCKET").map(std::path::PathBuf::from);
        std::thread::spawn(move || {
            niri::event_stream(|l| {
                if l.is_empty() {
                    // niri went away: if its socket is gone the session is over
                    if sock.as_ref().is_some_and(|s| !s.exists()) {
                        let _ = std::fs::remove_file(sock_path_for_exit());
                        std::process::exit(0);
                    }
                    return true;
                }
                tx.send(Msg::Niri(l.to_string())).is_ok()
            })
        });
    } else {
        eprintln!("shadow-sessiond: NIRI_SOCKET not set — run inside the niri session");
    }
    let mut hub = Hub { lua: None, model: Model::default(), widgets: String::new(), last_widgets: Instant::now() };
    hub.load_lua();
    hub.widgets();
    loop {
        let wait = Duration::from_secs(2).saturating_sub(hub.last_widgets.elapsed());
        match rx.recv_timeout(wait) {
            Ok(Msg::Niri(l)) => hub.on_niri(&l),
            Ok(Msg::Cmd(c, reply)) => {
                let r = hub.command(&c);
                let _ = reply.send(r);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => hub.widgets(),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if hub.last_widgets.elapsed() >= Duration::from_secs(2) {
            hub.widgets();
        }
    }
}
