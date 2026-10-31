//! Persistent bezel HUD data. Read every 2 s from sysfs and from small state files the daemons
//! write in /run/shadow (net.state, power.state, audio.state, npu.state, healer.state, sec.alert),
//! so the compositor never runs a subprocess for it. Only changed text is re-rasterised.
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

fn audio() -> String {
    if let Some(s) = kv("run/shadow/audio.state", "status") {
        return s.to_uppercase();
    }
    match rd("proc/asound/cards") {
        Some(c) if c.contains("sof") => "SOF".into(),
        Some(c) if !c.contains("no soundcards") && !c.is_empty() => "ALSA".into(),
        _ => "NO DSP".into(),
    }
}

fn wifi() -> String {
    let ssid = kv("run/shadow/net.state", "ssid").unwrap_or_default();
    let eap = kv("run/shadow/net.state", "eap").unwrap_or_else(|| "-".into());
    let dbm = kv("run/shadow/net.state", "dbm").or_else(|| {
        // /proc/net/wireless: "wlan0: 0000   54.  -56.  -256 ..."
        rd("proc/net/wireless")?.lines().nth(2).and_then(|l| l.split_whitespace().nth(3).map(|v| v.trim_end_matches('.').to_string()))
    });
    if ssid.is_empty() {
        return "WIFI —".into();
    }
    format!("{ssid} {}dBm {eap}", dbm.unwrap_or_else(|| "?".into()))
}

/// IDLE / LEARNING / PATCHING / SEC-ALERT, most urgent first
pub fn npu_state() -> String {
    if root().join("run/shadow/sec.alert").exists() {
        return "SEC-ALERT".into();
    }
    if kv("run/shadow/healer.state", "state").as_deref() == Some("patching") {
        return "PATCHING".into();
    }
    kv("run/shadow/npu.state", "state").map(|s| s.to_uppercase()).unwrap_or_else(|| "IDLE".into())
}

fn sync_state() -> String {
    let f = crate::config::state_dir().join(".sync-status");
    match std::fs::read_to_string(f).ok().and_then(|t| t.split_whitespace().next().map(String::from)).as_deref() {
        Some("ok") => "SYNC ✓".into(),
        Some("dirty") => "SYNC ●".into(),
        Some("error") => "SYNC ✗".into(),
        _ => "SYNC —".into(),
    }
}

pub struct Snapshot {
    pub top_left: String,
    pub top_right: String,
    pub bottom: String,
    pub power: Power,
}

pub fn snapshot(ring_id: usize, ring_n: usize, focused: &str, refresh_hz: u32, extra: &[String]) -> Snapshot {
    let pw = power();
    let bat = match (pw.battery, pw.watts) {
        (Some(b), Some(w)) if !pw.ac || pw.charging => format!("BAT {b}% {}{w:.1}W", if pw.charging { "+" } else { "-" }),
        (Some(b), _) => format!("BAT {b}% AC"),
        (None, _) => "AC".into(),
    };
    let temp = package_temp().map(|t| format!("{t}°C")).unwrap_or_else(|| "--°C".into());
    let mut tr = vec![format!("NPU {}", npu_state()), sync_state()];
    tr.extend(extra.iter().cloned());
    Snapshot {
        top_left: format!("◢ SHADOW//RING {:02}/{:02}  ▸ {}", ring_id, ring_n, if focused.is_empty() { "—" } else { focused }),
        top_right: tr.join("  │  "),
        bottom: format!("{bat}  │  {refresh_hz}Hz  │  PKG {temp} {}  │  DSP {}  │  {}", profile().to_uppercase(), audio(), wifi()),
        power: pw,
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
        let s = super::snapshot(2, 4, "shadow-term", 60, &[]);
        assert_eq!(s.bottom, "BAT 73% -6.4W  │  60Hz  │  PKG 48°C QUIET  │  DSP NO DSP  │  UC-Secure -61dBm PEAP/TLS1.2 OK");
        assert!(s.top_right.starts_with("NPU PATCHING"));
        assert!(s.top_left.contains("RING 02/04") && s.top_left.contains("shadow-term"));
        assert!(!s.power.ac);
        std::fs::write(r.join("run/shadow/sec.alert"), "x").unwrap();
        assert_eq!(super::npu_state(), "SEC-ALERT");
        std::env::remove_var("SHADOW_SYS_ROOT");
    }
}
