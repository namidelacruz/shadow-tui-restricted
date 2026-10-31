//! System information for the side bar. Read every 2 s from sysfs and from the small state files
//! the daemons write in /run/shadow (net.state, power.state, audio.state, npu.state, healer.state,
//! sec.alert) — no subprocesses, and the bar only repaints when a value actually changes.
use std::path::PathBuf;

fn root() -> PathBuf {
    std::env::var_os("SHADOW_SYS_ROOT").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}
fn rd(rel: &str) -> Option<String> {
    std::fs::read_to_string(root().join(rel.trim_start_matches('/'))).ok().map(|s| s.trim().to_string())
}
fn ls(rel: &str) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(root().join(rel.trim_start_matches('/')))
        .map(|r| r.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    v.sort();
    v
}
/// key=value state file
pub fn kv(rel: &str, key: &str) -> Option<String> {
    rd(rel)?.lines().find_map(|l| l.strip_prefix(&format!("{key}=")).map(|v| v.trim().to_string()))
}

#[derive(Default, Clone, PartialEq, Debug)]
pub struct Power {
    pub ac: bool,
    pub battery: Option<u8>,
    pub watts: Option<f64>,
    pub charging: bool,
}

pub fn power() -> Power {
    let mut p = Power::default();
    for s in ls("sys/class/power_supply") {
        let b = format!("sys/class/power_supply/{s}");
        match rd(&format!("{b}/type")).as_deref() {
            Some("Mains") => p.ac |= rd(&format!("{b}/online")).as_deref() == Some("1"),
            Some("Battery") if p.battery.is_none() => {
                p.battery = rd(&format!("{b}/capacity")).and_then(|c| c.parse().ok());
                p.charging = rd(&format!("{b}/status")).as_deref() == Some("Charging");
                let uw = rd(&format!("{b}/power_now")).and_then(|v| v.parse::<f64>().ok()).or_else(|| {
                    let i = rd(&format!("{b}/current_now"))?.parse::<f64>().ok()?;
                    let v = rd(&format!("{b}/voltage_now"))?.parse::<f64>().ok()?;
                    Some(i * v / 1e6)
                });
                p.watts = uw.map(|w| w / 1e6);
            }
            _ => {}
        }
    }
    p
}

pub fn package_temp() -> Option<i32> {
    let zones = ls("sys/class/thermal");
    let pick = zones.iter().find(|z| rd(&format!("sys/class/thermal/{z}/type")).as_deref() == Some("x86_pkg_temp"))
        .or_else(|| zones.iter().find(|z| z.starts_with("thermal_zone")))?;
    rd(&format!("sys/class/thermal/{pick}/temp"))?.parse::<i32>().ok().map(|m| m / 1000)
}

pub fn profile() -> String {
    rd("sys/firmware/acpi/platform_profile").unwrap_or_else(|| "n/a".into())
}

pub fn audio() -> String {
    if let Some(s) = kv("run/shadow/audio.state", "status") {
        return s.to_uppercase();
    }
    match rd("proc/asound/cards") {
        Some(c) if c.contains("sof") => "SOF".into(),
        Some(c) if !c.contains("no soundcards") && !c.is_empty() => "ALSA".into(),
        _ => "NO DSP".into(),
    }
}

#[derive(Default, Clone, PartialEq, Debug)]
pub struct Wifi {
    pub ssid: String,
    pub dbm: Option<String>,
    pub eap: String,
}

pub fn wifi() -> Wifi {
    let ssid = kv("run/shadow/net.state", "ssid").unwrap_or_default();
    let eap = kv("run/shadow/net.state", "eap").unwrap_or_default();
    let dbm = kv("run/shadow/net.state", "dbm").filter(|d| !d.is_empty()).or_else(|| {
        // /proc/net/wireless: "wlan0: 0000   54.  -56.  -256 ..."
        rd("proc/net/wireless")?.lines().nth(2).and_then(|l| l.split_whitespace().nth(3).map(|v| v.trim_end_matches('.').to_string()))
    });
    Wifi { ssid, dbm, eap }
}

/// IDLE / LEARNING / PATCHING / SEC-ALERT, most urgent first
pub fn npu_state() -> String {
    if root().join("run/shadow/sec.alert").exists() {
        return "SEC-ALERT".into();
    }
    if kv("run/shadow/healer.state", "state").as_deref() == Some("patching") {
        return "PATCHING".into();
    }
    let st = kv("run/shadow/npu.state", "state").map(|s| s.to_uppercase()).unwrap_or_else(|| "IDLE".into());
    match (st.as_str(), kv("run/shadow/npu.state", "day"), kv("run/shadow/npu.state", "days")) {
        ("LEARNING", Some(d), Some(n)) => format!("LEARNING {d}/{n}"),
        _ => st,
    }
}

/// why the model is (re)learning: first-week / os-update / drift
pub fn npu_reason() -> Option<String> {
    kv("run/shadow/npu.state", "reason").filter(|r| !r.is_empty())
}

pub fn sync_state() -> String {
    let f = shadow_ui::theme::state_dir().join(".sync-status");
    match std::fs::read_to_string(f).ok().and_then(|t| t.split_whitespace().next().map(String::from)).as_deref() {
        Some("ok") => "✓ synced".into(),
        Some("dirty") => "● changes".into(),
        Some("error") => "✗ failed".into(),
        _ => "—".into(),
    }
}

/// refresh rate the power daemon asks for (power.state refresh=60|120), if any
pub fn wanted_refresh() -> Option<u32> {
    kv("run/shadow/power.state", "refresh").and_then(|v| v.parse().ok())
}

#[cfg(test)]
mod t {
    #[test]
    fn fake_sysfs() {
        let r = std::env::temp_dir().join(format!("hud-{}", std::process::id()));
        let w = |p: &str, v: &str| {
            let f = r.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, v).unwrap();
        };
        w("sys/class/power_supply/AC/type", "Mains");
        w("sys/class/power_supply/AC/online", "0");
        w("sys/class/power_supply/BAT0/type", "Battery");
        w("sys/class/power_supply/BAT0/capacity", "73");
        w("sys/class/power_supply/BAT0/status", "Discharging");
        w("sys/class/power_supply/BAT0/power_now", "6400000");
        w("sys/class/thermal/thermal_zone3/type", "x86_pkg_temp");
        w("sys/class/thermal/thermal_zone3/temp", "48000");
        w("sys/firmware/acpi/platform_profile", "quiet");
        w("run/shadow/net.state", "ssid=UC-Secure\ndbm=-61\neap=PEAP/TLS1.2 OK\n");
        w("run/shadow/healer.state", "state=patching\n");
        std::env::set_var("SHADOW_SYS_ROOT", &r);
        let p = super::power();
        assert_eq!((p.ac, p.battery, p.charging), (false, Some(73), false));
        assert!((p.watts.unwrap() - 6.4).abs() < 1e-9);
        assert_eq!(super::package_temp(), Some(48));
        assert_eq!(super::profile(), "quiet");
        let wf = super::wifi();
        assert_eq!((wf.ssid.as_str(), wf.dbm.as_deref(), wf.eap.as_str()), ("UC-Secure", Some("-61"), "PEAP/TLS1.2 OK"));
        assert_eq!(super::npu_state(), "PATCHING");
        std::fs::remove_file(r.join("run/shadow/healer.state")).unwrap();
        w("run/shadow/npu.state", "state=learning\nday=3\ndays=7\nreason=os-update\n");
        assert_eq!(super::npu_state(), "LEARNING 3/7");
        assert_eq!(super::npu_reason().as_deref(), Some("os-update"));
        std::fs::write(r.join("run/shadow/sec.alert"), "x").unwrap();
        assert_eq!(super::npu_state(), "SEC-ALERT");
        std::env::remove_var("SHADOW_SYS_ROOT");
    }
}
