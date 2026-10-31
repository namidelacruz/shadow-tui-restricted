//! Power state and the knobs Dell Optimizer turns, done through the kernel:
//! thermal/fan mode (dell-pc platform_profile), CPU energy preference (intel_pstate EPP),
//! battery charging mode and thresholds (dell-laptop).
use crate::hw::Hw;
use crate::sys;

#[derive(Debug, Clone, PartialEq)]
pub struct Source {
    pub ac: bool,
    pub battery_pct: Option<u8>,
}

pub fn source(h: &Hw) -> Source {
    let ac = match &h.ac_supply {
        Some(s) => sys::read(&format!("sys/class/power_supply/{s}/online")).as_deref() == Some("1"),
        None => {
            // USB-C only machines: no Mains supply node; fall back to the battery status
            h.battery.as_ref().map_or(true, |b| {
                sys::read(&format!("sys/class/power_supply/{b}/status")).as_deref() != Some("Discharging")
            })
        }
    };
    let battery_pct = h.battery.as_ref().and_then(|b| sys::read(&format!("sys/class/power_supply/{b}/capacity"))?.parse().ok());
    Source { ac, battery_pct }
}

/// Shadow TUI modes (Dell Optimizer names in brackets).
pub const MODES: [(&str, &str); 6] = [
    ("auto", "adaptive: follows power source, battery level and what it has learned"),
    ("performance", "Ultra Performance — fans may run, maximum clocks"),
    ("balanced", "Optimized — Dell's default balance"),
    ("quiet", "Quiet — lower fan noise, reduced clocks"),
    ("cool", "Cool — keeps the bottom of the laptop cooler"),
    ("low-power", "battery saver — lowest power"),
];

pub fn valid_mode(m: &str) -> bool {
    MODES.iter().any(|(k, _)| *k == m)
}

/// Pick the closest thermal mode the firmware actually offers.
pub fn resolve_profile(want: &str, choices: &[String]) -> Option<String> {
    let chain: &[&str] = match want {
        "performance" => &["performance", "balanced-performance", "balanced"],
        "balanced" => &["balanced"],
        "quiet" => &["quiet", "low-power", "cool", "balanced"],
        "cool" => &["cool", "quiet", "balanced"],
        "low-power" => &["low-power", "quiet", "cool", "balanced"],
        _ => &["balanced"],
    };
    chain.iter().find(|c| choices.iter().any(|x| x == *c)).map(|s| s.to_string())
}

pub fn resolve_epp(want: &str, ac: bool, choices: &[String]) -> Option<String> {
    let chain: &[&str] = match (want, ac) {
        ("performance", _) => &["performance", "balance_performance"],
        ("balanced", true) => &["balance_performance", "default"],
        ("balanced", false) => &["balance_power", "default"],
        ("quiet" | "cool", _) => &["balance_power", "power"],
        ("low-power", _) => &["power", "balance_power"],
        _ => &["default", "balance_performance"],
    };
    chain.iter().find(|c| choices.iter().any(|x| x == *c)).map(|s| s.to_string())
}

/// What `auto` means right now (before learning is applied).
pub fn auto_mode(src: &Source) -> &'static str {
    if src.ac {
        return "balanced";
    }
    match src.battery_pct {
        Some(p) if p < 20 => "low-power",
        Some(p) if p < 50 => "quiet",
        _ => "balanced",
    }
}

/// Another power manager that would fight us over the same knobs.
pub fn other_manager() -> Option<&'static str> {
    for (unit, name) in [("tlp.service", "TLP"), ("auto-cpufreq.service", "auto-cpufreq"), ("tuned.service", "tuned")] {
        if sys::svc_active(unit) {
            return Some(name);
        }
    }
    None
}

pub fn ppd_active() -> bool {
    sys::svc_active("power-profiles-daemon.service")
}

#[derive(Debug, Default)]
pub struct Applied {
    pub profile: Option<String>,
    pub epp: Option<String>,
    pub via: String,
    pub notes: Vec<String>,
}

/// Apply a mode. Root only (sysfs writes), except through power-profiles-daemon.
pub fn apply_mode(h: &Hw, mode: &str, src: &Source) -> Applied {
    let mut a = Applied::default();
    if let Some(m) = other_manager() {
        a.via = format!("left to {m}");
        a.notes.push(format!("{m} manages power here; Shadow TUI does not override it (disable {m} to use these modes)"));
        return a;
    }
    if ppd_active() {
        // power-profiles-daemon owns platform_profile + EPP; talk to it instead of fighting it
        let ppd = match mode {
            "performance" => "performance",
            "quiet" | "cool" | "low-power" => "power-saver",
            _ => "balanced",
        };
        a.via = "power-profiles-daemon".into();
        if sys::run(&["powerprofilesctl", "get"]).1 == ppd {
            a.profile = Some(ppd.into());
            return a;
        }
        let (code, _, err) = sys::run(&["powerprofilesctl", "set", ppd]);
        if code == 0 {
            a.profile = Some(ppd.into());
        } else {
            a.notes.push(format!("powerprofilesctl failed: {err}"));
        }
        return a;
    }
    a.via = "kernel".into();
    if let (Some(path), Some(p)) = (&h.profile_path, resolve_profile(mode, &h.profile_choices)) {
        if sys::read(path).as_deref() != Some(&p) {
            if let Err(e) = sys::write(path, &p) {
                a.notes.push(format!("thermal mode not set: {e}"));
            }
        }
        a.profile = sys::read(path);
    } else if h.profile_path.is_none() {
        a.notes.push("no thermal modes exposed (needs the dell-pc driver, kernel 6.11+)".into());
    }
    if let Some(epp) = resolve_epp(mode, src.ac, &h.epp_choices) {
        let mut ok = 0;
        for pol in sys::ls("sys/devices/system/cpu/cpufreq") {
            let f = format!("sys/devices/system/cpu/cpufreq/{pol}/energy_performance_preference");
            if !sys::exists(&f) {
                continue;
            }
            if sys::read(&f).as_deref() == Some(&epp) || sys::write(&f, &epp).is_ok() {
                ok += 1;
            }
        }
        if ok > 0 {
            a.epp = Some(epp);
        } else {
            a.notes.push("CPU energy preference not writable (performance governor?)".into());
        }
    }
    a
}

// ---------------------------------------------------------------- charging
/// Friendly name -> kernel charge_types token.
pub fn charge_token(mode: &str, available: &[String]) -> Option<String> {
    let want: &[&str] = match mode {
        "standard" => &["Standard"],
        "adaptive" => &["Adaptive"],
        "express" => &["Fast"],
        "ac" => &["Long_Life", "Long Life"],
        "custom" => &["Custom"],
        "trickle" => &["Trickle"],
        _ => &[],
    };
    want.iter().find(|w| available.iter().any(|a| a == *w)).map(|s| s.to_string())
}

pub fn charge_current(h: &Hw) -> Option<String> {
    let b = h.battery.as_ref()?;
    let raw = sys::read(&format!("sys/class/power_supply/{b}/charge_types"))
        .or_else(|| sys::read(&format!("sys/class/power_supply/{b}/charge_type")))?;
    // "Fast [Standard] Adaptive Custom" or a single value
    raw.split_whitespace()
        .find(|w| w.starts_with('['))
        .map(|w| w.trim_matches(|c| c == '[' || c == ']').to_string())
        .or_else(|| (!raw.contains(' ')).then_some(raw))
}

pub fn thresholds(h: &Hw) -> Option<(u8, u8)> {
    let b = h.battery.as_ref()?;
    let s = sys::read(&format!("sys/class/power_supply/{b}/charge_control_start_threshold"))?.parse().ok()?;
    let e = sys::read(&format!("sys/class/power_supply/{b}/charge_control_end_threshold"))?.parse().ok()?;
    Some((s, e))
}

pub fn set_charge(h: &Hw, mode: &str, start: Option<u8>, end: Option<u8>) -> Result<String, String> {
    let b = h.battery.as_ref().ok_or("no battery")?;
    let tok = charge_token(mode, &h.charge_types)
        .ok_or_else(|| format!("charge mode '{mode}' is not offered by this firmware (available: {})", h.charge_types.join(" ")))?;
    if mode == "custom" {
        let (s, e) = (start.ok_or("custom needs START and END")?, end.ok_or("custom needs START and END")?);
        // Dell firmware limits: start 50-95, end 55-100, at least 5 apart
        if !(50..=95).contains(&s) || !(55..=100).contains(&e) || e < s + 5 {
            return Err("custom range must be START 50-95, END 55-100, END at least START+5".into());
        }
        if !h.charge_thresholds {
            return Err("this firmware does not expose custom thresholds".into());
        }
        sys::write(&format!("sys/class/power_supply/{b}/charge_control_end_threshold"), &e.to_string())?;
        sys::write(&format!("sys/class/power_supply/{b}/charge_control_start_threshold"), &s.to_string())?;
    }
    let file = if sys::exists(&format!("sys/class/power_supply/{b}/charge_types")) { "charge_types" } else { "charge_type" };
    sys::write(&format!("sys/class/power_supply/{b}/{file}"), &tok)?;
    Ok(tok)
}
