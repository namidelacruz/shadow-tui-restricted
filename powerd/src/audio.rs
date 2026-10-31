//! Audio doctor for Intel Core Ultra Series 2 (Lunar Lake): SOF DSP + SoundWire codecs
//! (Realtek RT7xx / Cirrus CS42L43 + CS35L56 amps). "Dummy Output" usually means one of:
//! missing SOF firmware/topology, missing UCM profiles, missing codec/amp firmware,
//! a kernel with a known audio regression, or the session audio server not running.
use crate::hw::{self, Hw};
use crate::kernel;
use crate::sys;

#[derive(Debug, Default)]
pub struct Report {
    pub problems: Vec<(String, String)>, // (code, text)
    pub info: Vec<String>,
    pub root_pkgs: Vec<String>,          // packages to install
    pub kernel_bad: Option<String>,
    pub sinks: Vec<String>,
    pub dummy: bool,
}

fn cards() -> Vec<String> {
    sys::read("proc/asound/cards")
        .unwrap_or_default()
        .lines()
        .filter(|l| l.contains(" [") && l.contains("]:"))
        .map(|l| l.split("]:").nth(1).unwrap_or("").trim().to_string())
        .collect()
}

fn pkg_installed(p: &str) -> bool {
    sys::run(&["pacman", "-Q", p]).0 == 0
}

fn pkg_available(p: &str) -> bool {
    sys::run(&["pacman", "-Si", p]).0 == 0
}

/// Output sinks known to PipeWire/WirePlumber (wpctl status, "Sinks:" under "Audio").
pub fn sinks() -> Option<Vec<String>> {
    let (code, out, _) = sys::run(&["wpctl", "status"]);
    if code != 0 {
        return None;
    }
    let mut in_audio = false;
    let mut in_sinks = false;
    let mut v = vec![];
    for l in out.lines() {
        let t = l.trim_start_matches(|c: char| c == '│' || c == '├' || c == '└' || c == '─' || c.is_whitespace());
        if l.starts_with("Audio") {
            in_audio = true;
            continue;
        }
        if l.starts_with("Video") || l.starts_with("Settings") {
            in_audio = false;
        }
        if !in_audio {
            continue;
        }
        if t.starts_with("Sinks:") {
            in_sinks = true;
            continue;
        }
        if t.ends_with(':') && !t.is_empty() {
            in_sinks = false;
        }
        if in_sinks && !t.is_empty() {
            // "*   48. Speaker                             [vol: 0.40]"
            let name = t.trim_start_matches('*').trim();
            let name = name.split_once(". ").map(|(_, n)| n).unwrap_or(name);
            let name = name.split("[vol").next().unwrap_or(name).trim();
            if !name.is_empty() {
                v.push(name.to_string());
            }
        }
    }
    Some(v)
}

pub fn diagnose(h: &Hw) -> Report {
    let mut r = Report::default();
    let mut prob = |code: &str, text: String| r.problems.push((code.into(), text));
    match &h.audio_pci {
        None => {
            prob("no-controller", "no Intel audio controller found on PCI".into());
            return r;
        }
        Some(a) => r.info.push(format!("controller {a}, driver {}", if h.audio_driver.is_empty() { "none" } else { &h.audio_driver })),
    }
    if h.audio_driver.is_empty() {
        prob("no-driver", "no kernel driver bound to the audio controller".into());
    }
    if !h.codecs.is_empty() {
        let c: Vec<String> = h.codecs.iter().map(hw::codec_name).collect();
        r.info.push(format!("SoundWire codecs: {}", c.join(", ")));
    }
    let sof_fw = ["usr/lib/firmware/intel/sof-ipc4/lnl", "usr/lib/firmware/intel/sof-ace-tplg", "usr/lib/firmware/intel/sof-ipc4-tplg"]
        .iter()
        .any(|d| !sys::ls(d).is_empty());
    if h.lunar_lake && !sof_fw {
        prob("sof-firmware", "Sound Open Firmware for Lunar Lake is missing (package sof-firmware)".into());
        r.root_pkgs.push("sof-firmware".into());
    }
    if sys::ls("usr/share/alsa/ucm2").is_empty() {
        prob("ucm", "ALSA UCM profiles are missing (package alsa-ucm-conf)".into());
        r.root_pkgs.push("alsa-ucm-conf".into());
    }
    if h.codecs.iter().any(|c| c.vendor == "01fa") && sys::ls("usr/lib/firmware/cirrus").is_empty() {
        prob("cirrus-fw", "Cirrus Logic amplifier firmware is missing".into());
        r.root_pkgs.push(if pkg_available("linux-firmware-cirrus") { "linux-firmware-cirrus" } else { "linux-firmware" }.into());
    }
    let cs = cards();
    if cs.is_empty() {
        prob("no-card", "the kernel created no sound card".into());
    } else {
        r.info.push(format!("sound cards: {}", cs.join(" | ")));
    }
    let running = kernel::running_version();
    if let Some(why) = kernel::bad_for(h, "audio", "linux", &running) {
        r.kernel_bad = Some(why.clone());
        prob("kernel", format!("kernel {running} has a known audio regression on this hardware: {why}"));
    }
    // session side (only meaningful as the user)
    if !sys::is_root() {
        for pkg in ["pipewire", "wireplumber", "pipewire-pulse"] {
            if sys::which("pacman").is_some() && !pkg_installed(pkg) {
                r.root_pkgs.push(pkg.into());
            }
        }
        match sinks() {
            None => prob("no-server", "PipeWire/WirePlumber is not running in this session".into()),
            Some(s) => {
                r.dummy = s.is_empty() || s.iter().all(|n| n.contains("Dummy"));
                if r.dummy {
                    prob("dummy", "only \"Dummy Output\" is available".into());
                }
                r.sinks = s;
            }
        }
    }
    r.root_pkgs.sort();
    r.root_pkgs.dedup();
    r
}

pub fn print(r: &Report) {
    for i in &r.info {
        println!("  · {i}");
    }
    if !r.sinks.is_empty() {
        println!("  · outputs: {}", r.sinks.join(", "));
    }
    if r.problems.is_empty() {
        println!("audio: OK");
    }
    for (_, t) in &r.problems {
        println!("  ! {t}");
    }
}

/// Fix what can be fixed. Root part: packages. User part: restart the audio server.
pub fn fix(h: &Hw) -> bool {
    let r = diagnose(h);
    print(&r);
    if r.problems.is_empty() {
        return true;
    }
    if !r.root_pkgs.is_empty() {
        let mut args = vec!["pacman", "-S", "--needed", "--noconfirm"];
        args.extend(r.root_pkgs.iter().map(|s| s.as_str()));
        let cmd: Vec<&str> = if sys::is_root() { args.clone() } else {
            let mut c = vec![if sys::which("pkexec").is_some() { "pkexec" } else { "sudo" }];
            c.extend(args.iter());
            c
        };
        println!("→ installing: {}", r.root_pkgs.join(" "));
        let (code, out, err) = sys::run(&cmd);
        if code != 0 {
            println!("  install failed: {}", err.lines().last().unwrap_or(&out));
        } else {
            println!("  installed — a reboot loads new firmware");
        }
    }
    if let Some(why) = &r.kernel_bad {
        println!("→ this kernel breaks audio here ({why}).");
        println!("  Boot the fallback kernel (shadow-powerd kernel ensure-fallback installs it), or update past the fixed version.");
    }
    if !sys::is_root() {
        println!("→ restarting the audio server (PipeWire + WirePlumber)");
        sys::run(&["systemctl", "--user", "restart", "wireplumber.service", "pipewire.service", "pipewire-pulse.service"]);
        std::thread::sleep(std::time::Duration::from_millis(
            std::env::var("SHADOW_OPT_SETTLE_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(2500)));
        let after = diagnose(h);
        if after.problems.is_empty() {
            println!("audio: OK — outputs: {}", after.sinks.join(", "));
            return true;
        }
        if after.dummy && after.kernel_bad.is_none() {
            println!("  still Dummy Output: reboot once (new firmware loads at boot); if it persists, keep the output of `shadow-powerd audio`");
        }
    }
    false
}
