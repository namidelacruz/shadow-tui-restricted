//! The web shell's API (10.0): what the taskbar, start menu, widgets and quick settings pages read
//! and do. Everything goes through fixed verbs: windows only by id, apps only by desktop-file id,
//! settings only through the validated values below — no command line from a page is ever run.
use shadow_apps::{cmd_out, spawn};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// the compositor's view: windows, screens, workspaces
pub fn state() -> Result<serde_json::Value, String> {
    let o = shadow_niri::compositor_command("shell")?;
    serde_json::from_str(&o).map_err(|e| format!("compositor: {e}"))
}

fn num(v: &serde_json::Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

/// what the shell may ask the compositor to do
const ACTIONS: &[&str] = &[
    "maximize", "unmaximize", "minimize", "restore", "close", "fullscreen", "focus-next", "focus-prev", "workspace-next", "workspace-prev",
    "move-to-workspace-next", "move-to-workspace-prev", "move-to-screen-next", "move-to-screen-prev", "focus-screen-next", "focus-screen-prev",
    "tutorial", "promote",
];

pub fn act(j: &serde_json::Value) -> Result<serde_json::Value, String> {
    let ok = |r: Result<String, String>| r.map(|m| serde_json::json!({ "ok": m }));
    match j["do"].as_str().unwrap_or("") {
        "focus" | "minimize" | "close" => {
            let id = num(&j["id"]).ok_or("which window?")?;
            ok(shadow_niri::compositor_command(&format!("{} {id}", j["do"].as_str().unwrap())))
        }
        // the taskbar button: focus it, or minimise it when it already has the keyboard
        "toggle" => {
            let id = num(&j["id"]).ok_or("which window?")?;
            let st = state()?;
            let w = st["windows"].as_array().and_then(|a| a.iter().find(|w| w["id"].as_u64() == Some(id)).cloned()).ok_or("no such window")?;
            let verb = if w["focused"].as_bool() == Some(true) && w["visible"].as_bool() == Some(true) && w["minimized"].as_bool() != Some(true) { "minimize" } else { "focus" };
            ok(shadow_niri::compositor_command(&format!("{verb} {id}")))
        }
        "workspace" => {
            let (s, n) = (num(&j["screen"]).ok_or("which screen?")?, num(&j["n"]).ok_or("which workspace?")?);
            ok(shadow_niri::compositor_command(&format!("workspace {s} {n}")))
        }
        "action" => {
            let a = j["action"].as_str().unwrap_or("");
            if !ACTIONS.contains(&a) {
                return Err(format!("not a shell action: {a}"));
            }
            ok(shadow_niri::compositor_command(&format!("action {a}")))
        }
        // where the next window unfolds from (the clicked button's rect, global logical px)
        "origin" => {
            let v: Vec<f64> = j["rect"].as_array().map(|a| a.iter().filter_map(|x| x.as_f64()).collect()).unwrap_or_default();
            if v.len() != 4 || v.iter().any(|x| !x.is_finite()) {
                return Err("origin needs [x, y, w, h]".into());
            }
            ok(shadow_niri::compositor_command(&format!("origin {} {} {} {}", v[0], v[1], v[2], v[3])))
        }
        other => Err(format!("unknown shell request: {other}")),
    }
}

/// installed apps for the start menu: id, name, glyph, bilingual label, pinned
pub fn apps() -> serde_json::Value {
    let pins = pinned();
    let mut v: Vec<serde_json::Value> = shadow_apps::desktop::load_apps()
        .into_iter()
        .filter(|e| !e.name.is_empty())
        .map(|e| {
            // the launcher's ids carry an "app:" tag; the shell uses the desktop-file name
            let id = e.id.trim_start_matches("app:").to_string();
            let g = shadow_ui::glyphs::for_app(&format!("{} {}", id, e.name));
            serde_json::json!({
                "id": id, "name": e.name, "detail": e.detail, "cats": e.cats, "keywords": e.keywords,
                "glyph": format!("{g:?}").to_ascii_lowercase(), "jp": shadow_ui::glyphs::jp(g), "app_ids": e.app_ids,
                "pinned": pins.iter().position(|p| *p == id),
            })
        })
        .collect();
    v.sort_by(|a, b| a["name"].as_str().unwrap_or("").to_lowercase().cmp(&b["name"].as_str().unwrap_or("").to_lowercase()));
    serde_json::Value::Array(v)
}

fn pins_file() -> std::path::PathBuf {
    shadow_apps::state_dir().join("pinned.list")
}

/// pinned apps (taskbar + start menu), in order
pub fn pinned() -> Vec<String> {
    match std::fs::read_to_string(pins_file()) {
        Ok(t) => t.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect(),
        Err(_) => ["shadow-term.desktop", "shadow-files.desktop", "shadow-settings.desktop", "firefox.desktop"].iter().map(|s| s.to_string()).collect(),
    }
}

pub fn pin(j: &serde_json::Value) -> Result<serde_json::Value, String> {
    let id = j["id"].as_str().unwrap_or("");
    if !valid_id(id) {
        return Err("not an app id".into());
    }
    let mut p = pinned();
    p.retain(|x| x != id);
    if j["pin"].as_bool().unwrap_or(true) {
        p.push(id.to_string());
    }
    shadow_apps::atomic_write(&pins_file(), &(p.join("\n") + "\n")).map_err(|e| e.to_string())?;
    Ok(serde_json::json!({ "ok": "saved" }))
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() < 200 && id.ends_with(".desktop") && id.chars().all(|c| c.is_ascii_alphanumeric() || "._-+@".contains(c))
}

/// start an installed app by its desktop-file id (its own Exec line, as the launcher does)
pub fn launch(j: &serde_json::Value) -> Result<serde_json::Value, String> {
    let id = j["id"].as_str().unwrap_or("");
    if !valid_id(id) {
        return Err("not an app id".into());
    }
    if j["rect"].is_array() {
        let _ = act(&serde_json::json!({ "do": "origin", "rect": j["rect"] }));
    }
    let e = shadow_apps::desktop::load_apps().into_iter().find(|e| e.id.trim_start_matches("app:") == id).ok_or("that app is not installed")?;
    let from = shadow_apps::focused_app();
    shadow_apps::desktop::record_launch(&e.id, &from);
    if e.terminal {
        spawn(&shadow_apps::terminal_exec(&e.cmd));
    } else {
        spawn(&e.cmd);
    }
    Ok(serde_json::json!({ "ok": format!("started {}", e.name) }))
}

/// lock, log out, restart, shut down, sleep
pub fn power(j: &serde_json::Value) -> Result<serde_json::Value, String> {
    let r = match j["what"].as_str().unwrap_or("") {
        "lock" => shadow_niri::compositor_command("lock"),
        "logout" => shadow_niri::compositor_command("action quit"),
        "sleep" => Ok(cmd_out(&["systemctl", "suspend"]).1),
        "restart" => Ok(cmd_out(&["systemctl", "reboot"]).1),
        "shutdown" => Ok(cmd_out(&["systemctl", "poweroff"]).1),
        w => return Err(format!("unknown power action: {w}")),
    };
    r.map(|m| serde_json::json!({ "ok": m }))
}

// ── the tray and quick settings: battery, network, sound, brightness, radios, updates ─────────

static STATUS: Mutex<Option<(Instant, serde_json::Value)>> = Mutex::new(None);

fn first_line(args: &[&str]) -> String {
    let (c, o) = cmd_out(args);
    if c == 0 { o.lines().next().unwrap_or("").trim().to_string() } else { String::new() }
}

fn battery() -> serde_json::Value {
    let Ok(rd) = std::fs::read_dir("/sys/class/power_supply") else { return serde_json::Value::Null };
    let mut ac = false;
    let mut bat: Option<(i64, String)> = None;
    for e in rd.flatten() {
        let p = e.path();
        let rd = |f: &str| std::fs::read_to_string(p.join(f)).unwrap_or_default().trim().to_string();
        match rd("type").as_str() {
            "Mains" | "USB" => ac |= rd("online") == "1",
            "Battery" if bat.is_none() => bat = rd("capacity").parse().ok().map(|c| (c, rd("status"))),
            _ => {}
        }
    }
    match bat {
        Some((pct, st)) => serde_json::json!({ "percent": pct, "charging": st == "Charging" || st == "Full", "ac": ac }),
        None => serde_json::json!({ "percent": null, "ac": true }),
    }
}

fn network() -> serde_json::Value {
    // nmcli -t: the active connection and the Wi-Fi radio
    let radio = first_line(&["nmcli", "-t", "radio", "wifi"]);
    let mut wifi = serde_json::Value::Null;
    let (_, o) = cmd_out(&["nmcli", "-t", "-f", "ACTIVE,SSID,SIGNAL", "dev", "wifi"]);
    for l in o.lines() {
        let f: Vec<&str> = l.splitn(3, ':').collect();
        if f.len() == 3 && f[0] == "yes" {
            wifi = serde_json::json!({ "ssid": f[1], "signal": f[2].parse::<i64>().unwrap_or(0) });
        }
    }
    let (_, o) = cmd_out(&["nmcli", "-t", "-f", "TYPE,STATE", "dev"]);
    let wired = o.lines().any(|l| l.starts_with("ethernet:connected"));
    let airplane = first_line(&["nmcli", "-t", "radio", "all"]).split(':').all(|x| x == "disabled");
    serde_json::json!({ "wifi_on": radio == "enabled", "wifi": wifi, "wired": wired, "airplane": airplane })
}

fn sound() -> serde_json::Value {
    // "Volume: 0.45 [MUTED]"
    let l = first_line(&["wpctl", "get-volume", "@DEFAULT_AUDIO_SINK@"]);
    let vol = l.split_whitespace().nth(1).and_then(|v| v.parse::<f64>().ok());
    serde_json::json!({ "volume": vol.map(|v| (v * 100.0).round() as i64), "muted": l.contains("MUTED") })
}

fn brightness() -> serde_json::Value {
    // brightnessctl -m: name,class,current,percent,max
    let l = first_line(&["brightnessctl", "-m"]);
    let pct = l.split(',').nth(3).and_then(|p| p.trim_end_matches('%').parse::<i64>().ok());
    serde_json::json!(pct)
}

fn bluetooth() -> serde_json::Value {
    if shadow_apps::cmd_out(&["sh", "-c", "command -v bluetoothctl"]).0 != 0 {
        return serde_json::Value::Null;
    }
    let (_, o) = cmd_out(&["bluetoothctl", "show"]);
    if o.trim().is_empty() {
        return serde_json::Value::Null;
    }
    serde_json::json!({ "on": o.lines().any(|l| l.trim() == "Powered: yes") })
}

fn update() -> serde_json::Value {
    let (c, o) = cmd_out(&["shadow", "update", "ready"]);
    if c == 0 {
        let mut it = o.split_whitespace();
        let (head, n, ver) = (it.next().unwrap_or(""), it.next().unwrap_or("0"), it.next().unwrap_or(""));
        serde_json::json!({ "ready": true, "commit": &head[..8.min(head.len())], "files": n.parse::<i64>().unwrap_or(0), "version": ver })
    } else {
        serde_json::json!({ "ready": false })
    }
}

/// every tray value, cached for 2 s (several taskbars ask at once)
pub fn status() -> serde_json::Value {
    if let Some((t, v)) = STATUS.lock().unwrap().as_ref() {
        if t.elapsed() < Duration::from_secs(2) {
            return v.clone();
        }
    }
    let v = serde_json::json!({
        "battery": battery(), "network": network(), "sound": sound(), "brightness": brightness(), "bluetooth": bluetooth(), "update": update(),
        "host": std::fs::read_to_string("/etc/hostname").unwrap_or_default().trim(),
        "user": std::env::var("USER").unwrap_or_default(),
    });
    *STATUS.lock().unwrap() = Some((Instant::now(), v.clone()));
    v
}

fn forget_status() {
    *STATUS.lock().unwrap() = None;
}

/// quick settings changes, each a fixed command with a validated value
pub fn quick(j: &serde_json::Value) -> Result<serde_json::Value, String> {
    let on = j["on"].as_bool();
    let pct = j["value"].as_f64().map(|v| v.clamp(0.0, 100.0).round() as i64);
    let run = |a: &[&str]| -> Result<String, String> {
        let (c, o) = cmd_out(a);
        if c == 0 { Ok("ok".into()) } else { Err(o.lines().last().unwrap_or("failed").trim().to_string()) }
    };
    let r = match j["what"].as_str().unwrap_or("") {
        "wifi" => run(&["nmcli", "radio", "wifi", if on.unwrap_or(true) { "on" } else { "off" }]),
        "airplane" => run(&["nmcli", "radio", "all", if on.unwrap_or(true) { "off" } else { "on" }]),
        "bluetooth" => run(&["bluetoothctl", "power", if on.unwrap_or(true) { "on" } else { "off" }]),
        "volume" => match pct {
            Some(p) => run(&["wpctl", "set-volume", "-l", "1.0", "@DEFAULT_AUDIO_SINK@", &format!("{p}%")]),
            None => Err("volume needs a value".into()),
        },
        "mute" => run(&["wpctl", "set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"]),
        "brightness" => match pct {
            Some(p) => run(&["brightnessctl", "set", &format!("{}%", p.max(1))]),
            None => Err("brightness needs a value".into()),
        },
        // the Shadow apps the quick settings open
        "open" => {
            let what = j["app"].as_str().unwrap_or("");
            let cmd = match what {
                "network" => "shadow-net",
                "settings" => "shadow-settings settings",
                "display" => "shadow-settings look",
                "update" => "shadow-update --install-ready",
                "updates" => "shadow-update",
                "tasks" => "shadow-tasks",
                "files" => "shadow-files",
                "terminal" => "shadow-term",
                "fix" => "shadow-settings fix",
                "power" => "shadow-settings settings",
                _ => return Err(format!("unknown app: {what}")),
            };
            spawn(cmd);
            Ok("opened".into())
        }
        w => Err(format!("unknown quick setting: {w}")),
    };
    forget_status();
    r.map(|m| serde_json::json!({ "ok": m }))
}

#[cfg(test)]
mod t {
    #[test]
    fn only_app_ids_and_fixed_actions_get_through() {
        assert!(super::valid_id("firefox.desktop") && super::valid_id("org.gnome.Nautilus.desktop"));
        for bad in ["", "x", "../../etc/passwd.desktop", "a b.desktop", "$(rm).desktop", "a;b.desktop"] {
            assert!(!super::valid_id(bad), "{bad}");
        }
        assert!(super::act(&serde_json::json!({ "do": "action", "action": "spawn rm -rf /" })).is_err());
        assert!(super::act(&serde_json::json!({ "do": "origin", "rect": [1, 2, 3] })).is_err());
        assert!(super::act(&serde_json::json!({ "do": "nope" })).is_err());
        assert!(super::power(&serde_json::json!({ "what": "rm" })).is_err());
        assert!(super::quick(&serde_json::json!({ "what": "open", "app": "sh -c x" })).is_err());
    }
}
