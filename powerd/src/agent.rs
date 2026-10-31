//! Session context for learning. The compositor owns the display now (it switches 120/60 Hz
//! itself through a DRM mode change), so this module only asks it what is focused, and refresh
//! commands go through `shadowctl`.
use crate::hw;
use crate::learn::Ctx;
use crate::power;
use crate::sys;

fn config_value(section: &str, key: &str) -> Option<String> {
    let t = std::fs::read_to_string(sys::config_dir().join("config.toml")).ok()?;
    let mut cur = String::new();
    for l in t.lines() {
        let l = l.trim();
        if l.starts_with('[') {
            cur = l.trim_matches(|c| c == '[' || c == ']').to_string();
        } else if cur == section {
            if let Some((k, v)) = l.split_once('=') {
                if k.trim() == key {
                    return Some(v.split('#').next().unwrap_or("").trim().trim_matches('"').to_string());
                }
            }
        }
    }
    None
}

pub fn learning_on() -> bool {
    config_value("power", "learning").map_or(true, |v| v != "false")
}

/// the centre-stage app, from the compositor
pub fn focused_app() -> String {
    let (code, out, _) = sys::run(&["shadowctl", "state"]);
    if code != 0 {
        return String::new();
    }
    let center = crate::json::parse(&out).ok().and_then(|v| v.get("center").and_then(|c| c.num()));
    let app = crate::json::parse(&out).ok().and_then(|v| match (v.get("windows"), center) {
        (Some(crate::json::Value::Arr(w)), Some(c)) => w.iter().find(|x| x.get("id").and_then(|i| i.num()) == Some(c)).and_then(|x| x.path_str(&["app_id"])),
        _ => None,
    });
    app.unwrap_or_default().chars().filter(|c| c.is_ascii_alphanumeric() || "-_.".contains(*c)).take(40).collect()
}

pub fn context() -> Ctx {
    let s = power::source(&hw::detect());
    Ctx { ac: s.ac, battery_pct: s.battery_pct, app: focused_app(), hour: sys::hour() }
}

/// current refresh as the compositor reports it
pub fn compositor_refresh() -> Option<u32> {
    let (code, out, _) = sys::run(&["shadowctl", "state"]);
    if code != 0 {
        return None;
    }
    crate::json::parse(&out).ok().and_then(|v| v.get("refresh_hz").and_then(|r| r.num())).map(|r| r as u32)
}
