//! State directory (~/.config/shadow-state) and line-preserving access to config.toml, shared by
//! the CLI tools. Only the small TOML subset the tools need: [section] / key = value.
use std::path::PathBuf;

pub struct Paths {
    pub state: PathBuf,
    pub share: PathBuf,
}

pub fn paths() -> Paths {
    Paths { state: local_state(), share: share() }
}

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| "/".into()))
}

/// ~/.config/shadow-state: config.toml, secrets.age, npu_weights.bin, healed_patches/ (synced to GitHub)
pub fn state_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("SHADOW_STATE").filter(|v| !v.is_empty()) {
        return d.into();
    }
    std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| home().join(".config")).join("shadow-state")
}

/// ~/.local/state/shadow-tui: logs, learned Wi-Fi fixes, caches that are not synced
pub fn local_state() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| home().join(".local/state")).join("shadow-tui")
}

pub fn share() -> PathBuf {
    std::env::var_os("SHADOW_SHARE").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/opt/shadow-tui/share"))
}

pub fn config_file() -> PathBuf {
    state_dir().join("config.toml")
}

/// raw value of `key` in `[section]` (strings unquoted)
pub fn get(section: &str, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(config_file()).ok()?;
    get_in(&text, section, key)
}

pub fn get_in(text: &str, section: &str, key: &str) -> Option<String> {
    let mut cur = String::new();
    for l in text.lines() {
        let t = l.trim();
        if t.starts_with('[') && t.ends_with(']') {
            cur = t.trim_matches(|c| c == '[' || c == ']').trim().to_string();
            continue;
        }
        if cur != section || t.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = t.split_once('=') {
            if k.trim().trim_matches('"') == key {
                let v = v.split(" #").next().unwrap_or("").trim();
                return Some(if v.starts_with('"') && v.ends_with('"') && v.len() >= 2 { v[1..v.len() - 1].replace("\\\"", "\"").replace("\\\\", "\\") } else { v.to_string() });
            }
        }
    }
    None
}

pub fn get_bool(section: &str, key: &str, default: bool) -> bool {
    match get(section, key).as_deref() {
        Some("true") => true,
        Some("false") => false,
        _ => default,
    }
}

pub fn q(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// set `key = literal` in `[section]`, keeping every other line (and comments) as they are
pub fn set_in(text: &str, section: &str, key: &str, literal: &str) -> String {
    let mut out: Vec<String> = vec![];
    let mut cur = String::new();
    let mut done = false;
    let mut section_seen = false;
    let lines: Vec<&str> = text.lines().collect();
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim();
        if t.starts_with('[') && t.ends_with(']') {
            if cur == section && !done {
                // end of our section without the key: append it here
                while out.last().map_or(false, |x: &String| x.trim().is_empty()) {
                    out.pop();
                }
                out.push(format!("{key} = {literal}"));
                out.push(String::new());
                done = true;
            }
            cur = t.trim_matches(|c| c == '[' || c == ']').trim().to_string();
            section_seen |= cur == section;
        } else if cur == section && !done && !t.starts_with('#') {
            if let Some((k, _)) = t.split_once('=') {
                if k.trim().trim_matches('"') == key {
                    out.push(format!("{key} = {literal}"));
                    done = true;
                    continue;
                }
            }
        }
        out.push(l.to_string());
        let _ = i;
    }
    if !done {
        if !section_seen {
            if out.last().map_or(false, |x| !x.trim().is_empty()) {
                out.push(String::new());
            }
            out.push(format!("[{section}]"));
        }
        out.push(format!("{key} = {literal}"));
    }
    out.join("\n") + "\n"
}

pub fn set(section: &str, key: &str, literal: &str) -> std::io::Result<()> {
    let p = config_file();
    let text = std::fs::read_to_string(&p).unwrap_or_default();
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    crate::sys::atomic_write(&p, &set_in(&text, section, key, literal))
}

pub fn notify(summary: &str, body: &str, urgency: &str) {
    if crate::sys::which("notify-send").is_some() && std::env::var_os("WAYLAND_DISPLAY").is_some() {
        crate::sys::run(&["notify-send", "-a", "Shadow TUI", "-u", urgency, summary, body]);
    }
}

pub fn spawn_detached(cmd: &[&str]) {
    use std::os::unix::process::CommandExt;
    if let Some((c, rest)) = cmd.split_first() {
        let _ = std::process::Command::new(c).args(rest).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).process_group(0).spawn();
    }
}

/// /run/shadow/<name> key=value state file read by the HUD (best effort)
pub fn write_run_state(name: &str, kv: &[(&str, String)]) {
    let dir = PathBuf::from(std::env::var("SHADOW_RUN_STATE").unwrap_or_else(|_| "/run/shadow".into()));
    let _ = std::fs::create_dir_all(&dir);
    let body: String = kv.iter().map(|(k, v)| format!("{k}={}\n", v.replace('\n', " "))).collect();
    let _ = crate::sys::atomic_write(&dir.join(name), &body);
}

#[cfg(test)]
mod t {
    use super::*;
    #[test]
    fn toml_subset() {
        let t = "# top\n[layout]\npresets = [45, 65, 85]\n\n[healer]\nmode = \"approve\" # comment\n\n[look]\nscale = 1.5\n";
        assert_eq!(get_in(t, "healer", "mode").as_deref(), Some("approve"));
        assert_eq!(get_in(t, "look", "scale").as_deref(), Some("1.5"));
        assert_eq!(get_in(t, "look", "nope"), None);
        let t2 = set_in(t, "healer", "mode", &q("auto"));
        assert_eq!(get_in(&t2, "healer", "mode").as_deref(), Some("auto"));
        assert!(t2.starts_with("# top\n[layout]\npresets = [45, 65, 85]"));
        let t3 = set_in(&t2, "healer", "notify", "true");
        assert_eq!(get_in(&t3, "healer", "notify").as_deref(), Some("true"));
        assert_eq!(get_in(&t3, "look", "scale").as_deref(), Some("1.5"));
        let t4 = set_in(&t3, "wifi", "self_correct", "false");
        assert!(t4.ends_with("[wifi]\nself_correct = false\n"));
        assert_eq!(get_in("", "x", "y"), None);
    }
}
