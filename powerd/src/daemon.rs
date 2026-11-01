//! Root daemon (shadow-powerd.service). Owns the privileged knobs, re-applies them when the power
//! source changes or firmware/resume resets them, and serves a tiny line protocol on
//! /run/shadow-powerd/sock. Every request is validated against fixed vocabularies, so any local
//! user may ask, but only for these settings.
use crate::hw::{self, Hw};
use crate::power::{self, Source};
use crate::sys;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub fn sock_path() -> PathBuf {
    sys::run_dir().join("sock")
}
fn state_path() -> PathBuf {
    sys::run_dir().join("state.json")
}
fn conf_path() -> PathBuf {
    sys::p("etc/shadow-tui/powerd.conf")
}
fn log_path() -> PathBuf {
    sys::run_dir().join("actions.log")
}

struct Conf {
    ac_mode: String,
    battery_mode: String,
    charge: Option<String>,
    charge_start: Option<u8>,
    charge_end: Option<u8>,
    refresh_ac: u32,
    refresh_battery: u32,
    learning: bool,
}

fn load_conf() -> Conf {
    let mut c = Conf { ac_mode: "auto".into(), battery_mode: "auto".into(), charge: None, charge_start: None, charge_end: None, refresh_ac: 120, refresh_battery: 60, learning: true };
    for (k, v) in sys::read_kv(&conf_path()) {
        match k.as_str() {
            "ac_mode" if power::valid_mode(&v) => c.ac_mode = v,
            "battery_mode" if power::valid_mode(&v) => c.battery_mode = v,
            "charge" => c.charge = Some(v),
            "charge_start" => c.charge_start = v.parse().ok(),
            "charge_end" => c.charge_end = v.parse().ok(),
            "refresh_ac" => c.refresh_ac = v.parse().unwrap_or(120),
            "refresh_battery" => c.refresh_battery = v.parse().unwrap_or(60),
            "learning" => c.learning = v != "false",
            _ => {}
        }
    }
    c
}

fn save_conf(c: &Conf) {
    let mut t = String::from("# Shadow TUI optimizer — written by shadow-powerd (Settings → Power)\n");
    t.push_str(&format!("ac_mode = {}\nbattery_mode = {}\nrefresh_ac = {}\nrefresh_battery = {}\nlearning = {}\n", c.ac_mode, c.battery_mode, c.refresh_ac, c.refresh_battery, c.learning));
    if let Some(ch) = &c.charge {
        t.push_str(&format!("charge = {ch}\n"));
    }
    if let (Some(s), Some(e)) = (c.charge_start, c.charge_end) {
        t.push_str(&format!("charge_start = {s}\ncharge_end = {e}\n"));
    }
    let _ = sys::atomic_write(&conf_path(), &t);
}

struct State {
    hw: Hw,
    conf: Conf,
    src: Option<Source>,
    transient: Option<(String, bool)>,           // manual mode until the power source changes (mode, for_ac)
    hint: Option<(String, Instant, String)>,     // learned mode from the session agent, expires
    mode: String,
    reason: String,
    applied: power::Applied,
    last_apply: Instant,
    model: crate::predict::Model,
    last_learn: Option<Instant>,
    sched: crate::schedule::Schedule,
    os_checked: Option<(Instant, Option<std::time::SystemTime>)>,
    pl: String,
    charge: crate::charging::Guard,
}

fn sched_path() -> PathBuf {
    sys::p("var/lib/shadow-powerd/schedule.state")
}

fn learn_path() -> PathBuf {
    sys::p("var/lib/shadow-powerd/learn.state")
}

/// /run/shadow/*.state for the compositor HUD and refresh policy
fn run_state(name: &str, body: &str) {
    let d = std::env::var("SHADOW_RUN_STATE").map(PathBuf::from).unwrap_or_else(|_| sys::p("run/shadow"));
    let _ = std::fs::create_dir_all(&d);
    let _ = sys::atomic_write(&d.join(name), body);
}

/// spec §3.4: system half of the 20 % ceiling (daemons); the user half is shadow.slice in the session
pub fn memory_governance() -> Option<String> {
    let total_kb: u64 = sys::read("proc/meminfo")?.lines().find_map(|l| l.strip_prefix("MemTotal:")).and_then(|v| v.split_whitespace().next()?.parse().ok())?;
    let slice = "sys/fs/cgroup/shadow.slice";
    if !sys::exists(slice) {
        return None;
    }
    let (high, max) = (total_kb * 1024 * 3 / 100, total_kb * 1024 * 4 / 100);
    let a = sys::write(&format!("{slice}/memory.high"), &high.to_string()).is_ok();
    let b = sys::write(&format!("{slice}/memory.max"), &max.to_string()).is_ok();
    Some(format!("system shadow.slice: memory.high {} MiB, memory.max {} MiB ({})", high >> 20, max >> 20, if a && b { "set" } else { "not writable" }))
}

impl State {
    fn decide(&self) -> (String, String) {
        let src = self.src.clone().unwrap_or(Source { ac: true, battery_pct: None });
        if let Some((m, for_ac)) = &self.transient {
            if *for_ac == src.ac {
                return (m.clone(), "manual (until the power source changes)".into());
            }
        }
        let configured = if src.ac { &self.conf.ac_mode } else { &self.conf.battery_mode };
        if configured != "auto" {
            return (configured.clone(), format!("set for {}", if src.ac { "AC" } else { "battery" }));
        }
        if let Some((m, until, why)) = &self.hint {
            if Instant::now() < *until {
                return (m.clone(), format!("learned: {why}"));
            }
        }
        (power::auto_mode(&src).to_string(), format!("auto ({})", if src.ac { "on AC" } else { "on battery" }))
    }

    fn apply(&mut self, force: bool) {
        let (mode, reason) = self.decide();
        let reassert = std::env::var("SHADOW_OPT_REASSERT_S").ok().and_then(|v| v.parse().ok()).unwrap_or(60);
        if !force && mode == self.mode && self.last_apply.elapsed() < Duration::from_secs(reassert) {
            return;
        }
        let src = self.src.clone().unwrap_or(Source { ac: true, battery_pct: None });
        // re-assert silently if firmware/resume reset the knob; log only real changes
        let before = (self.applied.profile.clone(), self.applied.epp.clone());
        self.applied = power::apply_mode(&self.hw, &mode, &src);
        if crate::rapl::available() {
            match crate::rapl::set(crate::rapl::for_mode(&mode)) {
                Ok(t) => self.pl = t,
                Err(e) => self.applied.notes.push(format!("RAPL: {e}")),
            }
        }
        if mode != self.mode || (self.applied.profile.clone(), self.applied.epp.clone()) != before {
            sys::log_line(&log_path(), &format!(
                "mode {mode} ({reason}) → thermal {} · cpu {} · via {}",
                self.applied.profile.as_deref().unwrap_or("-"),
                self.applied.epp.as_deref().unwrap_or("-"),
                self.applied.via
            ));
        }
        self.mode = mode;
        self.reason = reason;
        self.last_apply = Instant::now();
        self.write_state();
    }

    fn write_state(&self) {
        let src = self.src.clone().unwrap_or(Source { ac: true, battery_pct: None });
        let (s, e) = power::thresholds(&self.hw).map_or((String::from("null"), String::from("null")), |(s, e)| (s.to_string(), e.to_string()));
        let notes: Vec<String> = self.applied.notes.iter().map(|n| format!("\"{}\"", sys::esc(n))).collect();
        let json = format!(
            "{{\"ac\":{},\"battery\":{},\"mode\":\"{}\",\"reason\":\"{}\",\"thermal\":\"{}\",\"epp\":\"{}\",\"via\":\"{}\",\
             \"ac_mode\":\"{}\",\"battery_mode\":\"{}\",\"charge\":\"{}\",\"charge_start\":{s},\"charge_end\":{e},\
             \"thermal_choices\":\"{}\",\"charge_choices\":\"{}\",\"notes\":[{}],\"updated\":{}}}\n",
            src.ac,
            src.battery_pct.map_or("null".into(), |p| p.to_string()),
            self.mode,
            sys::esc(&self.reason),
            self.applied.profile.as_deref().unwrap_or(""),
            self.applied.epp.as_deref().unwrap_or(""),
            sys::esc(&self.applied.via),
            self.conf.ac_mode,
            self.conf.battery_mode,
            power::charge_current(&self.hw).unwrap_or_default(),
            self.hw.profile_choices.join(" "),
            self.hw.charge_types.join(" "),
            notes.join(","),
            sys::now()
        );
        let _ = sys::atomic_write(&state_path(), &json);
        let refresh = if src.ac { self.conf.refresh_ac } else { self.conf.refresh_battery };
        // charging for the side bar: current firmware mode, the chosen mode, what the firmware offers
        let cur = power::charge_current(&self.hw).unwrap_or_default();
        let thr = power::thresholds(&self.hw).filter(|_| cur == "Custom").map(|(a, b)| format!(" {a}-{b}")).unwrap_or_default();
        let mut avail: Vec<&str> = vec![];
        for (tok, name) in [("Standard", "standard"), ("Adaptive", "adaptive"), ("Fast", "express"), ("Trickle", "ac"), ("Custom", "custom")] {
            if self.hw.charge_types.iter().any(|t| t == tok) {
                avail.push(name);
            }
        }
        if !avail.is_empty() {
            avail.insert(0, "smart");
            if avail.contains(&"standard") {
                avail.push("full");
            }
        }
        run_state(
            "power.state",
            &format!(
                "ac={}\nmode={}\nrefresh={refresh}\npl={}\ncharge={cur}{thr}\ncharge_mode={}\ncharge_avail={}\nfull_once={}\n",
                src.ac as u8,
                self.mode,
                self.pl,
                self.conf.charge.clone().unwrap_or_else(|| "-".into()),
                avail.join(" "),
                self.charge.full_once.is_some() as u8
            ),
        );
        let npu = match self.sched.progress(sys::now()) {
            Some((day, days, why)) => format!("state=learning\nday={day}\ndays={days}\nreason={why}\n"),
            None => "state=idle\nphase=learned\n".to_string(),
        };
        run_state("npu.state", &format!("{npu}backend=cpu-statistical\nclasses={}\n", self.model.table.len()));
    }

    /// a 32-byte focus record from the compositor
    fn telemetry(&mut self, rec: crate::predict::Record) {
        use crate::predict::Load;
        if let Some((class, util, prior)) = self.model.focus(rec.class, crate::predict::cpu_times()) {
            self.learned(class, util, prior);
        }
        // while (re)learning, the model only watches: no power decisions until it has a week
        if !self.conf.learning || !self.sched.feed_forward() {
            return;
        }
        let ac = self.src.as_ref().map_or(rec.ac, |s| s.ac);
        let hint = match self.model.predict(rec.class) {
            Load::Low if !ac => Some((if self.hw.profile_choices.iter().any(|c| c == "cool") { "cool" } else { "low-power" }, 900, "predicted light work (reading/typing)")),
            Load::High => Some((if ac { "performance" } else { "balanced" }, 600, "predicted heavy work (compile/media)")),
            _ => None,
        };
        match hint {
            Some((m, secs, why)) => {
                self.hint = Some((m.to_string(), Instant::now() + Duration::from_secs(secs), why.to_string()));
                self.apply(false);
            }
            None if self.hint.as_ref().map_or(false, |h| h.2.starts_with("predicted")) => {
                self.hint = None;
                self.apply(false);
            }
            None => {}
        }
    }

    fn learned(&mut self, class: u32, util: f64, prior: Option<f64>) {
        self.last_learn = Some(Instant::now());
        if let Some(p) = prior {
            if let Some(msg) = self.sched.prediction_error(sys::now(), (util - p).abs()) {
                sys::log_line(&log_path(), &msg);
                self.hint = None;
            }
            self.save_sched();
        }
        let _ = std::fs::create_dir_all(learn_path().parent().unwrap());
        let _ = sys::atomic_write(&learn_path(), &self.model.save());
        sys::log_line(&log_path(), &format!("learned: class {class:08x} used {:.0}% CPU", util * 100.0));
        self.write_state();
    }

    /// Smart charging: decide from the AC history (at most once a day, or `now`), write only a
    /// change, through the guard. Also ends "100 % once" when the battery is full.
    fn smart_charge(&mut self, now_please: bool) -> Result<String, String> {
        let now = sys::now();
        if let Some(t) = self.charge.full_once {
            let full = self.src.as_ref().and_then(|s| s.battery_pct).is_some_and(|p| p >= 99);
            if !full && now.saturating_sub(t) < 86400 {
                return Ok("charging to 100 % once".into());
            }
            self.charge.full_once = None;
            self.charge.last_smart = 0;
            sys::log_line(&log_path(), "charging: full charge done, back to the chosen mode");
            if self.conf.charge.as_deref() != Some("smart") {
                // back to the manual mode chosen before
                let m = self.conf.charge.clone().unwrap_or_else(|| "standard".into());
                let r = crate::charging::safe_set(&self.hw, &mut self.charge, &m, self.conf.charge_start, self.conf.charge_end, now);
                self.charge.save();
                return r.map(|_| m);
            }
        }
        if self.conf.charge.as_deref() != Some("smart") || self.charge.paused {
            return Ok("not smart".into());
        }
        if !now_please && now.saturating_sub(self.charge.last_smart) < 86400 {
            return Ok("decided today".into());
        }
        self.charge.last_smart = now;
        let (m, s, e) = crate::charging::smart_choice(&self.hw.charge_types, self.hw.charge_thresholds, self.charge.ac_share());
        let r = crate::charging::safe_set(&self.hw, &mut self.charge, m, s, e, now);
        self.charge.save();
        match r {
            Ok(written) => {
                if written {
                    sys::log_line(&log_path(), &format!("smart charging → {m}{} (on AC {} of the time)", s.zip(e).map(|(a, b)| format!(" {a}–{b} %")).unwrap_or_default(), self.charge.ac_share().map(|x| format!("{:.0} %", x * 100.0)).unwrap_or_else(|| "unknown share".into())));
                }
                self.write_state();
                Ok(m.to_string())
            }
            Err(e) => {
                sys::log_line(&log_path(), &format!("smart charging: {e}"));
                Err(e)
            }
        }
    }

    fn save_sched(&self) {
        let _ = std::fs::create_dir_all(sched_path().parent().unwrap());
        let _ = sys::atomic_write(&sched_path(), &self.sched.save());
    }

    /// OS fingerprint when pacman.log changes (or hourly); phase completion every poll.
    fn check_schedule(&mut self) {
        let mt = std::fs::metadata(crate::schedule::package_db_marker()).and_then(|m| m.modified()).ok();
        let due = match self.os_checked {
            None => true,
            Some((t, m)) => m != mt || t.elapsed() >= Duration::from_secs(3600),
        };
        let os = if due {
            self.os_checked = Some((Instant::now(), mt));
            crate::schedule::os_fingerprint()
        } else {
            String::new()
        };
        let was = self.sched.feed_forward();
        self.model.stable = was;
        if let Some(msg) = self.sched.tick(sys::now(), &os) {
            sys::log_line(&log_path(), &msg);
            self.save_sched();
            if was && !self.sched.feed_forward() && self.hint.take().is_some() {
                self.apply(false); // drop predicted settings while relearning
            }
            self.model.stable = self.sched.feed_forward();
            self.write_state();
        } else if due {
            self.save_sched();
        }
    }

    fn handle(&mut self, line: &str) -> String {
        let w: Vec<&str> = line.split_whitespace().collect();
        match w.as_slice() {
            ["status"] => std::fs::read_to_string(state_path()).unwrap_or_else(|_| "{}".into()).trim().to_string(),
            ["relearn"] => {
                self.sched.relearn(sys::now(), crate::schedule::LEARN_DAYS, "manual");
                self.model.stable = false;
                self.hint = None;
                self.save_sched();
                self.apply(false);
                self.write_state();
                format!("ok relearning for {} days", crate::schedule::LEARN_DAYS)
            }
            ["learning"] => match self.sched.progress(sys::now()) {
                Some((d, n, why)) => format!("ok learning day {d}/{n} ({why}); predictions apply after day {n}"),
                None => "ok learned; adapting continuously, relearns for 7 days after an OS update".into(),
            },
            ["mode", m, rest @ ..] if power::valid_mode(m) => {
                let ac = self.src.as_ref().map_or(true, |s| s.ac);
                let keep = rest.first() == Some(&"keep");
                if keep || *m == "auto" {
                    if ac { self.conf.ac_mode = m.to_string() } else { self.conf.battery_mode = m.to_string() }
                    save_conf(&self.conf);
                    self.transient = None;
                } else {
                    self.transient = Some((m.to_string(), ac));
                }
                self.apply(true);
                format!("ok mode {} ({})", self.mode, self.reason)
            }
            ["policy", ac_m, bat_m] if power::valid_mode(ac_m) && power::valid_mode(bat_m) => {
                self.conf.ac_mode = ac_m.to_string();
                self.conf.battery_mode = bat_m.to_string();
                self.transient = None;
                save_conf(&self.conf);
                self.apply(true);
                format!("ok policy ac={ac_m} battery={bat_m}")
            }
            ["hint", m, secs, why @ ..] if power::valid_mode(m) && *m != "auto" => {
                let secs: u64 = secs.parse().unwrap_or(600).min(3600);
                let why = why.join(" ").chars().take(120).collect::<String>();
                self.hint = Some((m.to_string(), Instant::now() + Duration::from_secs(secs), why));
                self.apply(false);
                format!("ok hint ({} {})", self.mode, self.reason)
            }
            ["hint", "clear"] => {
                self.hint = None;
                self.apply(false);
                "ok".into()
            }
            ["charge", "smart"] => {
                self.conf.charge = Some("smart".into());
                self.charge.paused = false;
                self.charge.full_once = None;
                save_conf(&self.conf);
                self.charge.last_smart = 0; // decide now
                match self.smart_charge(true) {
                    Ok(m) => format!("ok charge smart ({m})"),
                    Err(e) => format!("error {e}"),
                }
            }
            ["charge", "full"] => match crate::charging::safe_set(&self.hw, &mut self.charge, "standard", None, None, sys::now()) {
                Ok(_) => {
                    self.charge.full_once = Some(sys::now());
                    self.charge.save();
                    sys::log_line(&log_path(), "charging → Standard until full (then back)");
                    self.write_state();
                    "ok charge full (100 % once, then back to your mode)".into()
                }
                Err(e) => {
                    self.charge.save();
                    format!("error {e}")
                }
            },
            ["charge", mode, rest @ ..] => {
                let s = rest.first().and_then(|x| x.parse().ok());
                let e = rest.get(1).and_then(|x| x.parse().ok());
                let r = crate::charging::safe_set(&self.hw, &mut self.charge, mode, s, e, sys::now());
                self.charge.save();
                match r {
                    Ok(written) => {
                        self.conf.charge = Some(mode.to_string());
                        self.charge.full_once = None;
                        self.charge.paused = false;
                        if *mode == "custom" {
                            self.conf.charge_start = s;
                            self.conf.charge_end = e;
                        }
                        save_conf(&self.conf);
                        if written {
                            sys::log_line(&log_path(), &format!("charging → {mode}"));
                        }
                        self.write_state();
                        format!("ok charge {mode}{}", if written { "" } else { " (already set)" })
                    }
                    Err(e) => format!("error {e}"),
                }
            }
            ["reapply"] => {
                self.hw = hw::detect();
                self.apply(true);
                "ok".into()
            }
            _ => "error unknown or invalid request".into(),
        }
    }
}

pub fn run() -> Result<(), String> {
    if !sys::is_root() && std::env::var_os("SHADOW_OPT_ROOT").is_none() {
        return Err("the daemon must run as root (shadow-powerd.service)".into());
    }
    std::fs::create_dir_all(sys::run_dir()).map_err(|e| e.to_string())?;
    let sp = sock_path();
    let _ = std::fs::remove_file(&sp);
    let listener = UnixListener::bind(&sp).map_err(|e| format!("{}: {e}", sp.display()))?;
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&sp, std::fs::Permissions::from_mode(0o666));
    }
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let mut st = State {
        hw: hw::detect(),
        conf: load_conf(),
        src: None,
        transient: None,
        hint: None,
        mode: String::new(),
        reason: String::new(),
        applied: power::Applied::default(),
        last_apply: Instant::now() - Duration::from_secs(3600),
        model: crate::predict::Model::load(&std::fs::read_to_string(learn_path()).unwrap_or_default()),
        last_learn: None,
        sched: crate::schedule::Schedule::load(&std::fs::read_to_string(sched_path()).unwrap_or_default(), sys::now(), &crate::schedule::os_fingerprint()),
        os_checked: None,
        pl: String::new(),
        charge: crate::charging::Guard::load(),
    };
    if let Some(m) = memory_governance() {
        sys::log_line(&log_path(), &m);
    }
    // telemetry from the compositor (datagrams; any local user may send, records are validated)
    let tel_path = sys::run_dir().join("telemetry.sock");
    let _ = std::fs::remove_file(&tel_path);
    let tel = std::os::unix::net::UnixDatagram::bind(&tel_path).ok();
    if let Some(t) = &tel {
        let _ = t.set_nonblocking(true);
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tel_path, std::fs::Permissions::from_mode(0o666));
    }
    sys::log_line(&log_path(), &format!("started on {} {}", st.hw.vendor, st.hw.product));
    // restore the chosen charging mode (firmware keeps it, but a BIOS reset may not)
    // — through the guard (no-op when already set, read back, write budget); "smart" is decided
    // by the daily smart pass instead
    if let Some(ch) = st.conf.charge.clone().filter(|c| c != "smart") {
        if st.charge.full_once.is_none() {
            if let Err(e) = crate::charging::safe_set(&st.hw, &mut st.charge, &ch, st.conf.charge_start, st.conf.charge_end, sys::now()) {
                sys::log_line(&log_path(), &format!("charging restore skipped: {e}"));
            }
            st.charge.save();
        }
    }
    let poll = Duration::from_millis(std::env::var("SHADOW_OPT_POLL_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(2000));
    let mut last_poll = Instant::now() - poll;
    let max_loops: Option<u64> = std::env::var("SHADOW_OPT_MAX_LOOPS").ok().and_then(|v| v.parse().ok());
    let mut loops = 0u64;
    let mut rapl_prev: Option<(u64, Instant)> = None;
    loop {
        if last_poll.elapsed() >= poll {
            last_poll = Instant::now();
            // package power: energy_uj is root-only (0400 since the Platypus fix), so the daemon
            // publishes it for the task manager and the bar
            if let Some(uj) = sys::read("sys/class/powercap/intel-rapl:0/energy_uj").and_then(|v| v.trim().parse::<u64>().ok()) {
                if let Some((p, t)) = rapl_prev {
                    let dt = t.elapsed().as_secs_f64();
                    if dt > 0.2 && uj >= p {
                        run_state("rapl.state", &format!("pkg_w={:.2}\n", (uj - p) as f64 / 1e6 / dt));
                    }
                }
                rapl_prev = Some((uj, Instant::now()));
            }
            let src = power::source(&st.hw);
            let changed = st.src.as_ref().map_or(true, |o| o.ac != src.ac
                || power::auto_mode(o) != power::auto_mode(&src));
            if st.src.as_ref().map_or(false, |o| o.ac != src.ac) {
                st.transient = None; // a manual pick lasts until the power source changes
                st.hint = None;      // learned hints are about the old source
                sys::log_line(&log_path(), if src.ac { "power: AC connected" } else { "power: on battery" });
            }
            let ac_now = src.ac;
            st.src = Some(src);
            st.apply(changed);
            // charging: AC history (persisted every ~10 min) and the once-a-day smart decision
            if st.hw.battery.is_some() && !st.hw.charge_types.is_empty() {
                st.charge.sample(ac_now, poll.as_secs().max(1), sys::now());
                if loops % 60 == 0 {
                    st.charge.save();
                }
                let _ = st.smart_charge(false);
            }
            if let Some((class, util, prior)) = st.model.tick(crate::predict::cpu_times()) {
                st.learned(class, util, prior);
            }
            st.check_schedule();
            loops += 1;
            if max_loops.map_or(false, |m| loops >= m) {
                return Ok(());
            }
        }
        if let Some(t) = &tel {
            let mut buf = [0u8; 64];
            while let Ok(n) = t.recv(&mut buf) {
                if let Some(r) = crate::predict::decode(&buf[..n]) {
                    st.telemetry(r);
                }
            }
        }
        match listener.accept() {
            Ok((stream, _)) => serve(&mut st, stream),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

fn serve(st: &mut State, stream: UnixStream) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut line = String::new();
    let mut r = BufReader::new(&stream);
    if r.read_line(&mut line).is_ok() && line.len() < 512 {
        let reply = st.handle(line.trim());
        let mut w = &stream;
        let _ = writeln!(w, "{reply}");
    }
}

/// Client side: one request, one reply line.
pub fn request(cmd: &str) -> Result<String, String> {
    let mut s = UnixStream::connect(sock_path())
        .map_err(|_| "the optimizer service is not running — sudo systemctl enable --now shadow-powerd.service".to_string())?;
    let _ = s.set_read_timeout(Some(Duration::from_secs(10)));
    writeln!(s, "{cmd}").map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(&s).read_line(&mut line).map_err(|e| e.to_string())?;
    let line = line.trim().to_string();
    match line.strip_prefix("error ") {
        Some(e) => Err(e.to_string()),
        None => Ok(line),
    }
}
