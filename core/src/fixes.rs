//! The fix center: known problems of desktop apps on Shadow's compositor (and Wayland tiling compositors in
//! general), found in what Hyprland/sway/niri users report, with repairs — and tasks for the
//! everyday "it's stuck" moments. Text console first (`shadow fix …` works on a text console);
//! the shadow-fix window drives the same commands.
//!
//!   shadow fix scan [--porcelain]        what's wrong now
//!   shadow fix apply ID|all-user         repair (system repairs ask for the password via sudo)
//!   shadow fix tasks [--porcelain]       per-app tasks: clear caches, unlock a profile, force quit
//!   shadow fix run TASK                  run one
//!   shadow fix triage [--porcelain] [--quiet] [--notify]
//!                                        AFTER updates: what the updates changed (app versions,
//!                                        desktop files, caches) and what that needs
//!   shadow fix update                    update the system + Flatpaks in a console, then triage
//!
//! Repairs are never run DURING an update: package hooks aren't used. Triage runs when the
//! package database changed since the last triage — at the next login (shadow-sessiond) or right
//! after `shadow fix update`. User-level repairs edit only your own files and keep a .shadow-bak
//! copy; system repairs say exactly what they change.
use crate::st;
use crate::sys;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    /// the app isn't installed / the check doesn't apply
    NotHere,
    Ok(String),
    Issue(String),
}

pub struct Fix {
    pub id: &'static str,
    pub app: &'static str,
    pub title: &'static str,
    /// needs root (asks for the password via sudo)
    pub root: bool,
}

pub const FIXES: &[Fix] = &[
    Fix { id: "polkit-agent", app: "session", title: "password prompts for privileged actions (polkit agent)", root: true },
    Fix { id: "xwayland", app: "session", title: "X11 apps (Steam, games, older apps) need xwayland-satellite", root: true },
    Fix { id: "portals", app: "session", title: "screen sharing and file pickers (xdg-desktop-portal-gnome + gtk)", root: true },
    Fix { id: "notifications", app: "session", title: "app notifications need a notification daemon", root: true },
    Fix { id: "keyring-pam", app: "session", title: "unlock the login keyring at login (saved app logins, no keyring prompts)", root: true },
    Fix { id: "dark-mode", app: "session", title: "GTK / libadwaita apps follow the dark Shadow theme", root: false },
    Fix { id: "discord-skip-update", app: "discord", title: "Discord: stop the \"update available, download\" loop of distro packages", root: false },
    Fix { id: "discord-tray", app: "discord", title: "Discord: closing hides it in a tray Shadow doesn't show — quit on close instead", root: false },
    Fix { id: "discord-wayland", app: "discord", title: "Discord: native Wayland (sharp text, PipeWire screen capture)", root: false },
    Fix { id: "code-keyring", app: "code", title: "VS Code: \"OS keyring couldn't be identified\" — use the Secret Service", root: false },
    Fix { id: "chromium-flags", app: "chromium", title: "Chrome / Chromium / Brave / Vivaldi / Edge: Wayland + keyring", root: false },
    Fix { id: "firefox-vaapi", app: "firefox", title: "Firefox: hardware video decoding (VA-API) on Intel", root: false },
    Fix { id: "firefox-portal", app: "firefox", title: "Firefox: system file dialogs through the portal", root: false },
    Fix { id: "steam-scaling", app: "steam", title: "Steam: readable UI on a scaled (HiDPI) panel", root: false },
    Fix { id: "electron-wayland", app: "electron", title: "Spotify / Slack / Obsidian: native Wayland", root: false },
    Fix { id: "flatpak-wayland", app: "flatpak", title: "Flatpak apps: Wayland socket + your GTK theme", root: false },
];

fn home() -> PathBuf {
    st::home()
}

fn have(cmd: &str) -> bool {
    sys::which(cmd).is_some()
}

fn any_file(paths: &[&str]) -> bool {
    paths.iter().any(|p| Path::new(p).exists())
}

fn backup(p: &Path) {
    if p.exists() {
        let b = p.with_file_name(format!("{}.shadow-bak", p.file_name().unwrap_or_default().to_string_lossy()));
        if !b.exists() {
            let _ = std::fs::copy(p, b);
        }
    }
}

fn write(p: &Path, text: &str) -> Result<(), String> {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    backup(p);
    sys::atomic_write(p, text).map_err(|e| format!("{}: {e}", p.display()))
}

// ── small, careful text edits (JSON / JSONC / user.js) ─────────────────────────────────────────

/// Set "key": literal in a flat JSON/JSONC object, keeping everything else (comments included).
pub fn json_set(text: &str, key: &str, literal: &str) -> String {
    let needle = format!("\"{key}\"");
    if let Some(i) = text.find(&needle) {
        // replace the value after the colon up to the next , or } on the same level
        let after = &text[i + needle.len()..];
        if let Some(c) = after.find(':') {
            let vstart = i + needle.len() + c + 1;
            let rest = &text[vstart..];
            let end = rest.find([',', '}', '\n']).unwrap_or(rest.len());
            return format!("{} {}{}", &text[..vstart], literal, &text[vstart + end..]);
        }
    }
    // the object's opening brace: the first '{' outside // comment lines (JSONC: argv.json)
    let mut open = None;
    let mut pos = 0;
    for l in text.split_inclusive('\n') {
        if !l.trim_start().starts_with("//") {
            if let Some(i) = l.find('{') {
                open = Some(pos + i);
                break;
            }
        }
        pos += l.len();
    }
    let Some(open) = open else {
        // no object at all (empty or unreadable file): a fresh one, the old text kept as comments
        let kept: String = text.lines().filter(|l| !l.trim().is_empty()).map(|l| format!("// {l}\n")).collect();
        return format!("{kept}{{\n  \"{key}\": {literal}\n}}\n");
    };
    let rest = &text[open + 1..];
    let body_empty = rest.trim_start().starts_with('}');
    let sep = if body_empty { "" } else if rest.starts_with('\n') { "," } else { ",\n  " };
    format!("{}\n  \"{key}\": {literal}{sep}{rest}", &text[..=open])
}

pub fn json_get<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\"");
    let i = text.find(&needle)?;
    let after = &text[i + needle.len()..];
    let c = after.find(':')?;
    let v = &after[c + 1..];
    let end = v.find([',', '}', '\n']).unwrap_or(v.len());
    Some(v[..end].trim())
}

/// Firefox user.js: set user_pref(name, value)
pub fn user_pref_set(text: &str, name: &str, value: &str) -> String {
    let line = format!("user_pref(\"{name}\", {value});");
    let mut found = false;
    let mut out: Vec<String> = text
        .lines()
        .map(|l| {
            if l.trim_start().starts_with(&format!("user_pref(\"{name}\"")) {
                found = true;
                line.clone()
            } else {
                l.to_string()
            }
        })
        .collect();
    if !found {
        out.push(line);
    }
    out.join("\n") + "\n"
}

fn ff_profiles() -> Vec<PathBuf> {
    let mut v = vec![];
    for base in [home().join(".mozilla/firefox"), home().join(".config/mozilla/firefox")] {
        let ini = std::fs::read_to_string(base.join("profiles.ini")).unwrap_or_default();
        let mut rel = true;
        for l in ini.lines() {
            if let Some(r) = l.strip_prefix("IsRelative=") {
                rel = r.trim() == "1";
            }
            if let Some(p) = l.strip_prefix("Path=") {
                let p = if rel { base.join(p.trim()) } else { PathBuf::from(p.trim()) };
                if p.is_dir() && !v.contains(&p) {
                    v.push(p);
                }
            }
        }
    }
    v
}

// ── desktop-file overrides (flags / environment without touching system files) ────────────────

fn app_dirs() -> Vec<PathBuf> {
    vec![PathBuf::from("/usr/share/applications"), PathBuf::from("/usr/local/share/applications"), PathBuf::from("/var/lib/flatpak/exports/share/applications"), home().join(".local/share/flatpak/exports/share/applications")]
}

fn system_desktop(file: &str) -> Option<PathBuf> {
    app_dirs().into_iter().map(|d| d.join(file)).find(|p| p.is_file())
}

fn override_path(file: &str) -> PathBuf {
    home().join(".local/share/applications").join(file)
}

fn mtime(p: &Path) -> u64 {
    std::fs::metadata(p).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0)
}

/// The override we wrote: flags/env as asked, and up to date with the system file?
fn override_state(file: &str, flags: &str, env: &str) -> State {
    let Some(src) = system_desktop(file) else { return State::NotHere };
    let o = override_path(file);
    let Ok(t) = std::fs::read_to_string(&o) else { return State::Issue("not set up".into()) };
    let Some(mark) = t.lines().find_map(|l| l.strip_prefix("X-Shadow-Fix=")) else {
        return State::Ok("you have your own override of this app; left alone".into());
    };
    let recorded: u64 = mark.split_whitespace().last().and_then(|x| x.parse().ok()).unwrap_or(0);
    if recorded < mtime(&src) {
        return State::Issue("the app was updated: its launcher changed, the override needs refreshing".into());
    }
    let exec_ok = t.lines().filter(|l| l.starts_with("Exec=")).all(|l| (flags.is_empty() || l.contains(flags)) && (env.is_empty() || l.contains(env)));
    if exec_ok { State::Ok("set".into()) } else { State::Issue("flags differ".into()) }
}

/// An Exec= command with `env` in front and `flags` after the program — after "flatpak run … APPID"
/// for Flatpaks, so the flags reach the app and not flatpak.
pub fn rewrite_exec(cmd: &str, flags: &str, env: &str) -> String {
    let mut words: Vec<&str> = cmd.split(' ').collect();
    let at = if words.first().is_some_and(|w| w.ends_with("flatpak")) {
        words.iter().position(|w| !w.starts_with('-') && w.contains('.') && !w.contains('/')).map(|i| i + 1).unwrap_or(words.len())
    } else {
        1
    };
    if !flags.is_empty() {
        words.insert(at.min(words.len()), flags);
    }
    let prefix = if env.is_empty() { String::new() } else { format!("env {env} ") };
    format!("{prefix}{}", words.join(" "))
}

/// Copy the system .desktop file to ~/.local/share/applications with `env` + `flags` in every Exec.
fn write_override(file: &str, flags: &str, env: &str, id: &str) -> Result<String, String> {
    let src = system_desktop(file).ok_or_else(|| format!("{file} is not installed"))?;
    let t = std::fs::read_to_string(&src).map_err(|e| e.to_string())?;
    let mut out = vec![];
    for l in t.lines() {
        if let Some(cmd) = l.strip_prefix("Exec=") {
            out.push(format!("Exec={}", rewrite_exec(cmd, flags, env)));
        } else {
            out.push(l.to_string());
        }
    }
    // our marker in the main section: which fix, and the system file's age (staleness after updates)
    let mut done = false;
    let mut res = vec![];
    for l in out {
        res.push(l.clone());
        if !done && l.trim() == "[Desktop Entry]" {
            res.push(format!("X-Shadow-Fix={id} {}", mtime(&src)));
            done = true;
        }
    }
    let o = override_path(file);
    if let Ok(cur) = std::fs::read_to_string(&o) {
        if !cur.contains("X-Shadow-Fix=") {
            return Err(format!("{} is your own override: not replaced", o.display()));
        }
    }
    write(&o, &(res.join("\n") + "\n"))?;
    let _ = sys::run(&["update-desktop-database", &home().join(".local/share/applications").to_string_lossy()]);
    Ok(format!("launcher override {}", o.display()))
}

const CHROMIUMS: &[&str] = &["google-chrome.desktop", "chromium.desktop", "chromium-browser.desktop", "brave-browser.desktop", "vivaldi-stable.desktop", "microsoft-edge.desktop"];
const ELECTRONS: &[(&str, &str)] = &[("spotify.desktop", "--enable-features=UseOzonePlatform --ozone-platform=wayland"), ("slack.desktop", "--ozone-platform-hint=auto"), ("obsidian.desktop", "--ozone-platform-hint=auto")];
const DISCORD_FILES: &[&str] = &["discord.desktop", "discord-canary.desktop", "discord-ptb.desktop"];
const DISCORD_FLAGS: &str = "--enable-features=UseOzonePlatform,WaylandWindowDecorations,WebRTCPipeWireCapturer --ozone-platform-hint=auto";
const CHROMIUM_FLAGS: &str = "--ozone-platform-hint=auto --password-store=gnome-libsecret";
const FLATPAKS: &[&str] = &["com.discordapp.Discord", "com.spotify.Client", "com.slack.Slack", "md.obsidian.Obsidian", "com.visualstudio.code", "com.google.Chrome"];

fn discord_dirs() -> Vec<PathBuf> {
    ["discord", "discordcanary", "discordptb"].iter().map(|d| home().join(".config").join(d)).filter(|p| p.is_dir()).collect()
}

fn secret_service() -> bool {
    have("gnome-keyring-daemon") || have("kwalletd6") || have("kwalletd5") || have("keepassxc")
}

fn scale() -> f64 {
    shadow_niri_scale().unwrap_or(1.0)
}

fn shadow_niri_scale() -> Option<f64> {
    let (c, o, _) = sys::run(&["shadowctl", "niri", "--json", "outputs"]);
    if c != 0 {
        return None;
    }
    // the largest "scale": N in the JSON
    o.match_indices("\"scale\":").filter_map(|(i, _)| o[i + 8..].split([',', '}']).next()?.trim().parse::<f64>().ok()).fold(None, |m: Option<f64>, x| Some(m.map_or(x, |m| m.max(x))))
}

fn gs(schema: &str, key: &str) -> Option<String> {
    let (c, o, _) = sys::run(&["gsettings", "get", schema, key]);
    (c == 0).then(|| o.trim().trim_matches('\'').to_string())
}

pub fn detect(id: &str) -> State {
    match id {
        "polkit-agent" => {
            let agents = ["/usr/libexec/polkit-mate-authentication-agent-1", "/usr/lib/mate-polkit/polkit-mate-authentication-agent-1", "/usr/bin/lxqt-policykit-agent", "/usr/libexec/polkit-gnome-authentication-agent-1", "/usr/lib/polkit-gnome/polkit-gnome-authentication-agent-1", "/usr/bin/lxpolkit", "/usr/libexec/xfce-polkit", "/usr/libexec/kf6/polkit-kde-authentication-agent-1"];
            if any_file(&agents) { State::Ok("installed (shadow-sessiond starts it)".into()) } else { State::Issue("none installed: actions that need a password fail".into()) }
        }
        "xwayland" => {
            if !have("shadow-compositor") {
                State::NotHere
            } else if have("xwayland-satellite") {
                State::Ok("installed (the compositor starts it for X11 apps)".into())
            } else {
                State::Issue("not installed: X11-only apps won't open".into())
            }
        }
        "portals" => {
            let gnome = any_file(&["/usr/libexec/xdg-desktop-portal-gnome", "/usr/lib/xdg-desktop-portal-gnome"]);
            let gtk = any_file(&["/usr/libexec/xdg-desktop-portal-gtk", "/usr/lib/xdg-desktop-portal-gtk"]);
            match (gnome, gtk) {
                (true, true) => State::Ok("gnome + gtk portals installed".into()),
                (false, _) => State::Issue("xdg-desktop-portal-gnome missing: no screen sharing".into()),
                (true, false) => State::Issue("xdg-desktop-portal-gtk missing: some file dialogs won't open".into()),
            }
        }
        "notifications" => {
            let running = sys::run(&["busctl", "--user", "status", "org.freedesktop.Notifications"]).0 == 0;
            let installed = ["mako", "swaync", "dunst", "fnott"].iter().any(|d| have(d));
            match (running, installed) {
                (true, _) => State::Ok("a notification daemon is running".into()),
                (false, true) => State::Ok("installed (shadow-sessiond starts it at login)".into()),
                (false, false) => State::Issue("no notification daemon: app notifications disappear".into()),
            }
        }
        "keyring-pam" => {
            let module = any_file(&["/usr/lib64/security/pam_gnome_keyring.so", "/usr/lib/security/pam_gnome_keyring.so", "/lib/security/pam_gnome_keyring.so"]);
            let greetd = std::fs::read_to_string("/etc/pam.d/greetd").ok();
            match (module, greetd) {
                (_, None) => State::NotHere,
                (false, _) => {
                    if home().join(".local/share/keyrings").exists() { State::Issue("gnome-keyring's PAM module is not installed".into()) } else { State::NotHere }
                }
                (true, Some(t)) if t.contains("pam_gnome_keyring") => State::Ok("unlocked at login".into()),
                (true, Some(_)) => State::Issue("the login screen doesn't unlock the keyring: apps ask for its password".into()),
            }
        }
        "dark-mode" => match gs("org.gnome.desktop.interface", "color-scheme") {
            None => State::NotHere,
            Some(c) if c == "prefer-dark" => State::Ok("dark".into()),
            Some(c) => State::Issue(format!("color-scheme is '{c}': GTK apps are light")),
        },
        "discord-skip-update" | "discord-tray" => {
            let dirs = discord_dirs();
            if dirs.is_empty() {
                return State::NotHere;
            }
            let (key, want) = if id == "discord-skip-update" { ("SKIP_HOST_UPDATE", "true") } else { ("MINIMIZE_TO_TRAY", "false") };
            let bad: Vec<String> = dirs.iter().filter(|d| json_get(&std::fs::read_to_string(d.join("settings.json")).unwrap_or_default(), key) != Some(want)).map(|d| d.file_name().unwrap_or_default().to_string_lossy().into_owned()).collect();
            if bad.is_empty() { State::Ok(format!("{key} = {want}")) } else { State::Issue(format!("{key} not {want} ({})", bad.join(", "))) }
        }
        "discord-wayland" => DISCORD_FILES.iter().map(|f| override_state(f, DISCORD_FLAGS, "")).find(|s| *s != State::NotHere).unwrap_or(State::NotHere),
        "code-keyring" => {
            let argv: Vec<PathBuf> = [".vscode/argv.json", ".vscode-oss/argv.json"].iter().map(|p| home().join(p)).collect();
            let present: Vec<&PathBuf> = argv.iter().filter(|p| p.parent().is_some_and(|d| d.is_dir())).collect();
            if present.is_empty() || !secret_service() {
                return State::NotHere;
            }
            if present.iter().all(|p| std::fs::read_to_string(p).unwrap_or_default().contains("\"password-store\"")) {
                State::Ok("password-store set".into())
            } else {
                State::Issue("no password-store: VS Code can't find the keyring outside GNOME/KDE".into())
            }
        }
        "chromium-flags" => {
            let st: Vec<State> = CHROMIUMS.iter().map(|f| override_state(f, CHROMIUM_FLAGS, "")).filter(|s| *s != State::NotHere).collect();
            if st.is_empty() {
                State::NotHere
            } else if let Some(i) = st.iter().find(|s| matches!(s, State::Issue(_))) {
                i.clone()
            } else {
                State::Ok("set".into())
            }
        }
        "firefox-vaapi" | "firefox-portal" => {
            let ps = ff_profiles();
            if ps.is_empty() {
                return State::NotHere;
            }
            let (pref, val) = if id == "firefox-vaapi" { ("media.ffmpeg.vaapi.enabled", "true") } else { ("widget.use-xdg-desktop-portal.file-picker", "1") };
            let line = format!("user_pref(\"{pref}\", {val});");
            let missing = ps.iter().filter(|p| !std::fs::read_to_string(p.join("user.js")).unwrap_or_default().contains(&line)).count();
            if id == "firefox-vaapi" && !any_file(&["/usr/lib64/dri/iHD_drv_video.so", "/usr/lib/dri/iHD_drv_video.so", "/usr/lib/x86_64-linux-gnu/dri/iHD_drv_video.so"]) {
                return State::Issue("Intel's VA-API driver (intel-media-driver) is not installed: video decodes on the CPU".into());
            }
            if missing == 0 { State::Ok("set".into()) } else { State::Issue(format!("not set in {missing} profile(s)")) }
        }
        "steam-scaling" => {
            let s = scale();
            if s <= 1.0 {
                return override_state("steam.desktop", "", "").clone().into_not_here_if_missing();
            }
            override_state("steam.desktop", "", &format!("STEAM_FORCE_DESKTOPUI_SCALING={s}"))
        }
        "electron-wayland" => {
            let st: Vec<State> = ELECTRONS.iter().map(|(f, fl)| override_state(f, fl, "")).filter(|s| *s != State::NotHere).collect();
            if st.is_empty() {
                State::NotHere
            } else if let Some(i) = st.iter().find(|s| matches!(s, State::Issue(_))) {
                i.clone()
            } else {
                State::Ok("set".into())
            }
        }
        "flatpak-wayland" => {
            if !have("flatpak") {
                return State::NotHere;
            }
            let installed = sys::run(&["flatpak", "list", "--app", "--columns=application"]).1;
            let ours: Vec<&str> = FLATPAKS.iter().copied().filter(|a| installed.lines().any(|l| l.trim() == *a)).collect();
            if ours.is_empty() {
                return State::NotHere;
            }
            let missing: Vec<&str> = ours.iter().copied().filter(|a| !sys::run(&["flatpak", "override", "--user", "--show", a]).1.contains("wayland")).collect();
            if missing.is_empty() { State::Ok("set".into()) } else { State::Issue(format!("no Wayland socket for {}", missing.join(", "))) }
        }
        _ => State::NotHere,
    }
}

trait NotHereIfMissing {
    fn into_not_here_if_missing(self) -> State;
}
impl NotHereIfMissing for State {
    fn into_not_here_if_missing(self) -> State {
        match self {
            State::NotHere => State::NotHere,
            _ => State::Ok("not needed at scale 1".into()),
        }
    }
}

fn root_install(arch_pkgs: &[&str]) -> Result<String, String> {
    if crate::distro::install(arch_pkgs) {
        Ok(format!("installed {}", arch_pkgs.join(" ")))
    } else {
        Err(format!("could not install {} — {}", arch_pkgs.join(" "), crate::distro::install_hint(arch_pkgs)))
    }
}

pub fn apply(id: &str) -> Result<String, String> {
    let fix = FIXES.iter().find(|f| f.id == id).ok_or_else(|| format!("unknown fix '{id}'"))?;
    if fix.root && !sys::is_root() {
        return Err("needs administrator rights: sudo shadow fix apply ".to_string() + id);
    }
    match id {
        "polkit-agent" => root_install(&["mate-polkit"]),
        "xwayland" => root_install(&["xwayland-satellite"]),
        "portals" => root_install(&["xdg-desktop-portal-gnome", "xdg-desktop-portal-gtk"]),
        "notifications" => root_install(&["mako"]),
        "keyring-pam" => {
            let p = Path::new("/etc/pam.d/greetd");
            let t = std::fs::read_to_string(p).map_err(|e| e.to_string())?;
            if t.contains("pam_gnome_keyring") {
                return Ok("already there".into());
            }
            // "optional" lines can never block a login, whatever gnome-keyring does
            let mut out = vec![];
            let lines: Vec<&str> = t.lines().collect();
            let last = |kind: &str| lines.iter().rposition(|l| l.trim_start().starts_with(kind));
            let (la, ls) = (last("auth"), last("session"));
            for (i, l) in lines.iter().enumerate() {
                out.push(l.to_string());
                if Some(i) == la {
                    out.push("auth       optional     pam_gnome_keyring.so".into());
                }
                if Some(i) == ls {
                    out.push("session    optional     pam_gnome_keyring.so auto_start".into());
                }
            }
            if la.is_none() || ls.is_none() {
                return Err("/etc/pam.d/greetd has no auth/session lines: not edited".into());
            }
            std::fs::copy(p, "/etc/pam.d/greetd.shadow-bak").map_err(|e| e.to_string())?;
            sys::atomic_write(p, &(out.join("\n") + "\n")).map_err(|e| e.to_string())?;
            if sys::which("restorecon").is_some() {
                sys::run(&["restorecon", "/etc/pam.d/greetd"]);
            }
            Ok("added optional pam_gnome_keyring lines (backup: /etc/pam.d/greetd.shadow-bak); takes effect at the next login".into())
        }
        "dark-mode" => {
            sys::run(&["gsettings", "set", "org.gnome.desktop.interface", "color-scheme", "prefer-dark"]);
            if gs("org.gnome.desktop.interface", "gtk-theme").is_some_and(|t| t == "Adwaita") {
                sys::run(&["gsettings", "set", "org.gnome.desktop.interface", "gtk-theme", "Adwaita-dark"]);
            }
            Ok("dark colour scheme for GTK apps".into())
        }
        "discord-skip-update" | "discord-tray" => {
            let (key, val) = if id == "discord-skip-update" { ("SKIP_HOST_UPDATE", "true") } else { ("MINIMIZE_TO_TRAY", "false") };
            let mut n = 0;
            for d in discord_dirs() {
                let p = d.join("settings.json");
                let t = std::fs::read_to_string(&p).unwrap_or_default();
                write(&p, &json_set(&t, key, val))?;
                n += 1;
            }
            Ok(format!("{key} = {val} in {n} Discord config(s); restart Discord"))
        }
        "discord-wayland" => {
            let r: Vec<String> = DISCORD_FILES.iter().filter(|f| system_desktop(f).is_some()).map(|f| write_override(f, DISCORD_FLAGS, "", id)).collect::<Result<_, _>>()?;
            Ok(format!("{} — note: screen sharing from the Discord app itself stays limited on Wayland; the browser or Vesktop share reliably", r.join(", ")))
        }
        "code-keyring" => {
            let mut n = 0;
            for p in [".vscode/argv.json", ".vscode-oss/argv.json"].iter().map(|p| home().join(p)) {
                if p.parent().is_some_and(|d| d.is_dir()) {
                    let t = std::fs::read_to_string(&p).unwrap_or_default();
                    write(&p, &json_set(&t, "password-store", "\"gnome-libsecret\""))?;
                    n += 1;
                }
            }
            Ok(format!("password-store = gnome-libsecret in {n} argv.json; restart VS Code"))
        }
        "chromium-flags" => {
            let r: Vec<String> = CHROMIUMS.iter().filter(|f| system_desktop(f).is_some()).map(|f| write_override(f, CHROMIUM_FLAGS, "", id)).collect::<Result<_, _>>()?;
            Ok(r.join(", "))
        }
        "firefox-vaapi" | "firefox-portal" => {
            let (pref, val) = if id == "firefox-vaapi" { ("media.ffmpeg.vaapi.enabled", "true") } else { ("widget.use-xdg-desktop-portal.file-picker", "1") };
            let ps = ff_profiles();
            for p in &ps {
                let f = p.join("user.js");
                let t = std::fs::read_to_string(&f).unwrap_or_default();
                write(&f, &user_pref_set(&t, pref, val))?;
            }
            let mut m = format!("{pref} = {val} in {} profile(s); restart Firefox", ps.len());
            if id == "firefox-vaapi" && !any_file(&["/usr/lib64/dri/iHD_drv_video.so", "/usr/lib/dri/iHD_drv_video.so"]) {
                m += match crate::distro::family() {
                    crate::distro::Family::Rpm => " · also install Intel's VA-API driver: sudo dnf install intel-media-driver (RPM Fusion) or libva-intel-media-driver",
                    _ => " · also install Intel's VA-API driver: sudo pacman -S intel-media-driver",
                };
            }
            Ok(m)
        }
        "steam-scaling" => write_override("steam.desktop", "", &format!("STEAM_FORCE_DESKTOPUI_SCALING={}", scale()), id),
        "electron-wayland" => {
            let r: Vec<String> = ELECTRONS.iter().filter(|(f, _)| system_desktop(f).is_some()).map(|(f, fl)| write_override(f, fl, "", id)).collect::<Result<_, _>>()?;
            Ok(r.join(", "))
        }
        "flatpak-wayland" => {
            let installed = sys::run(&["flatpak", "list", "--app", "--columns=application"]).1;
            let mut n = 0;
            for a in FLATPAKS.iter().filter(|a| installed.lines().any(|l| l.trim() == **a)) {
                sys::run(&["flatpak", "override", "--user", "--socket=wayland", "--env=ELECTRON_OZONE_PLATFORM_HINT=auto", "--filesystem=xdg-config/gtk-3.0:ro", "--filesystem=xdg-config/gtk-4.0:ro", a]);
                n += 1;
            }
            Ok(format!("Wayland socket + GTK theme for {n} Flatpak app(s)"))
        }
        _ => Err(format!("unknown fix '{id}'")),
    }
}

// ── tasks ───────────────────────────────────────────────────────────────────────────────────────

/// (id, label, applies?)
pub fn tasks() -> Vec<(String, String)> {
    let mut v = vec![];
    for d in discord_dirs() {
        let n = d.file_name().unwrap_or_default().to_string_lossy().into_owned();
        v.push((format!("cache:{n}"), format!("Discord ({n}): clear caches — fixes a white/black window after an update")));
    }
    for (dir, name) in [(".config/Code", "VS Code"), (".config/VSCodium", "VSCodium"), (".config/google-chrome", "Chrome"), (".config/chromium", "Chromium"), (".config/BraveSoftware/Brave-Browser", "Brave"), (".config/Slack", "Slack"), (".config/obsidian", "Obsidian"), (".config/spotify", "Spotify")] {
        if home().join(dir).is_dir() {
            v.push((format!("cache:{dir}"), format!("{name}: clear GPU / code caches — fixes a blank or garbled window")));
        }
    }
    if !ff_profiles().is_empty() {
        v.push(("ff:startup-cache".into(), "Firefox: clear the startup cache — fixes a broken UI after an update".into()));
        v.push(("ff:unlock".into(), "Firefox: \"already running, not responding\" — remove a stale profile lock".into()));
        v.push(("ff:safe".into(), "Firefox: start in troubleshoot mode (no extensions)".into()));
    }
    for (bin, name) in [("discord", "Discord"), ("Discord", "Discord"), ("firefox", "Firefox"), ("code", "VS Code"), ("steam", "Steam"), ("spotify", "Spotify"), ("slack", "Slack"), ("chrome", "Chrome"), ("electron", "Electron apps")] {
        if sys::run(&["pgrep", "-x", bin]).0 == 0 {
            v.push((format!("quit:{bin}"), format!("{name}: force quit (frozen / won't close)")));
        }
    }
    v.push(("portal:restart".into(), "Screen sharing / file dialog stuck: restart the desktop portals".into()));
    v.push(("pipewire:restart".into(), "No sound / mic or screen share black: restart PipeWire".into()));
    v
}

fn firefox_running() -> bool {
    sys::run(&["pgrep", "-x", "firefox"]).0 == 0 || sys::run(&["pgrep", "-f", "firefox-bin"]).0 == 0
}

fn rm_dirs(base: &Path, names: &[&str]) -> usize {
    let mut n = 0;
    for nm in names {
        let p = base.join(nm);
        if p.is_dir() && std::fs::remove_dir_all(&p).is_ok() {
            n += 1;
        }
    }
    n
}

pub fn run_task(id: &str) -> Result<String, String> {
    let (kind, arg) = id.split_once(':').ok_or("bad task")?;
    match kind {
        "cache" => {
            let base = if arg.contains('/') { home().join(arg) } else { home().join(".config").join(arg) };
            if !base.starts_with(home()) || !base.is_dir() {
                return Err("no such app folder".into());
            }
            let n = rm_dirs(&base, &["GPUCache", "Cache", "Code Cache", "DawnCache", "DawnGraphiteCache", "DawnWebGPUCache", "GrShaderCache", "ShaderCache", "Default/GPUCache"]);
            Ok(format!("{n} cache folder(s) cleared in {} — restart the app (settings and logins are kept)", base.display()))
        }
        "ff" => match arg {
            "startup-cache" => {
                let mut n = 0;
                for base in [home().join(".cache/mozilla/firefox"), home().join(".cache/mozilla")] {
                    if let Ok(rd) = std::fs::read_dir(&base) {
                        for e in rd.flatten() {
                            n += rm_dirs(&e.path(), &["startupCache"]);
                        }
                    }
                }
                Ok(format!("{n} startup cache(s) cleared; restart Firefox"))
            }
            "unlock" => {
                if firefox_running() {
                    return Err("Firefox is running — close it (or force quit it) first; the lock is real then".into());
                }
                let mut n = 0;
                for p in ff_profiles() {
                    for l in ["lock", ".parentlock"] {
                        let f = p.join(l);
                        if std::fs::symlink_metadata(&f).is_ok() && std::fs::remove_file(&f).is_ok() {
                            n += 1;
                        }
                    }
                }
                Ok(format!("{n} stale lock(s) removed"))
            }
            "safe" => {
                st::spawn_detached(&["firefox", "--safe-mode"]);
                Ok("started".into())
            }
            _ => Err("unknown task".into()),
        },
        "quit" => {
            let allowed = ["discord", "Discord", "firefox", "code", "steam", "spotify", "slack", "chrome", "electron"];
            if !allowed.contains(&arg) {
                return Err("unknown app".into());
            }
            sys::run(&["pkill", "-TERM", "-x", arg]);
            std::thread::sleep(std::time::Duration::from_secs(3));
            if sys::run(&["pgrep", "-x", arg]).0 == 0 {
                sys::run(&["pkill", "-KILL", "-x", arg]);
                return Ok(format!("{arg} didn't close in 3 s: killed"));
            }
            Ok(format!("{arg} closed"))
        }
        "portal" => {
            sys::run(&["systemctl", "--user", "restart", "xdg-desktop-portal.service", "xdg-desktop-portal-gnome.service", "xdg-desktop-portal-gtk.service"]);
            Ok("portals restarted".into())
        }
        "pipewire" => {
            sys::run(&["systemctl", "--user", "restart", "pipewire.service", "pipewire-pulse.service", "wireplumber.service"]);
            Ok("PipeWire restarted".into())
        }
        _ => Err("unknown task".into()),
    }
}

// ── after updates: triage ───────────────────────────────────────────────────────────────────────

fn triage_state() -> PathBuf {
    st::local_state().join("fix-triage.state")
}

/// What changes when the system or Flatpaks are updated.
fn package_fingerprint() -> String {
    let marks = ["/var/lib/pacman/local", "/var/lib/rpm/rpmdb.sqlite", "/usr/lib/sysimage/rpm/rpmdb.sqlite", "/var/lib/rpm/Packages", "/var/lib/flatpak/repo/refs", &home().join(".local/share/flatpak/repo/refs").to_string_lossy()]
        .iter()
        .map(|p| mtime(Path::new(p)).to_string())
        .collect::<Vec<_>>();
    marks.join(" ")
}

/// Versions of the apps whose caches go stale across updates
fn app_versions() -> Vec<(String, String)> {
    let names: &[(&str, &[&str])] = &[("discord", &["discord"]), ("firefox", &["firefox"]), ("code", &["code", "visual-studio-code-bin"]), ("chrome", &["google-chrome", "google-chrome-stable"]), ("chromium", &["chromium"]), ("slack", &["slack", "slack-desktop"]), ("obsidian", &["obsidian"]), ("spotify", &["spotify", "spotify-client"])];
    names.iter().filter_map(|(k, pk)| crate::distro::installed_version(pk).map(|(_, v)| (k.to_string(), v))).collect()
}

pub struct Triage {
    pub updated: bool,
    /// app → (old, new) version
    pub changed: Vec<(String, String, String)>,
    pub issues: Vec<(&'static Fix, String)>,
    pub tasks: Vec<(String, String)>,
}

pub fn triage(record: bool) -> Triage {
    let old = std::fs::read_to_string(triage_state()).unwrap_or_default();
    let fp = package_fingerprint();
    let updated = !old.lines().any(|l| l == format!("fp={fp}"));
    let vers = app_versions();
    let mut changed = vec![];
    for (k, v) in &vers {
        if let Some(o) = old.lines().find_map(|l| l.strip_prefix(&format!("v:{k}="))) {
            if o != v {
                changed.push((k.clone(), o.to_string(), v.clone()));
            }
        }
    }
    let issues: Vec<(&'static Fix, String)> = FIXES.iter().filter_map(|f| match detect(f.id) {
        State::Issue(d) => Some((f, d)),
        _ => None,
    }).collect();
    // caches that go stale when these apps change version
    let mut tasks = vec![];
    for (k, _, _) in &changed {
        match k.as_str() {
            "discord" => tasks.extend(discord_dirs().iter().map(|d| (format!("cache:{}", d.file_name().unwrap_or_default().to_string_lossy()), "Discord was updated: clear its caches (prevents a white/black window)".to_string()))),
            "firefox" => tasks.push(("ff:startup-cache".into(), "Firefox was updated: clear the startup cache".into())),
            "code" => tasks.push(("cache:.config/Code".into(), "VS Code was updated: clear its GPU caches".into())),
            "chrome" => tasks.push(("cache:.config/google-chrome".into(), "Chrome was updated: clear its GPU caches".into())),
            "chromium" => tasks.push(("cache:.config/chromium".into(), "Chromium was updated: clear its GPU caches".into())),
            _ => {}
        }
    }
    if record {
        let mut t = format!("fp={fp}\nat={}\n", sys::now());
        for (k, v) in &vers {
            t.push_str(&format!("v:{k}={v}\n"));
        }
        let _ = std::fs::create_dir_all(st::local_state());
        let _ = std::fs::write(triage_state(), t);
    }
    Triage { updated, changed, issues, tasks }
}

// ── console ─────────────────────────────────────────────────────────────────────────────────────

const R: &str = "\x1b[0m";
const AMB: &str = "\x1b[38;2;255;176;0m";
const CYN: &str = "\x1b[38;2;0;229;255m";
const DIM: &str = "\x1b[38;2;138;132;116m";
const RED: &str = "\x1b[38;2;255;42;0m";
const GRN: &str = "\x1b[38;2;124;217;90m";

fn ask(q: &str) -> Option<String> {
    use std::io::Write;
    print!("{q}");
    let _ = std::io::stdout().flush();
    let mut l = String::new();
    match std::io::stdin().read_line(&mut l) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(l.trim().to_string()),
    }
}

/// Apply a fix, through sudo when it needs root (the password is asked for there).
fn apply_any(id: &str, root: bool) -> Result<String, String> {
    if root && !sys::is_root() {
        let me = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| "shadow".into());
        let ok = std::process::Command::new("sudo").args(["-p", &format!("{AMB}[shadow fix]{R} administrator password: "), &me, "fix", "apply", id]).status().map(|s| s.success()).unwrap_or(false);
        return if ok { Ok("done".into()) } else { Err("not applied (wrong password, not an administrator, or see above)".into()) };
    }
    apply(id)
}

fn print_scan(porcelain: bool) -> usize {
    let mut n = 0;
    for f in FIXES {
        let s = detect(f.id);
        if porcelain {
            match &s {
                State::NotHere => {}
                State::Ok(d) => println!("ok {} {} {} {}|{}", f.id, f.root as u8, f.app, f.title, d),
                State::Issue(d) => println!("issue {} {} {} {}|{}", f.id, f.root as u8, f.app, f.title, d),
            }
            n += matches!(s, State::Issue(_)) as usize;
            continue;
        }
        match s {
            State::NotHere => {}
            State::Ok(d) => println!("  {GRN}✓{R} {}  {DIM}{d}{R}", f.title),
            State::Issue(d) => {
                n += 1;
                println!("  {AMB}!{R} {}\n      {DIM}{d}{R}  → {CYN}shadow fix apply {}{R}{}", f.title, f.id, if f.root { " (administrator)" } else { "" });
            }
        }
    }
    n
}

fn update_console() -> i32 {
    if Path::new(crate::updater::MARK).exists() {
        eprintln!("a Shadow update is in progress or was interrupted — finish it first (Update app / sudo shadow update recover)");
        return 1;
    }
    println!("{AMB}╭─ ◢ SHADOW // UPDATE THEN TRIAGE ─╮{R}\n");
    println!("Updates first (you're asked for your password by the package manager), then a triage of what");
    println!("the updates changed. Nothing is repaired during the update itself.\n");
    let sys_ok = match crate::distro::family() {
        crate::distro::Family::Rpm => std::process::Command::new("sudo").args(["dnf", "upgrade", "--refresh"]).status(),
        _ => std::process::Command::new("sudo").args(["pacman", "-Syu"]).status(),
    }
    .map(|s| s.success())
    .unwrap_or(false);
    println!("\n{}", if sys_ok { format!("{GRN}✓ system packages updated{R}") } else { format!("{AMB}! the system update did not complete (see above){R}") });
    if have("flatpak") {
        let ok = std::process::Command::new("flatpak").args(["update", "-y"]).status().map(|s| s.success()).unwrap_or(false);
        println!("{}", if ok { format!("{GRN}✓ Flatpaks updated{R}") } else { format!("{AMB}! flatpak update did not complete{R}") });
    }
    println!("\n{AMB}Triage{R}");
    triage_console()
}

fn triage_console() -> i32 {
    let t = triage(true);
    if !t.changed.is_empty() {
        println!("  updated apps:");
        for (k, o, n) in &t.changed {
            println!("    {k}: {DIM}{o}{R} → {n}");
        }
    }
    let mut items: Vec<(String, String, bool, bool)> = vec![]; // (id, label, root, is_task)
    for (f, d) in &t.issues {
        items.push((f.id.to_string(), format!("{} — {d}", f.title), f.root, false));
    }
    for (id, l) in &t.tasks {
        items.push((id.clone(), l.clone(), false, true));
    }
    if items.is_empty() {
        println!("  {GRN}✓ nothing needs fixing{R}");
        return 0;
    }
    for (i, (_, l, root, _)) in items.iter().enumerate() {
        println!("  {CYN}[{}]{R} {l}{}", i + 1, if *root { format!(" {DIM}(administrator){R}") } else { String::new() });
    }
    let a = ask(&format!("\n  fix: {CYN}a{R} all · {CYN}u{R} all that don't need a password · numbers (1 3) · {CYN}Enter{R} skip › ")).unwrap_or_default();
    let pick: Vec<usize> = match a.as_str() {
        "a" => (0..items.len()).collect(),
        "u" => (0..items.len()).filter(|i| !items[*i].2).collect(),
        s => s.split_whitespace().filter_map(|x| x.parse::<usize>().ok()).filter(|n| *n >= 1 && *n <= items.len()).map(|n| n - 1).collect(),
    };
    for i in pick {
        let (id, l, root, task) = &items[i];
        let r = if *task { run_task(id) } else { apply_any(id, *root) };
        match r {
            Ok(m) => println!("  {GRN}✓{R} {l}\n      {DIM}{m}{R}"),
            Err(e) => println!("  {RED}✗{R} {l}\n      {e}"),
        }
    }
    0
}

pub fn main(args: &[&str]) -> i32 {
    let porcelain = args.contains(&"--porcelain");
    match args {
        ["scan", ..] => {
            if !porcelain {
                println!("{AMB}╭─ ◢ SHADOW // FIX CENTER ─╮{R}\n");
            }
            let n = print_scan(porcelain);
            if !porcelain {
                println!("\n  {}", if n == 0 { format!("{GRN}nothing to fix{R}") } else { format!("{n} thing(s) to fix · shadow fix apply ID · tasks: shadow fix tasks") });
            }
            0
        }
        ["apply", "all-user", ..] => {
            let mut bad = 0;
            for f in FIXES.iter().filter(|f| !f.root) {
                if let State::Issue(_) = detect(f.id) {
                    match apply(f.id) {
                        Ok(m) => println!("✓ {}: {m}", f.id),
                        Err(e) => {
                            bad += 1;
                            println!("✗ {}: {e}", f.id)
                        }
                    }
                }
            }
            (bad > 0) as i32
        }
        ["apply", id, ..] => match FIXES.iter().find(|f| f.id == *id) {
            Some(f) => match apply_any(id, f.root) {
                Ok(m) => {
                    println!("✓ {m}");
                    0
                }
                Err(e) => {
                    eprintln!("✗ {e}");
                    1
                }
            },
            None => {
                eprintln!("unknown fix '{id}'");
                2
            }
        },
        ["tasks", ..] => {
            for (id, l) in tasks() {
                if porcelain { println!("{id}|{l}") } else { println!("  {CYN}{id:<28}{R} {l}") }
            }
            0
        }
        ["run", id, ..] => match run_task(id) {
            Ok(m) => {
                println!("✓ {m}");
                0
            }
            Err(e) => {
                eprintln!("✗ {e}");
                1
            }
        },
        ["triage", ..] if porcelain || args.contains(&"--quiet") || args.contains(&"--notify") => {
            // the session's after-login check: only when the packages changed since the last triage
            let t = triage(!args.contains(&"--dry-run"));
            let n = t.issues.len() + t.tasks.len();
            if porcelain {
                println!("updated {}", t.updated as u8);
                for (k, o, nv) in &t.changed {
                    println!("changed {k} {o} {nv}");
                }
                for (f, d) in &t.issues {
                    println!("issue {} {} {} {}|{d}", f.id, f.root as u8, f.app, f.title);
                }
                for (id, l) in &t.tasks {
                    println!("task {id}|{l}");
                }
            }
            if args.contains(&"--notify") && t.updated && n > 0 {
                st::notify("Updates changed your apps", &format!("{n} thing(s) to fix — open the Fix center (Mod+Shift+H)"), "normal");
            }
            0
        }
        ["triage", ..] => triage_console(),
        ["update", ..] => update_console(),
        _ => {
            eprintln!("usage: shadow fix scan | apply ID|all-user | tasks | run TASK | triage [--porcelain|--notify] | update");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_edits_keep_the_rest() {
        assert_eq!(json_set("{\n  \"a\": 1\n}", "SKIP_HOST_UPDATE", "true"), "{\n  \"SKIP_HOST_UPDATE\": true,\n  \"a\": 1\n}");
        assert_eq!(json_set("{\"MINIMIZE_TO_TRAY\": true, \"x\": 2}", "MINIMIZE_TO_TRAY", "false"), "{\"MINIMIZE_TO_TRAY\": false, \"x\": 2}");
        assert_eq!(json_set("", "k", "1"), "{\n  \"k\": 1\n}\n");
        assert_eq!(json_set("{\"IS_MAXIMIZED\": true}", "k", "1"), "{\n  \"k\": 1,\n  \"IS_MAXIMIZED\": true}");
        let jsonc = "// comment\n{\n\t\"enable-crash-reporter\": true\n}";
        let o = json_set(jsonc, "password-store", "\"gnome-libsecret\"");
        assert!(o.starts_with("// comment\n{\n  \"password-store\": \"gnome-libsecret\","), "{o}");
        assert_eq!(json_get(&o, "password-store"), Some("\"gnome-libsecret\""));
        assert_eq!(json_set("{}", "k", "1"), "{\n  \"k\": 1}");
    }

    #[test]
    fn user_js() {
        let t = user_pref_set("user_pref(\"a\", 1);\n", "media.ffmpeg.vaapi.enabled", "true");
        assert!(t.contains("user_pref(\"a\", 1);") && t.contains("user_pref(\"media.ffmpeg.vaapi.enabled\", true);"));
        let t2 = user_pref_set(&t, "media.ffmpeg.vaapi.enabled", "false");
        assert_eq!(t2.matches("vaapi").count(), 1);
    }

    #[test]
    fn launcher_exec_rewrite() {
        assert_eq!(rewrite_exec("/usr/bin/discord %U", "--x", ""), "/usr/bin/discord --x %U");
        assert_eq!(rewrite_exec("/usr/bin/steam %U", "", "STEAM_FORCE_DESKTOPUI_SCALING=1.5"), "env STEAM_FORCE_DESKTOPUI_SCALING=1.5 /usr/bin/steam %U");
        assert_eq!(rewrite_exec("/usr/bin/flatpak run --branch=stable --arch=x86_64 --command=discord com.discordapp.Discord", "--x", ""), "/usr/bin/flatpak run --branch=stable --arch=x86_64 --command=discord com.discordapp.Discord --x");
    }
}
