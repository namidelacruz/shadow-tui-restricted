//! shadow-powerd — Shadow TUI's Dell optimizer (Dell 14 Plus / Intel Core Ultra 200V).
#[path = "../../tui/src/json.rs"]
#[allow(dead_code)]
mod json;
mod agent;
mod audio;
mod daemon;
mod hw;
mod kernel;
mod learn;
mod power;
mod predict;
mod rapl;
#[allow(dead_code)]
mod sys;

use json::Value;

const HELP: &str = "\
shadow-powerd — Dell optimizer for Shadow TUI

  status                         machine, power, refresh rate and what has been learned
  hw                             detected hardware and which controls the firmware exposes
  mode [MODE] [--keep]           thermal/fan + CPU mode: auto performance balanced quiet cool low-power
                                 (a manual mode lasts until the power source changes; --keep makes it
                                 the default for AC or battery, whichever you are on)
  policy AC_MODE BATTERY_MODE    defaults for AC and battery (e.g. policy auto low-power)
  charge [MODE] [START END]      standard adaptive express ac custom (custom START 50-95, END 55-100)
  refresh [auto|max|60|HZ] [--keep]
                                 built-in panel refresh: auto = highest on AC, 60 Hz on battery
  learn [--reset|--on|--off]     show / reset / toggle learning from your choices
  audio [--fix]                  Lunar Lake audio doctor (\"Dummy Output\", firmware, UCM, kernel)
  kernel [status|ensure-fallback|check-upgrade]
                                 known-bad kernel guard and linux-lts fallback
  npu                            NPU (AI accelerator) readiness
  rapl                           package power limits (PL1/PL2) now and from firmware
  memory                         the 20 % RAM ceiling: shadow.slice limits and current use
  daemon                         the root service (shadow-powerd.service)
  (as shadow-audiod: audio --fix at boot and resume)
";

/// [power] learning in ~/.config/shadow-state/config.toml (line-preserving)
fn write_setting(_path: &[&str], v: Value) -> Result<(), String> {
    let on = matches!(v, Value::Bool(true));
    let f = sys::config_dir().join("config.toml");
    let text = std::fs::read_to_string(&f).unwrap_or_default();
    let mut out = String::new();
    let (mut cur, mut done) = (String::new(), false);
    for l in text.lines() {
        let t = l.trim();
        if t.starts_with('[') {
            if cur == "power" && !done {
                out.push_str(&format!("learning = {on}\n"));
                done = true;
            }
            cur = t.trim_matches(|c| c == '[' || c == ']').to_string();
        } else if cur == "power" && t.split('=').next().map(str::trim) == Some("learning") {
            out.push_str(&format!("learning = {on}\n"));
            done = true;
            continue;
        }
        out.push_str(l);
        out.push('\n');
    }
    if !done {
        if cur != "power" {
            out.push_str("\n[power]\n");
        }
        out.push_str(&format!("learning = {on}\n"));
    }
    let _ = std::fs::create_dir_all(f.parent().unwrap());
    sys::atomic_write(&f, &out).map_err(|e| e.to_string())
}

fn memory() -> i32 {
    let total_kb: u64 = sys::read("proc/meminfo").and_then(|m| m.lines().find_map(|l| l.strip_prefix("MemTotal:")).and_then(|v| v.split_whitespace().next()?.parse().ok())).unwrap_or(0);
    let mib = |b: u64| b >> 20;
    let mut sum_max = 0u64;
    let uid = sys::run(&["id", "-u"]).1;
    for (label, path) in [("system  shadow.slice", "sys/fs/cgroup/shadow.slice".to_string()), ("session shadow.slice", format!("sys/fs/cgroup/user.slice/user-{uid}.slice/user@{uid}.service/shadow.slice"))] {
        let cur = sys::read(&format!("{path}/memory.current")).and_then(|v| v.parse::<u64>().ok());
        let max = sys::read(&format!("{path}/memory.max")).and_then(|v| v.parse::<u64>().ok());
        let high = sys::read(&format!("{path}/memory.high")).and_then(|v| v.parse::<u64>().ok());
        match cur {
            Some(c) => {
                sum_max += max.unwrap_or(0);
                println!("{label}: using {} MiB · high {} · max {}", mib(c), high.map(|h| format!("{} MiB", mib(h))).unwrap_or("-".into()), max.map(|m| format!("{} MiB", mib(m))).unwrap_or("-".into()));
            }
            None => println!("{label}: not active"),
        }
    }
    if total_kb > 0 {
        let pct = sum_max as f64 / (total_kb as f64 * 1024.0) * 100.0;
        println!("ceiling: {:.1} % of {} MiB RAM (target ≤ 20 %)", pct, total_kb >> 10);
    }
    0
}

fn main() {
    if std::env::args().next().map_or(false, |a| a.ends_with("shadow-audiod")) {
        // boot / resume: run the audio fixer quietly; never fail the boot
        let h = hw::detect();
        let r = audio::diagnose(&h);
        let status = if r.problems.is_empty() { "ok" } else if audio::fix(&h) { "fixed" } else { "fault" };
        let d = sys::p("run/shadow");
        let _ = std::fs::create_dir_all(&d);
        let _ = std::fs::write(d.join("audio.state"), format!("status={status}\n"));
        std::process::exit(0);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let code = match a.as_slice() {
        [] | ["status"] => status(),
        ["hw"] => {
            print!("{}", hw::report(&hw::detect()));
            0
        }
        ["mode"] => show_mode(),
        ["mode", m, rest @ ..] => set_mode(m, rest.contains(&"--keep")),
        ["policy", ac, bat] => req(&format!("policy {ac} {bat}")),
        ["charge"] => show_charge(),
        ["charge", rest @ ..] => req(&format!("charge {}", rest.join(" "))),
        ["refresh"] => show_refresh(),
        ["refresh", r, rest @ ..] => set_refresh(r, rest.contains(&"--keep")),
        ["learn", rest @ ..] => learn_cmd(rest),
        ["audio"] => {
            let r = audio::diagnose(&hw::detect());
            audio::print(&r);
            if r.problems.is_empty() { 0 } else { 1 }
        }
        ["audio", "--fix"] => if audio::fix(&hw::detect()) { 0 } else { 1 },
        ["kernel"] | ["kernel", "status"] => {
            kernel::status(&hw::detect());
            0
        }
        // runs inside a pacman hook with AbortOnFail: an internal error must never block updates,
        // only a confirmed known-bad kernel with no fallback does
        ["kernel", "check-upgrade"] => std::panic::catch_unwind(|| kernel::check_upgrade(&hw::detect())).unwrap_or_else(|_| {
            eprintln!("Shadow TUI: kernel check failed internally — not blocking the update");
            0
        }),
        ["kernel", "ensure-fallback"] => match kernel::ensure_fallback(&hw::detect()) {
            Ok(m) => {
                println!("{m}");
                0
            }
            Err(e) => {
                eprintln!("shadow-powerd: {e}");
                1
            }
        },
        ["npu"] => npu(),
        ["daemon"] => fail(daemon::run()),
        ["agent"] => {
            println!("the session agent is no longer needed: shadow-compositor switches the refresh rate and reports focus");
            0
        }
        ["rapl"] => {
            match (rapl::firmware(), rapl::pl1_w()) {
                (Some((a, b)), Some(now)) => println!("PL1 now {now:.1} W · firmware PL1 {:.1} W / PL2 {:.1} W", a as f64 / 1e6, b as f64 / 1e6),
                _ => println!("intel-rapl powercap not available"),
            }
            0
        }
        ["memory"] => memory(),
        ["-h"] | ["--help"] | ["help"] => {
            print!("{HELP}");
            0
        }
        _ => {
            eprint!("{HELP}");
            2
        }
    };
    std::process::exit(code);
}

fn fail(r: Result<(), String>) -> i32 {
    match r {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("shadow-powerd: {e}");
            1
        }
    }
}

fn req(cmd: &str) -> i32 {
    match daemon::request(cmd) {
        Ok(r) => {
            println!("{}", r.trim_start_matches("ok ").trim_start_matches("ok"));
            0
        }
        Err(e) => {
            eprintln!("shadow-powerd: {e}");
            1
        }
    }
}

fn state() -> Option<Value> {
    daemon::request("status").ok().and_then(|s| json::parse(&s).ok())
}

fn show_mode() -> i32 {
    match state() {
        Some(s) => {
            println!("mode    : {} ({})", s.path_str(&["mode"]).unwrap_or_default(), s.path_str(&["reason"]).unwrap_or_default());
            println!("thermal : {}   cpu: {}   via: {}", s.path_str(&["thermal"]).unwrap_or("-".into()),
                s.path_str(&["epp"]).unwrap_or("-".into()), s.path_str(&["via"]).unwrap_or_default());
            println!("defaults: AC {} · battery {}", s.path_str(&["ac_mode"]).unwrap_or_default(), s.path_str(&["battery_mode"]).unwrap_or_default());
            println!("modes   :");
            for (k, d) in power::MODES {
                println!("  {k:<12} {d}");
            }
            0
        }
        None => req("status"),
    }
}

fn set_mode(m: &str, keep: bool) -> i32 {
    if !power::valid_mode(m) {
        eprintln!("shadow-powerd: unknown mode {m} (auto performance balanced quiet cool low-power)");
        return 2;
    }
    if m != "auto" && !sys::is_root() && agent::learning_on() {
        learn::record("mode", &agent::context(), m);
    }
    req(&format!("mode {m}{}", if keep { " keep" } else { "" }))
}

fn show_charge() -> i32 {
    let h = hw::detect();
    let cur = power::charge_current(&h).unwrap_or_else(|| "-".into());
    println!("charging : {cur}{}", power::thresholds(&h).filter(|_| cur == "Custom").map(|(s, e)| format!(" ({s}–{e} %)")).unwrap_or_default());
    println!("available: {}", if h.charge_types.is_empty() { "not exposed by this firmware/kernel".into() } else { h.charge_types.join(" ") });
    println!("modes    : standard · adaptive (Dell learns your pattern) · express (fast) · ac (mostly plugged in) · custom START END");
    0
}

fn show_refresh() -> i32 {
    match agent::compositor_refresh() {
        Some(hz) => println!("panel   : {hz} Hz (shadow-compositor switches between [display] refresh_ac / refresh_battery in config.toml)"),
        None => println!("panel   : compositor not reachable (run inside the Shadow TUI session)"),
    }
    let st = sys::read("run/shadow/power.state").unwrap_or_default();
    if let Some(r) = st.lines().find_map(|l| l.strip_prefix("refresh=")) {
        println!("powerd  : suggests {r} Hz (used when [display] refresh_policy = \"powerd\")");
    }
    0
}

fn set_refresh(r: &str, keep: bool) -> i32 {
    let hz: u32 = match r {
        "max" => 240,
        "auto" => {
            println!("refresh: auto — the compositor follows the power source ([display] refresh_policy = \"auto\")");
            return 0;
        }
        n => match n.parse::<f64>() {
            Ok(x) if (24.0..=500.0).contains(&x) => x.round() as u32,
            _ => {
                eprintln!("shadow-powerd: refresh must be auto, max or a rate in Hz");
                return 2;
            }
        },
    };
    let ctx = agent::context();
    if agent::learning_on() {
        learn::record("refresh", &ctx, &hz.to_string());
    }
    let (code, out, err) = sys::run(&["shadowctl", "refresh", &hz.to_string()]);
    println!("{}", if code == 0 { out } else { err });
    if keep {
        println!("to make it permanent: set [display] refresh_{} = {hz} in ~/.config/shadow-state/config.toml (or Settings → Display)", if ctx.ac { "ac" } else { "battery" });
    }
    if code == 0 { 0 } else { 1 }
}

fn learn_cmd(rest: &[&str]) -> i32 {
    match rest {
        ["--reset"] => match learn::reset() {
            Ok(()) => {
                let _ = daemon::request("hint clear");
                println!("learning reset");
                0
            }
            Err(e) => {
                eprintln!("shadow-powerd: {e}");
                1
            }
        },
        ["--on"] | ["--off"] => match write_setting(&["power", "learning"], Value::Bool(rest[0] == "--on")) {
            Ok(()) => {
                if rest[0] == "--off" {
                    let _ = daemon::request("hint clear");
                }
                println!("learning {}", if rest[0] == "--on" { "on" } else { "off" });
                0
            }
            Err(e) => {
                eprintln!("shadow-powerd: {e}");
                1
            }
        },
        _ => {
            let m = learn::Model::load();
            println!("learning: {} · {} observations (from your manual choices; old ones fade over ~2 weeks)",
                if agent::learning_on() { "on" } else { "off" }, m.len());
            for l in m.explain() {
                println!("  {l}");
            }
            let ctx = agent::context();
            for kind in ["mode", "refresh"] {
                match m.predict(kind, &ctx) {
                    Some((c, share, n, desc)) => println!("now     : {kind} → {c} ({desc}; {:.0}% of {n:.1})", share * 100.0),
                    None => println!("now     : {kind} → no confident preference yet (needs 3+ choices in a similar situation)"),
                }
            }
            0
        }
    }
}

fn status() -> i32 {
    let h = hw::detect();
    println!("SHADOW NETWORK // OPTIMIZER");
    println!("machine : {} {}{}", h.vendor, h.product, if h.dell_14_plus { "  [Dell 14 Plus profile]" } else { "" });
    let src = power::source(&h);
    println!("power   : {}{}", if src.ac { "AC" } else { "battery" }, src.battery_pct.map(|p| format!(" · {p} %")).unwrap_or_default());
    match state() {
        Some(s) => {
            println!("mode    : {} ({}) → thermal {} · cpu {}", s.path_str(&["mode"]).unwrap_or_default(),
                s.path_str(&["reason"]).unwrap_or_default(), s.path_str(&["thermal"]).unwrap_or("-".into()), s.path_str(&["epp"]).unwrap_or("-".into()));
            if let Some(Value::Arr(n)) = s.get("notes") {
                for x in n {
                    println!("note    : {}", x.str().unwrap_or(""));
                }
            }
        }
        None => println!("mode    : optimizer service not running (sudo systemctl enable --now shadow-powerd.service)"),
    }
    let cur = power::charge_current(&h).unwrap_or_else(|| "-".into());
    println!("charging: {cur}");
    show_refresh();
    let ar = audio::diagnose(&h);
    println!("audio   : {}", if ar.problems.is_empty() { "OK".to_string() } else { ar.problems.iter().map(|p| p.1.clone()).collect::<Vec<_>>().join("; ") });
    let run = kernel::running_version();
    println!("kernel  : {run}{}", kernel::bad_for(&h, "*", kernel::running_package(), &run).map(|w| format!(" ⚠ {w}")).unwrap_or_default());
    println!("npu     : {}", if h.npu_kernel && h.npu_userspace { "ready" } else if h.npu_kernel { "kernel driver only (shadow-powerd npu)" } else { "not available" });
    0
}

fn npu() -> i32 {
    let h = hw::detect();
    let ok = |b: bool| if b { "OK  " } else { "MISS" };
    println!("[{}] kernel driver intel_vpu bound", ok(h.npu_kernel));
    println!("[{}] /dev/accel/accel0", ok(h.npu_node));
    println!("[{}] NPU firmware (linux-firmware)", ok(h.npu_firmware));
    println!("[{}] user-space driver (Level Zero NPU) — AUR: intel-npu-driver", ok(h.npu_userspace));
    println!("[{}] OpenVINO runtime — AUR: openvino-bin (NPU plugin)", ok(h.openvino));
    let groups = sys::run(&["id", "-nG"]).1;
    let render = groups.split_whitespace().any(|g| g == "render");
    println!("[{}] you are in the 'render' group (access to the NPU device)", ok(render));
    println!();
    println!("The optimizer's learning runs on the CPU (a few microseconds per decision). The NPU is used");
    println!("by the local assistant model in a later phase, where it saves real power over the CPU/GPU.");
    if h.npu_kernel && h.npu_userspace && h.openvino && render { 0 } else { 1 }
}
