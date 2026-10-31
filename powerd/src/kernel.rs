//! Kernel guard: keeps an Arch update from silently breaking this laptop.
//!
//! * a known-bad list (/opt/shadow-tui/share/hw/known-bad.conf, plus /etc/shadow-tui/
//!   known-bad.conf for your own entries) of kernel ranges with regressions on this hardware
//! * `check-upgrade` runs from a pacman hook before a kernel is installed: if the new version is
//!   known-bad for this machine it warns, and refuses only when there is no good fallback kernel
//! * `ensure-fallback` installs linux-lts and adds a boot entry, so there is always a kernel to
//!   go back to.
use crate::hw::Hw;
use crate::sys;
use std::io::Read;

pub const KERNELS: [&str; 4] = ["linux", "linux-lts", "linux-zen", "linux-hardened"];

pub fn parse_ver(s: &str) -> Vec<u32> {
    // "6.17.8.arch1-1", "6.17.8-arch1-1", "6.12.50-1-lts", "1:6.1" -> [6,17,8]
    let s = s.split_once(':').map(|(_, r)| r).unwrap_or(s);
    s.split(|c: char| !c.is_ascii_digit() && c != '.')
        .next()
        .unwrap_or("")
        .split('.')
        .filter(|p| !p.is_empty())
        .take(3)
        .filter_map(|p| p.parse().ok())
        .collect()
}

fn cmp(a: &[u32], b: &[u32]) -> std::cmp::Ordering {
    let n = a.len().max(b.len());
    for i in 0..n {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x.cmp(&y);
        }
    }
    std::cmp::Ordering::Equal
}

pub fn running_version() -> String {
    sys::read("proc/sys/kernel/osrelease").unwrap_or_default()
}

pub fn running_package() -> &'static str {
    let r = running_version();
    if r.ends_with("-lts") { "linux-lts" } else if r.contains("-zen") { "linux-zen" } else if r.contains("-hardened") { "linux-hardened" } else { "linux" }
}

#[derive(Debug, Clone)]
pub struct Rule {
    pub area: String,
    pub hw: String,
    pub pkg: String,
    pub from: Vec<u32>,
    pub to: Option<Vec<u32>>,
    pub why: String,
}

pub fn rules() -> Vec<Rule> {
    let mut out = vec![];
    for f in ["opt/shadow-tui/share/hw/known-bad.conf", "etc/shadow-tui/known-bad.conf"] {
        for l in sys::read(f).unwrap_or_default().lines() {
            let l = l.trim();
            if l.is_empty() || l.starts_with('#') {
                continue;
            }
            // five whitespace-separated fields (any amount of padding), then free-text description
            let mut rest = l;
            let mut w: Vec<&str> = vec![];
            while w.len() < 5 {
                rest = rest.trim_start();
                if rest.is_empty() {
                    break;
                }
                let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
                w.push(&rest[..end]);
                rest = &rest[end..];
            }
            if w.len() < 5 {
                continue;
            }
            out.push(Rule {
                area: w[0].into(),
                hw: w[1].into(),
                pkg: w[2].into(),
                from: parse_ver(w[3]),
                to: (w[4] != "-").then(|| parse_ver(w[4])),
                why: rest.trim().to_string(),
            });
        }
    }
    out
}

fn hw_matches(tag: &str, h: &Hw) -> bool {
    match tag {
        "any" => true,
        "lunar-lake" => h.lunar_lake,
        "dell" => h.dell,
        "dell-14-plus" => h.dell_14_plus,
        _ => false,
    }
}

/// Reason if `ver` of `pkg` is known-bad for this hardware (area "*" = any area).
pub fn bad_for(h: &Hw, area: &str, pkg: &str, ver: &str) -> Option<String> {
    let v = parse_ver(ver);
    if v.is_empty() {
        return None;
    }
    rules().into_iter().find_map(|r| {
        let area_ok = area == "*" || r.area == area;
        let pkg_ok = r.pkg == "*" || r.pkg == pkg;
        let in_range = cmp(&v, &r.from) != std::cmp::Ordering::Less
            && r.to.as_ref().map_or(true, |t| cmp(&v, t) == std::cmp::Ordering::Less);
        (area_ok && pkg_ok && hw_matches(&r.hw, h) && in_range).then(|| format!("[{}] {}", r.area, r.why))
    })
}

pub fn installed() -> Vec<(String, String)> {
    KERNELS
        .iter()
        .filter_map(|k| {
            let (code, out, _) = sys::run(&["pacman", "-Q", k]);
            (code == 0).then(|| (k.to_string(), out.split_whitespace().nth(1).unwrap_or("").to_string()))
        })
        .collect()
}

pub fn bootloader() -> &'static str {
    if sys::exists("boot/loader/entries") || sys::exists("efi/loader/entries") {
        "systemd-boot"
    } else if sys::exists("boot/grub/grub.cfg") {
        "grub"
    } else if sys::exists("boot/limine.conf") || sys::exists("boot/limine/limine.conf") {
        "limine"
    } else {
        "unknown"
    }
}

pub fn status(h: &Hw) {
    let run = running_version();
    println!("running kernel : {run}{}", bad_for(h, "*", running_package(), &run).map(|w| format!("   ⚠ {w}")).unwrap_or_default());
    for (k, v) in installed() {
        let flag = bad_for(h, "*", &k, &v).map(|w| format!("   ⚠ known-bad: {w}")).unwrap_or_else(|| "   ok".into());
        println!("installed      : {k} {v}{flag}");
    }
    println!("bootloader     : {}", bootloader());
    println!("fallback       : {}", if has_good_fallback(h, None) { "yes" } else { "none — run: sudo shadow-powerd kernel ensure-fallback" });
}

/// Is there an installed kernel (other than `except`) that is not known-bad?
pub fn has_good_fallback(h: &Hw, except: Option<&str>) -> bool {
    installed().iter().any(|(k, v)| Some(k.as_str()) != except && bad_for(h, "*", k, v).is_none())
}

/// pacman PreTransaction hook (NeedsTargets: package names on stdin). Exit 1 aborts the update.
pub fn check_upgrade(h: &Hw) -> i32 {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let mut code = 0;
    for pkg in input.split_whitespace().filter(|p| KERNELS.contains(p)) {
        let (_, info, _) = sys::run(&["pacman", "-Si", pkg]);
        let Some(new) = info.lines().find(|l| l.starts_with("Version")).and_then(|l| l.split(':').nth(1)).map(|s| s.trim().to_string()) else { continue };
        let Some(why) = bad_for(h, "*", pkg, &new) else { continue };
        println!("Shadow TUI: {pkg} {new} has a known regression on this laptop:");
        println!("    {why}");
        if sys::exists("etc/shadow-tui/allow-kernel") {
            println!("    continuing: /etc/shadow-tui/allow-kernel is present");
        } else if has_good_fallback(h, Some(pkg)) {
            println!("    continuing: a good fallback kernel is installed — pick it at boot if this one misbehaves");
        } else {
            println!("    STOPPED: no good fallback kernel is installed, so this update could leave you without it.");
            println!("    Fix:      sudo shadow-powerd kernel ensure-fallback   (installs linux-lts + boot entry), then update again");
            println!("    Override: sudo touch /etc/shadow-tui/allow-kernel");
            code = 1;
        }
    }
    code
}

/// systemd-boot entry for linux-lts made from an existing entry (kernel cmdline/root kept).
pub fn make_sdboot_entry(existing: &str) -> Option<String> {
    if !existing.lines().any(|l| l.trim_start().starts_with("linux") && l.contains("vmlinuz-linux") && !l.contains("vmlinuz-linux-")) {
        return None;
    }
    let mut out = vec![];
    for l in existing.lines() {
        let t = l.trim_start();
        if t.starts_with("title") {
            out.push("title   Shadow TUI fallback (linux-lts)".to_string());
        } else if t.starts_with("linux") {
            out.push(l.replace("vmlinuz-linux", "vmlinuz-linux-lts"));
        } else if t.starts_with("initrd") && l.contains("initramfs-linux") {
            out.push(l.replace("initramfs-linux-fallback.img", "initramfs-linux-lts-fallback.img")
                .replace("initramfs-linux.img", "initramfs-linux-lts.img"));
        } else if t.starts_with("sort-key") || t.starts_with("version") {
            continue;
        } else {
            out.push(l.to_string());
        }
    }
    Some(out.join("\n") + "\n")
}

pub fn ensure_fallback(h: &Hw) -> Result<String, String> {
    if !sys::is_root() && std::env::var_os("SHADOW_OPT_ROOT").is_none() {
        return Err("run as root: sudo shadow-powerd kernel ensure-fallback".into());
    }
    let mut done = vec![];
    let lts = installed().into_iter().find(|(k, _)| k == "linux-lts");
    match lts {
        Some((_, v)) if bad_for(h, "*", "linux-lts", &v).is_some() => {
            return Err(format!("linux-lts {v} is itself known-bad here; keep your current kernel"));
        }
        Some(_) => {}
        None => {
            let (code, _, err) = sys::run(&["pacman", "-S", "--needed", "--noconfirm", "linux-lts"]);
            if code != 0 {
                return Err(format!("could not install linux-lts: {}", err.lines().last().unwrap_or("")));
            }
            done.push("installed linux-lts".to_string());
        }
    }
    match bootloader() {
        "systemd-boot" => {
            let dir = if sys::exists("boot/loader/entries") { "boot" } else { "efi" };
            let entries = sys::ls(&format!("{dir}/loader/entries"));
            let has_lts = entries.iter().any(|e| sys::read(&format!("{dir}/loader/entries/{e}")).unwrap_or_default().contains("vmlinuz-linux-lts"));
            if has_lts {
                done.push("systemd-boot already has a linux-lts entry".into());
            } else if !sys::exists(&format!("{dir}/vmlinuz-linux-lts")) {
                return Err(format!("linux-lts is installed but /{dir}/vmlinuz-linux-lts is missing (kernel images live elsewhere?) — add the boot entry by hand"));
            } else {
                let base = entries.iter().find_map(|e| make_sdboot_entry(&sys::read(&format!("{dir}/loader/entries/{e}")).unwrap_or_default()));
                let Some(text) = base else {
                    return Err("no existing linux entry to copy the kernel command line from — add the boot entry by hand".into());
                };
                sys::write(&format!("{dir}/loader/entries/shadow-fallback-lts.conf"), &text)?;
                done.push(format!("added boot entry /{dir}/loader/entries/shadow-fallback-lts.conf"));
            }
        }
        "grub" => {
            let (code, _, err) = sys::run(&["grub-mkconfig", "-o", &sys::p("boot/grub/grub.cfg").to_string_lossy()]);
            if code != 0 {
                return Err(format!("grub-mkconfig failed: {err}"));
            }
            done.push("regenerated GRUB menu (linux-lts under Advanced options)".into());
        }
        other => done.push(format!("bootloader {other}: add a linux-lts entry yourself (images: /boot/vmlinuz-linux-lts, /boot/initramfs-linux-lts.img)")),
    }
    Ok(done.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn versions() {
        assert_eq!(parse_ver("6.17.8.arch1-1"), vec![6, 17, 8]);
        assert_eq!(parse_ver("6.12.50-1-lts"), vec![6, 12, 50]);
        assert_eq!(parse_ver("6.18-arch1"), vec![6, 18]);
        assert_eq!(cmp(&parse_ver("6.18"), &parse_ver("6.18.0")), std::cmp::Ordering::Equal);
        assert_eq!(cmp(&parse_ver("6.18.3"), &parse_ver("6.17.8")), std::cmp::Ordering::Greater);
    }
    #[test]
    fn sdboot_entry() {
        let e = "title Arch Linux\nlinux /vmlinuz-linux\ninitrd /intel-ucode.img\ninitrd /initramfs-linux.img\noptions root=UUID=abc rw quiet\n";
        let n = make_sdboot_entry(e).unwrap();
        assert!(n.contains("linux /vmlinuz-linux-lts"));
        assert!(n.contains("initrd /initramfs-linux-lts.img"));
        assert!(n.contains("initrd /intel-ucode.img"));
        assert!(n.contains("options root=UUID=abc rw quiet"));
        assert!(make_sdboot_entry("title x\nlinux /vmlinuz-linux-zen\n").is_none());
    }
}

#[cfg(test)]
mod rule_tests {
    use super::*;
    #[test]
    fn padded_rules_parse() {
        let root = std::env::temp_dir().join(format!("sn-rules-{}", std::process::id()));
        std::fs::create_dir_all(root.join("opt/shadow-tui/share/hw")).unwrap();
        std::fs::write(root.join("opt/shadow-tui/share/hw/known-bad.conf"),
            "# c\naudio    lunar-lake    *        6.17.8    6.18.3   Lunar Lake audio (Dummy Output)\n").unwrap();
        std::env::set_var("SHADOW_OPT_ROOT", &root);
        let r = rules();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].why, "Lunar Lake audio (Dummy Output)");
        let h = Hw { lunar_lake: true, ..Default::default() };
        assert!(bad_for(&h, "audio", "linux", "6.17.9.arch1-1").is_some());
        assert!(bad_for(&h, "audio", "linux", "6.18.3.arch1-1").is_none());
        assert!(bad_for(&h, "audio", "linux", "6.17.7.arch1-1").is_none());
        let other = Hw::default();
        assert!(bad_for(&other, "audio", "linux", "6.17.9").is_none());
        let _ = std::fs::remove_dir_all(root);
    }
}
