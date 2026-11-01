//! shadow-passfix — "my password doesn't work": find out why, and fix it.
//!
//! A plain text console, so it works from a text console (Ctrl+Alt+F2) when you can't get into the
//! desktop, and from the Fix center. It checks, in order of how often they're the cause:
//!   · pam_faillock lockout (3 wrong passwords lock the account for 10 minutes on Arch/Fedora/RHEL
//!     — and keep failing even with the right password)
//!   · Caps Lock / Num Lock
//!   · keyboard layout: the text console and login screen use the console keymap, the desktop
//!     and lock screen the XKB layout — a password with symbols typed on one may differ on the other
//!   · PAM files for the lock screen and login screen exist and include stacks that exist
//!   · expired password / account (account stage of PAM)
//!   · administrator rights (wheel) for sudo-based repairs
//!   · fingerprint-first PAM stacks (the lock screen waits for a finger before the password)
//!   · the login keyring out of sync with the password (keyring prompts after a password change)
//! and tests your password with the "shadow-passtest" PAM service, which has no faillock: a test
//! can never lock you out. Repairs that need root ask for the password through sudo.
#[path = "../pam.rs"]
#[allow(dead_code)]
mod pam;

use std::io::Write;
use std::path::Path;

const R: &str = "\x1b[0m";
const AMB: &str = "\x1b[38;2;255;176;0m";
const CYN: &str = "\x1b[38;2;0;229;255m";
const DIM: &str = "\x1b[38;2;138;132;116m";
const RED: &str = "\x1b[38;2;255;42;0m";
const GRN: &str = "\x1b[38;2;124;217;90m";

#[derive(Clone, Copy, PartialEq, Debug)]
enum Lvl {
    Ok,
    Warn,
    Bad,
}

struct Finding {
    lvl: Lvl,
    what: String,
    /// a repair: (label, shell command, needs root)
    fix: Option<(String, String, bool)>,
}

fn f(lvl: Lvl, what: impl Into<String>) -> Finding {
    Finding { lvl, what: what.into(), fix: None }
}

fn out(cmd: &[&str]) -> (i32, String) {
    match std::process::Command::new(cmd[0]).args(&cmd[1..]).stdin(std::process::Stdio::null()).output() {
        Ok(o) => (o.status.code().unwrap_or(1), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))),
        Err(_) => (127, String::new()),
    }
}

fn user() -> String {
    std::env::var("SUDO_USER").or_else(|_| std::env::var("USER")).unwrap_or_else(|_| out(&["id", "-un"]).1.trim().to_string())
}

/// faillock.conf / pam lines: (deny, unlock_time seconds)
pub fn faillock_policy(conf: &str, pam: &str) -> (u32, u32) {
    let mut deny = 3;
    let mut unlock = 600;
    for l in conf.lines().map(str::trim).filter(|l| !l.starts_with('#')) {
        if let Some((k, v)) = l.split_once('=') {
            match k.trim() {
                "deny" => deny = v.trim().parse().unwrap_or(deny),
                "unlock_time" => unlock = v.trim().parse().unwrap_or(unlock),
                _ => {}
            }
        }
    }
    for w in pam.split_whitespace() {
        if let Some(v) = w.strip_prefix("deny=") {
            deny = v.parse().unwrap_or(deny);
        }
        if let Some(v) = w.strip_prefix("unlock_time=") {
            unlock = v.parse().unwrap_or(unlock);
        }
    }
    (deny, unlock)
}

/// `faillock --user U` output: number of failures still counted (valid "V")
pub fn faillock_failures(text: &str) -> usize {
    text.lines().filter(|l| l.split_whitespace().last() == Some("V")).count()
}

fn pam_stack_ok(service: &str) -> Result<(), String> {
    let p = Path::new("/etc/pam.d").join(service);
    let t = std::fs::read_to_string(&p).map_err(|_| format!("/etc/pam.d/{service} is missing"))?;
    for l in t.lines().map(str::trim).filter(|l| !l.starts_with('#') && !l.is_empty()) {
        let w: Vec<&str> = l.split_whitespace().collect();
        if w.len() >= 3 && (w[1] == "include" || w[1] == "substack") && !Path::new("/etc/pam.d").join(w[2]).exists() {
            return Err(format!("/etc/pam.d/{service} includes '{}', which does not exist", w[2]));
        }
    }
    Ok(())
}

fn led(name: &str) -> Option<bool> {
    let rd = std::fs::read_dir("/sys/class/leds").ok()?;
    let mut seen = None;
    for e in rd.flatten() {
        if e.file_name().to_string_lossy().ends_with(name) {
            let on = std::fs::read_to_string(e.path().join("brightness")).is_ok_and(|b| b.trim() != "0");
            seen = Some(seen.unwrap_or(false) || on);
        }
    }
    seen
}

fn checks() -> Vec<Finding> {
    let u = user();
    let mut v = vec![];
    // 1. faillock
    let conf = std::fs::read_to_string("/etc/security/faillock.conf").unwrap_or_default();
    let pam_all = ["system-auth", "system-login", "password-auth"].iter().map(|s| std::fs::read_to_string(Path::new("/etc/pam.d").join(s)).unwrap_or_default()).collect::<String>();
    let uses_faillock = pam_all.contains("pam_faillock");
    if uses_faillock {
        let (deny, unlock) = faillock_policy(&conf, &pam_all);
        let (c, t) = out(&["faillock", "--user", &u]);
        let n = faillock_failures(&t);
        let readable = c == 0 && !t.contains("Permission denied");
        let mut x = if !readable {
            f(Lvl::Warn, format!("pam_faillock is active: {deny} wrong passwords lock '{u}' for {} min — your current count needs root to read", unlock / 60))
        } else if n as u32 >= deny {
            f(Lvl::Bad, format!("'{u}' is LOCKED by pam_faillock: {n} failed attempts (limit {deny}); even the right password fails until it unlocks ({} min after the last failure) or is reset", unlock / 60))
        } else if n > 0 {
            f(Lvl::Warn, format!("{n} failed attempt(s) counted for '{u}' (lock at {deny})"))
        } else {
            f(Lvl::Ok, format!("not locked by pam_faillock (lock after {deny} failures for {} min)", unlock / 60))
        };
        if !readable || n > 0 {
            x.fix = Some(("reset the failure count".into(), format!("faillock --user {u} --reset"), true));
        }
        v.push(x);
    } else {
        v.push(f(Lvl::Ok, "no pam_faillock lockout in the PAM stack"));
    }
    // 2. Caps / Num Lock
    match led("capslock") {
        Some(true) => v.push(f(Lvl::Warn, "Caps Lock is ON")),
        Some(false) => v.push(f(Lvl::Ok, "Caps Lock is off")),
        None => {}
    }
    if led("numlock") == Some(false) {
        v.push(f(Lvl::Ok, "Num Lock is off (digits on the keypad won't type)"));
    }
    // 3. keyboard layout: console keymap vs desktop XKB layout
    let (_, loc) = out(&["localectl", "status"]);
    let get = |k: &str| loc.lines().find_map(|l| l.trim().strip_prefix(k)).map(|s| s.trim().to_string());
    let vc = get("VC Keymap:").unwrap_or_default();
    let x11 = get("X11 Layout:").unwrap_or_default();
    let cfg = std::fs::read_to_string(std::env::var("HOME").map(|h| format!("{h}/.config/shadow-state/config.toml")).unwrap_or_default()).unwrap_or_default();
    let shadow_layout = cfg.lines().find_map(|l| l.trim().strip_prefix("xkb_layout")).and_then(|r| r.split('=').nth(1)).map(|s| s.trim().trim_matches('"').to_string()).filter(|s| !s.is_empty());
    let desktop = shadow_layout.clone().unwrap_or_else(|| if x11.is_empty() { "us".into() } else { x11.clone() });
    let vc_base = vc.split(['-', '_']).next().unwrap_or("").to_string();
    let desk_base = desktop.split([',', '(']).next().unwrap_or("").to_string();
    if vc.is_empty() || vc == "n/a" || vc_base == desk_base || (vc_base == "us" && desk_base == "us") {
        v.push(f(Lvl::Ok, format!("keyboard layouts agree (console '{}', desktop '{desktop}')", if vc.is_empty() { "default" } else { &vc })));
    } else {
        let mut x = f(Lvl::Warn, format!("console keymap '{vc}' ≠ desktop layout '{desktop}': a password typed at the login screen may differ from the one typed in the desktop/lock screen"));
        x.fix = Some((format!("set the console keymap to '{desk_base}'"), format!("localectl set-keymap {desk_base}"), true));
        v.push(x);
    }
    // 4. PAM files
    for svc in ["shadow-lock", "greetd", "shadow-passtest"] {
        if svc == "greetd" && !Path::new("/etc/greetd").exists() {
            continue;
        }
        match pam_stack_ok(svc) {
            Ok(()) => v.push(f(Lvl::Ok, format!("/etc/pam.d/{svc} is intact"))),
            Err(e) => {
                let mut x = f(Lvl::Bad, e);
                let shipped = format!("/opt/shadow-tui/share/etc-defaults/pam.d/{svc}");
                if Path::new(&shipped).exists() {
                    x.fix = Some((format!("restore the shipped /etc/pam.d/{svc}"), format!("install -m644 {shipped} /etc/pam.d/{svc}"), true));
                }
                v.push(x);
            }
        }
    }
    // 5. administrator
    let (_, groups) = out(&["id", "-nG", &u]);
    if u == "root" || groups.split_whitespace().any(|g| g == "wheel") {
        v.push(f(Lvl::Ok, format!("'{u}' is an administrator (wheel)")));
    } else {
        v.push(f(Lvl::Warn, format!("'{u}' is not in 'wheel': sudo and administrator prompts refuse your password — from root: usermod -aG wheel {u}")));
    }
    // 6. fingerprint first
    if pam_all.contains("pam_fprintd") {
        v.push(f(Lvl::Warn, "fingerprint login is in the PAM stack: the lock screen asks for a finger first — press Enter on an empty password or wait, then type the password"));
    }
    // 7. keyring
    let kr = std::env::var("HOME").map(|h| format!("{h}/.local/share/keyrings/login.keyring")).unwrap_or_default();
    if Path::new(&kr).exists() {
        let greetd_unlocks = std::fs::read_to_string("/etc/pam.d/greetd").unwrap_or_default().contains("pam_gnome_keyring");
        let mut x = if greetd_unlocks {
            f(Lvl::Ok, "the login keyring is unlocked at login (pam_gnome_keyring in greetd)")
        } else {
            f(Lvl::Warn, "the login keyring is NOT unlocked at login: apps (VS Code, Chrome, Discord) ask for the keyring password or lose saved logins")
        };
        if !greetd_unlocks && Path::new("/etc/pam.d/greetd").exists() {
            x.fix = Some(("unlock the keyring at login (adds optional pam_gnome_keyring lines to /etc/pam.d/greetd)".into(), "shadow fix apply keyring-pam".into(), true));
        }
        v.push(x);
        v.push(Finding {
            lvl: Lvl::Ok,
            what: "if you changed your password and the keyring keeps asking for the OLD one: reset the login keyring (saved app logins in it are lost; a backup is kept)".into(),
            fix: Some(("back up and reset the login keyring".into(), format!("mv {kr} {kr}.bak-$(date +%s)"), false)),
        });
    }
    v
}

/// None at the end of input
fn ask(q: &str) -> Option<String> {
    print!("{q}");
    let _ = std::io::stdout().flush();
    let mut l = String::new();
    match std::io::stdin().read_line(&mut l) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(l.trim().to_string()),
    }
}

fn read_secret(q: &str) -> String {
    print!("{q}");
    let _ = std::io::stdout().flush();
    // no echo while typing
    let tty = std::io::IsTerminal::is_terminal(&std::io::stdin());
    if tty {
        let _ = std::process::Command::new("stty").arg("-echo").status();
    }
    let mut l = String::new();
    let _ = std::io::stdin().read_line(&mut l);
    if tty {
        let _ = std::process::Command::new("stty").arg("echo").status();
        println!();
    }
    l.trim_end_matches(['\n', '\r']).to_string()
}

fn test_password() {
    let u = user();
    println!("\n{AMB}Password test{R} for '{u}' — uses the shadow-passtest PAM service (no lockout counting).");
    if pam_stack_ok("shadow-passtest").is_err() {
        println!("{RED}✗ /etc/pam.d/shadow-passtest is missing: reinstall Shadow (./install.sh) to get it{R}");
        return;
    }
    let mut pw = read_secret("  password: ");
    let r = pam::authenticate_detailed("shadow-passtest", &u, &pw, false);
    // overwrite before dropping
    // SAFETY: zero bytes keep the String valid UTF-8
    unsafe { pw.as_bytes_mut().iter_mut().for_each(|b| std::ptr::write_volatile(b, 0)) };
    match r {
        Ok(()) => {
            println!("  {GRN}✓ the password is correct.{R}");
            println!("    If the lock or login screen still refuses it: a faillock lockout, Caps Lock or the");
            println!("    keyboard layout (see the checks above) is the cause, not the password.");
        }
        Err(e) if e.code == pam::PAM_NEW_AUTHTOK_REQD => println!("  {AMB}! the password is right but has EXPIRED: run  passwd  here to set a new one{R}"),
        Err(e) if e.code == pam::PAM_ACCT_EXPIRED || (e.account_stage && e.code == pam::PAM_PERM_DENIED) => println!("  {RED}✗ the account itself is expired or locked ({e}); an administrator must run:  chage -E -1 {u}; passwd -u {u}{R}"),
        Err(e) if e.code == pam::PAM_AUTH_ERR => {
            println!("  {RED}✗ wrong password{R} (as typed here, with the console keymap).");
            println!("    Typed with Caps Lock off and the right layout? Then the password itself differs:");
            println!("    an administrator can set a new one with  passwd {u}  (or boot to a rescue shell).");
        }
        Err(e) => println!("  {RED}✗ {e}{R}"),
    }
}

fn run_fix(label: &str, cmd: &str, root: bool) {
    println!("\n{AMB}→ {label}{R}\n  {DIM}$ {}{cmd}{R}", if root { "sudo " } else { "" });
    let full = if root && !out(&["id", "-u"]).1.trim().eq("0") { format!("sudo -p '[shadow-passfix] administrator password: ' sh -c '{}'", cmd.replace('\'', "'\\''")) } else { cmd.to_string() };
    let ok = std::process::Command::new("sh").args(["-c", &full]).status().map(|s| s.success()).unwrap_or(false);
    if ok {
        println!("  {GRN}✓ done{R}");
    } else if root {
        println!("  {RED}✗ not done.{R} If sudo itself is locked out (same faillock counter), wait for the unlock time,");
        println!("    log in as root on a text console, or reboot: the failure counter lives in /run and is cleared at boot.");
    } else {
        println!("  {RED}✗ failed{R}");
    }
}

fn main() {
    if std::env::args().any(|a| a == "--check") {
        // non-interactive summary for the Fix center: "lvl text"
        for c in checks() {
            println!("{} {}", match c.lvl { Lvl::Ok => "ok", Lvl::Warn => "warn", Lvl::Bad => "bad" }, c.what);
        }
        return;
    }
    loop {
        print!("\x1b[2J\x1b[H");
        println!("{AMB}╭─ ◢ SHADOW // PASSWORD REPAIR ─╮{R}  {DIM}lockouts · layouts · PAM · keyring{R}\n");
        let cs = checks();
        let mut fixes = vec![];
        for c in &cs {
            let mark = match c.lvl {
                Lvl::Ok => format!("{GRN}✓{R}"),
                Lvl::Warn => format!("{AMB}!{R}"),
                Lvl::Bad => format!("{RED}✗{R}"),
            };
            println!(" {mark} {}", c.what);
            if let Some((label, cmd, root)) = &c.fix {
                fixes.push((label.clone(), cmd.clone(), *root));
                println!("     {CYN}[{}]{R} {label}{}", fixes.len(), if *root { " (administrator)" } else { "" });
            }
        }
        println!("\n {CYN}t{R} test my password (never counts towards a lockout) · {CYN}1-{}{R} apply a repair · {CYN}r{R} recheck · {CYN}q{R} quit", fixes.len().max(1));
        let Some(a) = ask(&format!("\n {AMB}passfix ›{R} ")) else { return };
        match a.as_str() {
            "q" | "quit" => return,
            "t" => {
                test_password();
                ask(&format!("\n {DIM}Enter to continue{R} "));
            }
            n => {
                if let Some((label, cmd, root)) = n.parse::<usize>().ok().and_then(|i| fixes.get(i.wrapping_sub(1))) {
                    run_fix(label, cmd, *root);
                    ask(&format!("\n {DIM}Enter to continue{R} "));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn faillock_parsing() {
        assert_eq!(faillock_policy("# deny = 5\n", ""), (3, 600));
        assert_eq!(faillock_policy("deny = 5\nunlock_time = 900\n", ""), (5, 900));
        assert_eq!(faillock_policy("", "auth required pam_faillock.so preauth deny=4 unlock_time=120"), (4, 120));
        let t = "nami:\nWhen                Type  Source                                           Valid\n2026-11-01 16:00:01 TTY   /dev/tty2                                            V\n2026-11-01 16:00:09 TTY   /dev/tty2                                            V\n2026-11-01 15:00:00 TTY   /dev/tty2                                            I\n";
        assert_eq!(faillock_failures(t), 2);
    }
}
