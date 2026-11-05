//! Session context for learning, from niri's public IPC (`niri msg --json`). shadow-displayd
//! switches the panel between 120/60 Hz; this module only asks niri what is focused and what
//! the panel runs at.
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

/// the focused app, from niri
pub fn focused_app() -> String {
    let (code, out, _) = sys::run(&["shadowctl", "niri", "--json", "focused-window"]);
    if code != 0 {
        return String::new();
    }
    let app = crate::json::parse(&out).ok().and_then(|v| v.path_str(&["app_id"]));
    app.unwrap_or_default().chars().filter(|c| c.is_ascii_alphanumeric() || "-_.".contains(*c)).take(40).collect()
}

/// the built-in panel's (connector, width, height, refresh mHz, all refreshes at that size)
pub fn panel() -> Option<(String, i64, i64, i64, Vec<i64>)> {
    let (code, out, _) = sys::run(&["shadowctl", "niri", "--json", "outputs"]);
    if code != 0 {
        return None;
    }
    let v = crate::json::parse(&out).ok()?;
    for (name, o) in v.entries() {
        let n = name.to_ascii_uppercase();
        if !(n.starts_with("EDP") || n.starts_with("LVDS") || n.starts_with("DSI")) {
            continue;
        }
        let modes = match o.get("modes") {
            Some(crate::json::Value::Arr(m)) => m.clone(),
            _ => continue,
        };
        let cur = o.get("current_mode").and_then(|c| c.num())? as usize;
        let m = modes.get(cur)?;
        let (w, h, r) = (m.get("width")?.num()? as i64, m.get("height")?.num()? as i64, m.get("refresh_rate")?.num()? as i64);
        let rates = modes.iter().filter(|x| x.get("width").and_then(|v| v.num()) == Some(w as f64) && x.get("height").and_then(|v| v.num()) == Some(h as f64)).filter_map(|x| x.get("refresh_rate").and_then(|v| v.num()).map(|r| r as i64)).collect();
        return Some((name.clone(), w, h, r, rates));
    }
    None
}

pub fn context() -> Ctx {
    let s = power::source(&hw::detect());
    Ctx { ac: s.ac, battery_pct: s.battery_pct, app: focused_app(), hour: sys::hour() }
}

/// current panel refresh in Hz
pub fn compositor_refresh() -> Option<u32> {
    panel().map(|p| (p.3 as f64 / 1000.0).round() as u32)
}
