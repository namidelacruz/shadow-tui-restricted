//! Things a session needs so that privileged actions can ask for a password instead of failing.
//!
//! * A polkit authentication agent. pkexec, systemd (reboot past an inhibitor), NetworkManager
//!   (connections for all users on distributions that require it, e.g. RHEL) and Shadow's own
//!   Wi-Fi helpers authorise through polkit. Without an agent in the session there is nobody to
//!   show the password prompt, and those actions fail with "no authentication agent found".
//!   niri doesn't ship one, so shadow-sessiond starts the first one installed.
//! * Brightness without root: systemd-logind's Session.SetBrightness, which the user at the
//!   active seat may call on every distribution (no udev group, no setuid helper).
use std::path::Path;

/// (process name, paths) of the agents we know, best first. Fedora/RHEL put them in
/// /usr/libexec, Arch in /usr/lib/<package>.
pub const AGENTS: &[(&str, &[&str])] = &[
    ("polkit-mate-aut", &["/usr/libexec/polkit-mate-authentication-agent-1", "/usr/lib/mate-polkit/polkit-mate-authentication-agent-1"]),
    ("lxqt-policykit-", &["/usr/bin/lxqt-policykit-agent"]),
    ("polkit-gnome-au", &["/usr/libexec/polkit-gnome-authentication-agent-1", "/usr/lib/polkit-gnome/polkit-gnome-authentication-agent-1"]),
    ("lxpolkit", &["/usr/bin/lxpolkit", "/usr/libexec/lxpolkit"]),
    ("xfce-polkit", &["/usr/libexec/xfce-polkit", "/usr/lib/xfce-polkit/xfce-polkit"]),
    ("polkit-kde-auth", &["/usr/libexec/kf6/polkit-kde-authentication-agent-1", "/usr/lib/polkit-kde-authentication-agent-1", "/usr/libexec/polkit-kde-authentication-agent-1"]),
];

/// The installed agent we would start.
pub fn agent_path() -> Option<&'static str> {
    AGENTS.iter().flat_map(|(_, ps)| ps.iter()).copied().find(|p| Path::new(p).is_file())
}

/// An agent of ours or anyone else's already running for this user.
pub fn agent_running() -> Option<String> {
    let uid = unsafe_uid();
    let rd = std::fs::read_dir("/proc").ok()?;
    for e in rd.flatten() {
        if !e.file_name().to_string_lossy().chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let comm = std::fs::read_to_string(e.path().join("comm")).unwrap_or_default();
        let comm = comm.trim();
        if !AGENTS.iter().any(|(n, _)| comm.starts_with(n)) && !comm.contains("polkit-agent") && !comm.contains("authentication-agent") {
            continue;
        }
        let owner = std::fs::read_to_string(e.path().join("status")).ok().and_then(|s| s.lines().find_map(|l| l.strip_prefix("Uid:")).and_then(|u| u.split_whitespace().next()?.parse::<u32>().ok()));
        if owner == Some(uid) {
            return Some(comm.to_string());
        }
    }
    None
}

fn unsafe_uid() -> u32 {
    extern "C" {
        fn getuid() -> u32;
    }
    // SAFETY: getuid has no preconditions
    unsafe { getuid() }
}

/// Start the agent if none runs. Returns a line for the log.
pub fn ensure_agent() -> String {
    if let Some(a) = agent_running() {
        return format!("polkit agent running ({a})");
    }
    match agent_path() {
        Some(p) => {
            crate::spawn(p);
            format!("polkit agent started: {p}")
        }
        None => "NO polkit authentication agent installed: actions that need a password (Wi-Fi fixes, reboot past an inhibitor, some network changes) will fail. Install one: mate-polkit (Fedora/RHEL/Arch) or lxqt-policykit".into(),
    }
}

// ── brightness ─────────────────────────────────────────────────────────────────────────────────

/// The backlight to drive: firmware/platform interfaces before raw ones.
pub fn backlight(root: &Path) -> Option<(String, u32, u32)> {
    let dir = root.join("sys/class/backlight");
    let mut v: Vec<(u8, String, u32, u32)> = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            let r = |f: &str| std::fs::read_to_string(e.path().join(f)).ok()?.trim().parse::<u32>().ok();
            let rank = match std::fs::read_to_string(e.path().join("type")).unwrap_or_default().trim() {
                "firmware" => 0,
                "platform" => 1,
                _ => 2,
            };
            Some((rank, n, r("brightness")?, r("max_brightness")?.max(1)))
        })
        .collect();
    v.sort();
    v.into_iter().next().map(|(_, n, c, m)| (n, c, m))
}

/// "5%+", "5%-", "40%": the new raw value (never fully dark: 1 % floor)
pub fn target(cur: u32, max: u32, spec: &str) -> Option<u32> {
    let s = spec.trim();
    let (num, op) = if let Some(n) = s.strip_suffix("%+") { (n, '+') } else if let Some(n) = s.strip_suffix("%-") { (n, '-') } else { (s.trim_end_matches('%'), '=') };
    let pct: f64 = num.parse().ok()?;
    let step = (max as f64 * pct / 100.0).round() as i64;
    let v = match op {
        '+' => cur as i64 + step.max(1),
        '-' => cur as i64 - step.max(1),
        _ => step,
    };
    let floor = ((max as f64) * 0.01).ceil() as i64;
    Some(v.clamp(floor.max(1), max as i64) as u32)
}

/// Set the brightness: logind (no root) → brightnessctl → direct write. Returns the new percent.
pub fn set_brightness(spec: &str) -> Result<u32, String> {
    let (dev, cur, max) = backlight(Path::new("/")).ok_or("no backlight device")?;
    let v = target(cur, max, spec).ok_or_else(|| format!("bad value '{spec}' (use 5%+, 5%- or 40%)"))?;
    let pct = (v as f64 * 100.0 / max as f64).round() as u32;
    let logind = std::process::Command::new("busctl")
        .args(["call", "--quiet", "org.freedesktop.login1", "/org/freedesktop/login1/session/auto", "org.freedesktop.login1.Session", "SetBrightness", "ssu", "backlight", &dev, &v.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if logind {
        return Ok(pct);
    }
    let bc = std::process::Command::new("brightnessctl").args(["-q", "-d", &dev, "set", &v.to_string()]).status().map(|s| s.success()).unwrap_or(false);
    if bc {
        return Ok(pct);
    }
    std::fs::write(format!("/sys/class/backlight/{dev}/brightness"), v.to_string()).map(|_| pct).map_err(|e| format!("cannot set brightness ({e}); logind and brightnessctl failed too"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brightness_steps() {
        assert_eq!(target(500, 1000, "5%+"), Some(550));
        assert_eq!(target(30, 1000, "5%-"), Some(10), "never below 1 %");
        assert_eq!(target(500, 1000, "40%"), Some(400));
        assert_eq!(target(990, 1000, "5%+"), Some(1000));
        assert_eq!(target(0, 1000, "bad"), None);
    }

    #[test]
    fn picks_the_firmware_backlight() {
        let r = std::env::temp_dir().join(format!("bl-{}", std::process::id()));
        for (n, t, c) in [("acpi_video0", "firmware", "7"), ("intel_backlight", "raw", "300")] {
            let d = r.join("sys/class/backlight").join(n);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("type"), t).unwrap();
            std::fs::write(d.join("brightness"), c).unwrap();
            std::fs::write(d.join("max_brightness"), "10").unwrap();
        }
        assert_eq!(backlight(&r).map(|b| b.0), Some("acpi_video0".into()));
        let _ = std::fs::remove_dir_all(&r);
    }
}
